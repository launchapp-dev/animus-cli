//! Application-facing durable idempotency helpers for `chat send`.

use std::collections::BTreeMap;

use animus_actor::Actor;
use anyhow::{anyhow, Context, Result};
use orchestrator_core::{
    ChatOperationBegin, ChatOperationClaim, ChatOperationReceipt, ChatOperationStatus, ChatOperationStore,
    DEFAULT_CHAT_OPERATION_LEASE_SECS,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::client::{ConversationStoreClient, SharedOperationClient};

#[derive(Clone)]
pub(crate) enum ChatOperationAuthority {
    Local(ChatOperationStore),
    Shared(SharedOperationClient),
}

impl ChatOperationAuthority {
    fn renew(&self, claim: &ChatOperationClaim) -> Result<bool> {
        match self {
            Self::Local(store) => store.renew(claim),
            Self::Shared(store) => store.renew(claim),
        }
    }

    fn bind_execution_hash(&self, claim: &mut ChatOperationClaim, hash: &str, allow_rebind: bool) -> Result<bool> {
        match self {
            Self::Local(store) if allow_rebind => store.rebind_recovered_execution_hash(claim, hash),
            Self::Local(store) => store.bind_execution_hash(claim, hash),
            Self::Shared(store) => store.bind_execution(claim, hash, allow_rebind),
        }
    }

    fn release_pending(&self, claim: &ChatOperationClaim) -> Result<bool> {
        match self {
            Self::Local(store) => store.release_pending(claim),
            Self::Shared(store) => store.release(claim),
        }
    }

    fn mark_user_accepted(&self, claim: &mut ChatOperationClaim, seq: u64) -> Result<bool> {
        match self {
            Self::Local(store) => store.mark_user_accepted(claim, seq),
            Self::Shared(store) => store.accept_user(claim, seq),
        }
    }

    fn finish(
        &self,
        claim: &ChatOperationClaim,
        status: orchestrator_core::ChatOperationStatus,
        assistant_seq: Option<u64>,
        code: Option<&str>,
        message: Option<&str>,
    ) -> Result<bool> {
        match self {
            Self::Local(store) => match status {
                orchestrator_core::ChatOperationStatus::Completed => store.complete(
                    claim,
                    assistant_seq.ok_or_else(|| anyhow!("completed operation is missing assistant sequence"))?,
                ),
                orchestrator_core::ChatOperationStatus::AssistantFailed => {
                    store.fail(claim, code.unwrap_or("assistant_failed"), message.unwrap_or("assistant failed"))
                }
                orchestrator_core::ChatOperationStatus::AssistantInterrupted => {
                    store.interrupt(claim, message.unwrap_or("assistant interrupted"))
                }
                _ => Err(anyhow!("operation terminalization requires a terminal status")),
            },
            Self::Shared(store) => store.terminalize(claim, status, assistant_seq, code, message),
        }
    }

    fn receipt(&self, claim: &ChatOperationClaim) -> Result<ChatOperationReceipt> {
        match self {
            Self::Local(store) => store.receipt(claim),
            Self::Shared(store) => store.receipt(claim),
        }
    }
}

pub(crate) struct ChatTurnOperation {
    authority: ChatOperationAuthority,
    claim: Box<ChatOperationClaim>,
    heartbeat: Option<LeaseHeartbeat>,
    #[cfg(test)]
    fail_after_user_accept_once: bool,
}

pub(crate) enum ExecutionHashBinding {
    Bound,
    Drifted,
}

impl ChatTurnOperation {
    pub(crate) fn new(authority: ChatOperationAuthority, claim: Box<ChatOperationClaim>) -> Self {
        Self {
            authority,
            claim,
            heartbeat: None,
            #[cfg(test)]
            fail_after_user_accept_once: false,
        }
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

    pub(crate) fn bind_execution_hash(&mut self, execution_hash: &str) -> Result<ExecutionHashBinding> {
        if self.claim.execution_hash.as_deref().is_some_and(|stored| stored != execution_hash) {
            return Ok(ExecutionHashBinding::Drifted);
        }
        if !self.authority.bind_execution_hash(&mut self.claim, execution_hash, false)? {
            return Err(crate::conflict_error(
                "idempotency_in_progress: chat operation authority moved before execution was bound",
            ));
        }
        Ok(ExecutionHashBinding::Bound)
    }

    pub(crate) fn rebind_recovered_execution_hash(&mut self, execution_hash: &str) -> Result<()> {
        if !self.authority.bind_execution_hash(&mut self.claim, execution_hash, true)? {
            return Err(crate::conflict_error(
                "idempotency_in_progress: recovered chat operation could not rebind execution authority",
            ));
        }
        Ok(())
    }

    pub(crate) fn renew_authority(&self) -> Result<bool> {
        self.authority.renew(&self.claim)
    }

    pub(crate) fn receipt(&self) -> Result<ChatOperationReceipt> {
        self.authority.receipt(&self.claim)
    }

    pub(crate) fn reconcile_recovered_accepted(&self, assistant_seq: Option<u64>) -> Result<ChatOperationReceipt> {
        reconcile_recovered_accepted(&self.authority, &self.claim, assistant_seq)
    }

    pub(crate) fn release_pending(&self) -> Result<()> {
        if !self.authority.release_pending(&self.claim)? {
            return Err(crate::conflict_error(
                "idempotency_in_progress: pending chat operation authority moved before release",
            ));
        }
        Ok(())
    }

    pub(crate) fn interrupt_recovered_user(&mut self, user_seq: u64, message: &str) -> Result<ChatOperationReceipt> {
        self.reconcile_durable_user(user_seq, None, message)
    }

    /// Reconcile an error after the canonical user append. Acceptance RPCs can
    /// fail ambiguously after the backend commits, so a false/error acceptance
    /// result does not stop terminalization: the lease-fenced terminal RPC is
    /// the authoritative second check. A durable assistant row wins and is
    /// completed; otherwise the accepted user becomes interrupted.
    pub(crate) fn reconcile_durable_user(
        &mut self,
        user_seq: u64,
        assistant_seq: Option<u64>,
        interruption_message: &str,
    ) -> Result<ChatOperationReceipt> {
        let mut acceptance_error = None;
        if self.claim.status == orchestrator_core::ChatOperationStatus::Pending {
            match self.authority.mark_user_accepted(&mut self.claim, user_seq) {
                Ok(true) => {}
                Ok(false) => {
                    acceptance_error = Some("chat operation authority moved before user reconciliation".to_string());
                }
                Err(error) => {
                    acceptance_error = Some(format!("user acceptance returned an ambiguous error: {error:#}"));
                }
            }
        } else if self.claim.user_seq != Some(user_seq) {
            return Err(crate::conflict_error(
                "idempotency_conflict: recovered chat operation has a different canonical user sequence",
            ));
        }

        let (status, code, message) = match assistant_seq {
            Some(_) => (orchestrator_core::ChatOperationStatus::Completed, None, None),
            None => (
                orchestrator_core::ChatOperationStatus::AssistantInterrupted,
                Some("assistant_interrupted"),
                Some(interruption_message),
            ),
        };
        if !self.authority.finish(&self.claim, status, assistant_seq, code, message)? {
            let receipt = self.authority.receipt(&self.claim).with_context(|| {
                acceptance_error
                    .unwrap_or_else(|| "chat operation lost authority before durable-user terminalization".to_string())
            })?;
            if receipt.status.is_terminal() {
                self.claim.status = receipt.status;
                return Ok(receipt);
            }
            return Err(crate::conflict_error(
                "idempotency_in_progress: recovered chat operation lost authority before interruption",
            ));
        }
        let receipt = self.authority.receipt(&self.claim)?;
        self.claim.status = receipt.status;
        self.claim.user_seq = receipt.user_seq;
        Ok(receipt)
    }

    pub(crate) fn mark_user_accepted(&mut self, seq: u64) -> Result<()> {
        if self.claim.status == orchestrator_core::ChatOperationStatus::Pending {
            if !self.authority.mark_user_accepted(&mut self.claim, seq)? {
                return Err(crate::conflict_error(
                    "idempotency_in_progress: chat operation authority moved before user acceptance",
                ));
            }
            #[cfg(test)]
            if std::mem::take(&mut self.fail_after_user_accept_once) {
                return Err(anyhow!("injected lost user-accept response"));
            }
        } else if self.claim.user_seq != Some(seq) {
            return Err(crate::conflict_error(
                "idempotency_conflict: recovered chat operation has a different canonical user sequence",
            ));
        }
        self.heartbeat = Some(LeaseHeartbeat::start(self.authority.clone(), self.claim.clone()));
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn inject_lost_user_accept_response(&mut self) {
        self.fail_after_user_accept_once = true;
    }

    pub(crate) fn finish_heartbeat(&mut self) -> Result<()> {
        if let Some(heartbeat) = self.heartbeat.take() {
            if !heartbeat.finish()? || !self.authority.renew(&self.claim)? {
                return Err(crate::conflict_error(
                    "idempotency_in_progress: chat operation authority moved during provider execution",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn complete(&mut self, assistant_seq: u64) -> Result<ChatOperationReceipt> {
        self.finish_heartbeat()?;
        if !self.authority.finish(
            &self.claim,
            orchestrator_core::ChatOperationStatus::Completed,
            Some(assistant_seq),
            None,
            None,
        )? {
            return Err(crate::conflict_error(
                "idempotency_in_progress: assistant was persisted but canonical chat receipt is reconciling",
            ));
        }
        self.authority.receipt(&self.claim)
    }

    pub(crate) fn fail(&mut self, code: &str, error: &str) -> Result<ChatOperationReceipt> {
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.finish();
        }
        let changed = self.authority.finish(
            &self.claim,
            orchestrator_core::ChatOperationStatus::AssistantFailed,
            None,
            Some(code),
            Some(error),
        )?;
        let receipt = self.authority.receipt(&self.claim)?;
        if !changed && !receipt.status.is_terminal() {
            return Err(crate::conflict_error(
                "idempotency_in_progress: chat operation authority moved before failure was recorded",
            ));
        }
        Ok(receipt)
    }
}

struct LeaseHeartbeat {
    stop: Option<std::sync::mpsc::Sender<()>>,
    join: Option<std::thread::JoinHandle<Result<bool>>>,
}

impl LeaseHeartbeat {
    fn start(authority: ChatOperationAuthority, claim: Box<ChatOperationClaim>) -> Self {
        let (stop, receiver) = std::sync::mpsc::channel();
        let db_window = claim
            .lease_expires_at
            .map(|expires| expires.saturating_sub(chrono::Utc::now().timestamp()))
            .unwrap_or(DEFAULT_CHAT_OPERATION_LEASE_SECS);
        // The expiry comes from the backend clock. Host skew can only make the
        // interval more conservative: clamp the upper bound to the v1 backend
        // renewal cadence and the lower bound to one second.
        let interval_secs = (db_window / 3).clamp(1, (DEFAULT_CHAT_OPERATION_LEASE_SECS / 3).max(1));
        let interval = std::time::Duration::from_secs(u64::try_from(interval_secs).unwrap_or(1));
        let join = std::thread::spawn(move || loop {
            match receiver.recv_timeout(interval) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(true),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if !authority.renew(&claim)? {
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
    conversation_store: &ConversationStoreClient,
    project_root: &std::path::Path,
    actor: &Actor,
    conversation_id: &str,
    caller_key: String,
    request_hash: String,
    require_shared: bool,
) -> Result<(ChatOperationAuthority, ChatOperationBegin)> {
    if actor.user_id.trim().is_empty() {
        return Err(crate::invalid_input_error("idempotent chat send requires a non-empty actor user_id"));
    }
    let workspace_id = actor
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| crate::invalid_input_error("idempotent chat send requires actor tenant_id (workspace)"))?;
    let project_scope = protocol::repository_scope_for_path(project_root);
    let request = orchestrator_core::ChatOperationRequest {
        project_scope,
        workspace_id: workspace_id.to_string(),
        actor_id: actor.user_id.clone(),
        conversation_id: conversation_id.to_string(),
        caller_key,
        request_hash,
    };
    match conversation_store.shared_operation_client(require_shared)? {
        Some(shared) => {
            let outcome = shared.begin(request)?;
            Ok((ChatOperationAuthority::Shared(shared), outcome))
        }
        None => {
            let local = ChatOperationStore::for_project(project_root)?;
            let outcome = local.begin(request)?;
            Ok((ChatOperationAuthority::Local(local), outcome))
        }
    }
}

pub(crate) fn reconcile_recovered_accepted(
    authority: &ChatOperationAuthority,
    claim: &ChatOperationClaim,
    assistant_seq: Option<u64>,
) -> Result<ChatOperationReceipt> {
    if let Some(seq) = assistant_seq {
        if !authority.finish(claim, ChatOperationStatus::Completed, Some(seq), None, None)? {
            return authority.receipt(claim);
        }
    } else if !authority.finish(
        claim,
        ChatOperationStatus::AssistantInterrupted,
        None,
        Some("assistant_interrupted"),
        Some("the prior process stopped after accepting the user message; provider execution is not repeated automatically"),
    )? {
        return authority.receipt(claim);
    }
    authority.receipt(claim)
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
