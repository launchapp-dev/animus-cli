//! Application-facing durable idempotency helpers for `chat send`.

use std::collections::BTreeMap;

use animus_actor::Actor;
use anyhow::{anyhow, Context, Result};
use orchestrator_core::{
    ChatOperationBegin, ChatOperationClaim, ChatOperationReceipt, ChatOperationStore, DEFAULT_CHAT_OPERATION_LEASE_SECS,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) struct ChatTurnOperation {
    store: ChatOperationStore,
    claim: Box<ChatOperationClaim>,
    heartbeat: Option<LeaseHeartbeat>,
}

impl ChatTurnOperation {
    pub(crate) fn new(store: ChatOperationStore, claim: Box<ChatOperationClaim>) -> Self {
        Self { store, claim, heartbeat: None }
    }

    pub(crate) fn claim(&self) -> &ChatOperationClaim {
        &self.claim
    }

    pub(crate) fn user_message_id(&self) -> &str {
        &self.claim.user_message_id
    }

    pub(crate) fn assistant_message_id(&self) -> &str {
        &self.claim.assistant_message_id
    }

    pub(crate) fn mark_user_accepted(&mut self, seq: u64) -> Result<()> {
        if self.claim.status == orchestrator_core::ChatOperationStatus::Pending {
            if !self.store.mark_user_accepted(&mut self.claim, seq)? {
                return Err(crate::conflict_error(
                    "idempotency_in_progress: chat operation authority moved before user acceptance",
                ));
            }
        } else if self.claim.user_seq != Some(seq) {
            return Err(crate::conflict_error(
                "idempotency_conflict: recovered chat operation has a different canonical user sequence",
            ));
        }
        self.heartbeat = Some(LeaseHeartbeat::start(self.store.clone(), self.claim.clone()));
        Ok(())
    }

    pub(crate) fn finish_heartbeat(&mut self) -> Result<()> {
        if let Some(heartbeat) = self.heartbeat.take() {
            if !heartbeat.finish()? || !self.store.renew(&self.claim)? {
                return Err(crate::conflict_error(
                    "idempotency_in_progress: chat operation authority moved during provider execution",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn complete(&mut self, assistant_seq: u64) -> Result<ChatOperationReceipt> {
        self.finish_heartbeat()?;
        if !self.store.complete(&self.claim, assistant_seq)? {
            return Err(crate::conflict_error(
                "idempotency_in_progress: assistant was persisted but canonical chat receipt is reconciling",
            ));
        }
        self.store.receipt(&self.claim)
    }

    pub(crate) fn fail(&mut self, code: &str, error: &str) -> Result<ChatOperationReceipt> {
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.finish();
        }
        if !self.store.fail(&self.claim, code, error)? {
            return self.store.receipt(&self.claim);
        }
        self.store.receipt(&self.claim)
    }
}

struct LeaseHeartbeat {
    stop: Option<std::sync::mpsc::Sender<()>>,
    join: Option<std::thread::JoinHandle<Result<bool>>>,
}

impl LeaseHeartbeat {
    fn start(store: ChatOperationStore, claim: Box<ChatOperationClaim>) -> Self {
        let (stop, receiver) = std::sync::mpsc::channel();
        let interval =
            std::time::Duration::from_secs(u64::try_from((DEFAULT_CHAT_OPERATION_LEASE_SECS / 3).max(1)).unwrap_or(1));
        let join = std::thread::spawn(move || loop {
            match receiver.recv_timeout(interval) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(true),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if !store.renew(&claim)? {
                        return Ok(false);
                    }
                }
            }
        });
        Self { stop: Some(stop), join: Some(join) }
    }

    fn finish(mut self) -> Result<bool> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.join
            .take()
            .expect("chat operation lease heartbeat join handle")
            .join()
            .map_err(|_| anyhow!("chat operation lease heartbeat panicked"))?
    }
}

impl Drop for LeaseHeartbeat {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        Value::Object(values) => {
            let sorted: BTreeMap<_, _> = values.into_iter().map(|(key, value)| (key, canonical_json(value))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        scalar => scalar,
    }
}

pub(crate) fn effective_request_hash(value: Value) -> Result<String> {
    let encoded = serde_json::to_vec(&canonical_json(value)).context("encoding effective chat operation request")?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

pub(crate) fn begin(
    project_root: &std::path::Path,
    actor: &Actor,
    conversation_id: &str,
    caller_key: String,
    request_hash: String,
) -> Result<(ChatOperationStore, ChatOperationBegin)> {
    if actor.user_id.trim().is_empty() {
        return Err(crate::invalid_input_error("idempotent chat send requires a non-empty actor user_id"));
    }
    let workspace_id = actor
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| crate::invalid_input_error("idempotent chat send requires actor tenant_id (workspace)"))?;
    let store = ChatOperationStore::for_project(project_root)?;
    let request = store.request(workspace_id, actor.user_id.clone(), conversation_id, caller_key, request_hash);
    let outcome = store.begin(request)?;
    Ok((store, outcome))
}

pub(crate) fn reconcile_recovered_accepted(
    store: &ChatOperationStore,
    claim: &ChatOperationClaim,
    assistant_seq: Option<u64>,
) -> Result<ChatOperationReceipt> {
    if let Some(seq) = assistant_seq {
        if !store.complete(claim, seq)? {
            return store.receipt(claim);
        }
    } else if !store.interrupt(
        claim,
        "the prior process stopped after accepting the user message; provider execution is not repeated automatically",
    )? {
        return store.receipt(claim);
    }
    store.receipt(claim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_hash_is_object_order_independent_and_effect_sensitive() {
        let first = effective_request_hash(serde_json::json!({"b": 2, "a": {"y": 1, "x": 0}})).unwrap();
        let reordered = effective_request_hash(serde_json::json!({"a": {"x": 0, "y": 1}, "b": 2})).unwrap();
        let changed = effective_request_hash(serde_json::json!({"a": {"x": 9, "y": 1}, "b": 2})).unwrap();
        assert_eq!(first, reordered);
        assert_ne!(first, changed);
    }
}
