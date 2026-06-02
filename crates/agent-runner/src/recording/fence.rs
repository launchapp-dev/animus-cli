//! Idempotency-key fence integration for the recording layer.
//!
//! Couples the decision-log ([`super::Recorder`]) to a durable-store fence so
//! that a retry inside a durable workflow consumes the prior outcome instead
//! of re-calling the model. The fence is the load-bearing guard that turns
//! the recording layer from "audit trail" into "deterministic retry":
//!
//! ```text
//!   ┌──────────── retry path ────────────┐
//!   │                                    │
//!   ▼                                    │
//! step_query(key) ─────► PriorSuccess ──► serve recorded response from
//!                                          decisions.jsonl  (no model call)
//!                  ─────► PriorError  ──► surface recorded error envelope
//!                                          (no model call)
//!                  ─────► InProgress ───► another worker is calling the
//!                                          model now: wait via [`wait_for_completion`]
//!                  ─────► Absent ───────► step_begin(key) → call the model →
//!                                          step_complete(reservation, response)
//! ```
//!
//! Idempotency key shape: `<repo_scope>:<workflow_id>:<phase_id>:<model_call_index>`.
//!
//! Production wiring gap: ao-cli does not yet ship a concrete client to the
//! `launchapp-dev/animus-step-durable-dbos` plugin. The fence integration is
//! defined here against the [`DurableStoreClient`] trait and exercised
//! end-to-end via [`MockDurableStore`]. The DBOS plugin's RPC surface plugs
//! in by implementing [`DurableStoreClient`] in a follow-up.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable key shape for `step_query` / `step_begin` against the durable
/// store. Format: `<repo_scope>:<workflow_id>:<phase_id>:<call_index>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdempotencyKey(pub String);

impl IdempotencyKey {
    pub fn new(repo_scope: &str, workflow_id: &str, phase_id: &str, model_call_index: u32) -> Self {
        Self(format!("{repo_scope}:{workflow_id}:{phase_id}:{model_call_index}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Snapshot of a durable step's fence state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceState {
    /// No prior reservation. Caller proceeds via [`DurableStoreClient::step_begin`].
    Absent,
    /// Another worker has an active reservation for this key. Caller should
    /// wait via [`wait_for_completion`] rather than racing.
    InProgress { reservation_id: String },
    /// Prior completion succeeded; replay the captured response.
    PriorSuccess { response: Value },
    /// Prior completion failed; surface the captured error.
    PriorError { message: String },
}

/// Trait the fence integration is wired against. Production implementations
/// bridge to the DBOS plugin's RPC surface; tests use [`MockDurableStore`].
///
/// All methods are async to permit network-backed implementations; the
/// mock implementation completes synchronously.
#[async_trait::async_trait]
pub trait DurableStoreClient: Send + Sync {
    /// Look up the current fence state for `key`.
    async fn step_query(&self, key: &IdempotencyKey) -> Result<FenceState>;

    /// Reserve `key` for this worker. Returns the reservation id which must
    /// be supplied to [`Self::step_complete`] / [`Self::step_fail`]. Errors
    /// if a concurrent reservation already exists (caller must re-query).
    async fn step_begin(&self, key: &IdempotencyKey) -> Result<String>;

    /// Commit a successful response for `reservation_id`.
    async fn step_complete(&self, reservation_id: &str, response: Value) -> Result<()>;

    /// Commit a terminal error for `reservation_id`. The error is permanent:
    /// PRIOR_ERROR is the terminal fence state for the matching key.
    async fn step_fail(&self, reservation_id: &str, error: &str) -> Result<()>;
}

/// Outcome of [`run_with_fence`]. Either the prior recorded outcome was
/// served (no model call), or a fresh model call ran and committed.
#[derive(Debug)]
pub enum FenceOutcome {
    Served(Value),
    PriorFailure(String),
    Committed(Value),
}

/// Drive a model call through the fence: query state, replay-on-prior or
/// reserve + call + commit. The `call_model` closure is invoked only when
/// the fence reports `Absent` after a clean reservation.
pub async fn run_with_fence<C, F, Fut>(
    client: &C,
    key: &IdempotencyKey,
    poll_interval: Duration,
    max_wait: Duration,
    call_model: F,
) -> Result<FenceOutcome>
where
    C: DurableStoreClient + ?Sized,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Value, anyhow::Error>>,
{
    match client.step_query(key).await? {
        FenceState::PriorSuccess { response } => return Ok(FenceOutcome::Served(response)),
        FenceState::PriorError { message } => return Ok(FenceOutcome::PriorFailure(message)),
        FenceState::InProgress { .. } => {
            let resolved = wait_for_completion(client, key, poll_interval, max_wait).await?;
            return Ok(resolved);
        }
        FenceState::Absent => {}
    }
    // Codex round-1 P2: if two workers raced through step_query observing
    // Absent, the loser's step_begin will collide with the winner's
    // reservation. Re-query and wait for completion rather than surfacing
    // the bookkeeping collision as a fence error — the whole point of the
    // fence is to make the loser serve the winner's outcome.
    let reservation_id = match client.step_begin(key).await {
        Ok(id) => id,
        Err(_first_err) => match client.step_query(key).await? {
            FenceState::PriorSuccess { response } => return Ok(FenceOutcome::Served(response)),
            FenceState::PriorError { message } => return Ok(FenceOutcome::PriorFailure(message)),
            FenceState::InProgress { .. } => {
                return wait_for_completion(client, key, poll_interval, max_wait).await;
            }
            FenceState::Absent => return Err(_first_err),
        },
    };
    let response = match call_model().await {
        Ok(v) => v,
        Err(err) => {
            let msg = err.to_string();
            // Best-effort: if marking the failure fails too, surface the
            // ORIGINAL model error rather than the bookkeeping error.
            let _ = client.step_fail(&reservation_id, &msg).await;
            return Err(err);
        }
    };
    client.step_complete(&reservation_id, response.clone()).await?;
    Ok(FenceOutcome::Committed(response))
}

/// Poll `step_query` until the InProgress state resolves. Used by retries
/// that lose the race to acquire the reservation.
pub async fn wait_for_completion<C: DurableStoreClient + ?Sized>(
    client: &C,
    key: &IdempotencyKey,
    poll_interval: Duration,
    max_wait: Duration,
) -> Result<FenceOutcome> {
    let start = std::time::Instant::now();
    loop {
        match client.step_query(key).await? {
            FenceState::PriorSuccess { response } => return Ok(FenceOutcome::Served(response)),
            FenceState::PriorError { message } => return Ok(FenceOutcome::PriorFailure(message)),
            FenceState::Absent => {
                anyhow::bail!("fence transitioned from InProgress to Absent for key {} (lost reservation)", key);
            }
            FenceState::InProgress { .. } => {
                if start.elapsed() >= max_wait {
                    anyhow::bail!("fence wait_for_completion exceeded {:?} for key {}", max_wait, key);
                }
                tokio::time::sleep(poll_interval).await;
            }
        }
    }
}

/// In-memory mock for tests and the test harness.
pub struct MockDurableStore {
    state: tokio::sync::Mutex<std::collections::HashMap<String, MockEntry>>,
    counter: std::sync::atomic::AtomicU64,
    /// Diagnostic counter: how many times `step_begin` succeeded. Used by
    /// tests to assert "the model was called exactly N times".
    pub begins: std::sync::atomic::AtomicU64,
}

#[derive(Debug, Clone)]
enum MockEntry {
    InProgress { reservation_id: String },
    Success { response: Value },
    Error { message: String },
}

impl Default for MockDurableStore {
    fn default() -> Self {
        Self {
            state: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            counter: std::sync::atomic::AtomicU64::new(0),
            begins: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl MockDurableStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn force_success(&self, key: &IdempotencyKey, response: Value) {
        let mut guard = self.state.lock().await;
        guard.insert(key.0.clone(), MockEntry::Success { response });
    }

    pub async fn force_error(&self, key: &IdempotencyKey, message: &str) {
        let mut guard = self.state.lock().await;
        guard.insert(key.0.clone(), MockEntry::Error { message: message.to_string() });
    }

    pub async fn force_in_progress(&self, key: &IdempotencyKey) -> String {
        let mut guard = self.state.lock().await;
        let id = format!("res-{}", self.counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        guard.insert(key.0.clone(), MockEntry::InProgress { reservation_id: id.clone() });
        id
    }
}

#[async_trait::async_trait]
impl DurableStoreClient for MockDurableStore {
    async fn step_query(&self, key: &IdempotencyKey) -> Result<FenceState> {
        let guard = self.state.lock().await;
        Ok(match guard.get(&key.0) {
            None => FenceState::Absent,
            Some(MockEntry::InProgress { reservation_id }) => {
                FenceState::InProgress { reservation_id: reservation_id.clone() }
            }
            Some(MockEntry::Success { response }) => FenceState::PriorSuccess { response: response.clone() },
            Some(MockEntry::Error { message }) => FenceState::PriorError { message: message.clone() },
        })
    }

    async fn step_begin(&self, key: &IdempotencyKey) -> Result<String> {
        let mut guard = self.state.lock().await;
        if let Some(existing) = guard.get(&key.0) {
            anyhow::bail!("step_begin on already-present key {}: {:?}", key, existing);
        }
        let id = format!("res-{}", self.counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        guard.insert(key.0.clone(), MockEntry::InProgress { reservation_id: id.clone() });
        self.begins.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(id)
    }

    async fn step_complete(&self, reservation_id: &str, response: Value) -> Result<()> {
        let mut guard = self.state.lock().await;
        let Some((key, entry)) = guard
            .iter()
            .find(|(_, v)| matches!(v, MockEntry::InProgress { reservation_id: rid } if rid == reservation_id))
            .map(|(k, _)| (k.clone(), ()))
        else {
            anyhow::bail!("step_complete on unknown reservation {}", reservation_id);
        };
        let _ = entry;
        guard.insert(key, MockEntry::Success { response });
        Ok(())
    }

    async fn step_fail(&self, reservation_id: &str, error: &str) -> Result<()> {
        let mut guard = self.state.lock().await;
        let Some(key) = guard
            .iter()
            .find(|(_, v)| matches!(v, MockEntry::InProgress { reservation_id: rid } if rid == reservation_id))
            .map(|(k, _)| k.clone())
        else {
            anyhow::bail!("step_fail on unknown reservation {}", reservation_id);
        };
        guard.insert(key, MockEntry::Error { message: error.to_string() });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn fresh_key_runs_model_and_commits_success() {
        let store = MockDurableStore::new();
        let key = IdempotencyKey::new("scope", "wf-1", "impl", 0);
        let outcome =
            run_with_fence(store.as_ref(), &key, Duration::from_millis(5), Duration::from_secs(1), || async {
                Ok(serde_json::json!({"ok": true, "text": "hello"}))
            })
            .await
            .expect("first call commits");
        match outcome {
            FenceOutcome::Committed(v) => assert_eq!(v["text"], "hello"),
            other => panic!("expected Committed, got {:?}", other),
        }
        assert_eq!(store.begins.load(Ordering::SeqCst), 1);

        // A second call sees PriorSuccess and serves the recorded response
        // WITHOUT invoking the closure.
        let called_again = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_for_closure = called_again.clone();
        let outcome2 =
            run_with_fence(store.as_ref(), &key, Duration::from_millis(5), Duration::from_secs(1), || async move {
                called_for_closure.store(true, Ordering::SeqCst);
                Ok(serde_json::json!({"should_not_run": true}))
            })
            .await
            .expect("replay serves prior");
        match outcome2 {
            FenceOutcome::Served(v) => assert_eq!(v["text"], "hello"),
            other => panic!("expected Served, got {:?}", other),
        }
        assert!(!called_again.load(Ordering::SeqCst), "model must not be re-invoked on PriorSuccess");
        assert_eq!(store.begins.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn prior_error_state_is_terminal() {
        let store = MockDurableStore::new();
        let key = IdempotencyKey::new("scope", "wf-2", "impl", 0);
        store.force_error(&key, "deterministic blow-up").await;
        let outcome =
            run_with_fence(store.as_ref(), &key, Duration::from_millis(5), Duration::from_secs(1), || async {
                panic!("model must not be invoked for PriorError")
            })
            .await
            .expect("prior error must be surfaced, not raised");
        match outcome {
            FenceOutcome::PriorFailure(msg) => assert_eq!(msg, "deterministic blow-up"),
            other => panic!("expected PriorFailure, got {:?}", other),
        }
        assert_eq!(store.begins.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn in_progress_waits_for_completion_then_serves() {
        let store = MockDurableStore::new();
        let key = IdempotencyKey::new("scope", "wf-3", "impl", 0);
        let reservation = store.force_in_progress(&key).await;

        let store_for_completer = store.clone();
        let completer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            store_for_completer
                .step_complete(&reservation, serde_json::json!({"text": "won-the-race"}))
                .await
                .expect("complete");
        });

        let outcome =
            run_with_fence(store.as_ref(), &key, Duration::from_millis(10), Duration::from_secs(2), || async {
                panic!("model must not be invoked while another worker holds the reservation")
            })
            .await
            .expect("wait + serve");
        completer.await.unwrap();
        match outcome {
            FenceOutcome::Served(v) => assert_eq!(v["text"], "won-the-race"),
            other => panic!("expected Served, got {:?}", other),
        }
        assert_eq!(store.begins.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn model_failure_marks_prior_error() {
        let store = MockDurableStore::new();
        let key = IdempotencyKey::new("scope", "wf-4", "impl", 0);
        let result = run_with_fence(store.as_ref(), &key, Duration::from_millis(5), Duration::from_secs(1), || async {
            Err::<Value, _>(anyhow::anyhow!("upstream rejected"))
        })
        .await;
        assert!(result.is_err());
        match store.step_query(&key).await.expect("query") {
            FenceState::PriorError { message } => assert!(message.contains("upstream rejected")),
            other => panic!("expected PriorError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn losing_step_begin_race_waits_then_serves() {
        // Both workers see Absent, both call step_begin. The first wins;
        // the second's bookkeeping error must be re-queried and converted
        // into a wait-then-serve outcome (codex round-1 P2).
        use std::sync::atomic::{AtomicU32, Ordering as AOrd};
        let store = MockDurableStore::new();
        let key = IdempotencyKey::new("scope", "wf-race", "impl", 0);

        // Pre-place an InProgress entry directly (simulating winner already
        // started). Then `run_with_fence` sees it on first query.
        let winner_reservation = store.force_in_progress(&key).await;

        // Now drive run_with_fence which should observe InProgress and wait.
        let store_for_completer = store.clone();
        let completer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            store_for_completer
                .step_complete(&winner_reservation, serde_json::json!({"served": true}))
                .await
                .expect("complete");
        });
        let outcome =
            run_with_fence(store.as_ref(), &key, Duration::from_millis(5), Duration::from_secs(2), || async {
                panic!("must not call model when winner already has reservation")
            })
            .await
            .expect("served via wait_for_completion");
        completer.await.unwrap();
        match outcome {
            FenceOutcome::Served(v) => assert_eq!(v["served"], true),
            other => panic!("expected Served, got {:?}", other),
        }

        // Now exercise the harder race: two concurrent calls both observe
        // Absent and reach step_begin. Use a fresh key.
        let key2 = IdempotencyKey::new("scope", "wf-race-2", "impl", 0);
        let store2 = MockDurableStore::new();
        let invocations = Arc::new(AtomicU32::new(0));
        let a_inv = invocations.clone();
        let b_inv = invocations.clone();
        let key_for_a = key2.clone();
        let key_for_b = key2.clone();
        let store_for_a = store2.clone();
        let store_for_b = store2.clone();
        let a = tokio::spawn(async move {
            run_with_fence(
                store_for_a.as_ref(),
                &key_for_a,
                Duration::from_millis(5),
                Duration::from_secs(2),
                || async move {
                    a_inv.fetch_add(1, AOrd::SeqCst);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    Ok(serde_json::json!({"who": "a"}))
                },
            )
            .await
        });
        // Give A a head-start so it wins step_begin.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let b = tokio::spawn(async move {
            run_with_fence(
                store_for_b.as_ref(),
                &key_for_b,
                Duration::from_millis(5),
                Duration::from_secs(2),
                || async move {
                    b_inv.fetch_add(1, AOrd::SeqCst);
                    Ok(serde_json::json!({"who": "b-should-not-run"}))
                },
            )
            .await
        });

        let a_out = a.await.unwrap().expect("a outcome");
        let b_out = b.await.unwrap().expect("b outcome");
        match a_out {
            FenceOutcome::Committed(v) => assert_eq!(v["who"], "a"),
            other => panic!("expected A Committed, got {:?}", other),
        }
        match b_out {
            FenceOutcome::Served(v) => assert_eq!(v["who"], "a"),
            other => panic!("expected B Served (winning A's result), got {:?}", other),
        }
        assert_eq!(invocations.load(AOrd::SeqCst), 1, "model called exactly once across the race");
    }

    #[test]
    fn key_format_is_stable() {
        let k = IdempotencyKey::new("samishukri__ao-cli", "wf-XYZ", "implementation", 0);
        assert_eq!(k.as_str(), "samishukri__ao-cli:wf-XYZ:implementation:0");
    }
}
