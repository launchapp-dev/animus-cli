//! Durable, caller-scoped idempotency for actor-initiated workflow launches.
//!
//! The reservation lives beside the local workflow journal in `workflow.db`,
//! even when an external `workflow_journal` plugin owns the run records.  This
//! keeps the kernel's launch admission atomic across CLI/daemon processes while
//! leaving journal persistence behind the existing plugin seam.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::OrchestratorWorkflow;

pub const MAX_WORKFLOW_LAUNCH_IDEMPOTENCY_KEY_BYTES: usize = 128;
pub const DEFAULT_WORKFLOW_LAUNCH_LEASE_SECS: i64 = 300;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS workflow_launch_idempotency (
    project_scope    TEXT NOT NULL,
    workspace_id    TEXT NOT NULL,
    actor_id        TEXT NOT NULL,
    caller_key      TEXT NOT NULL,
    request_hash    TEXT NOT NULL,
    workflow_ref    TEXT NOT NULL,
    subject_id      TEXT NOT NULL,
    workflow_id     TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('pending', 'completed')),
    lease_token     TEXT,
    lease_expires_at INTEGER,
    response_json   TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (project_scope, workspace_id, actor_id, caller_key),
    UNIQUE (project_scope, workflow_id)
);
CREATE INDEX IF NOT EXISTS idx_workflow_launch_idempotency_pending
    ON workflow_launch_idempotency(project_scope, state, lease_expires_at);
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowLaunchIdempotencyRequest {
    pub project_scope: String,
    pub workspace_id: String,
    pub actor_id: String,
    pub caller_key: String,
    pub request_hash: String,
    pub workflow_ref: String,
    pub subject_id: String,
}

impl WorkflowLaunchIdempotencyRequest {
    pub fn validate(&self) -> Result<()> {
        validate_required("project scope", &self.project_scope, 512)?;
        validate_required("workspace id", &self.workspace_id, 512)?;
        validate_required("actor id", &self.actor_id, 512)?;
        validate_required("workflow ref", &self.workflow_ref, 512)?;
        validate_required("subject id", &self.subject_id, 1024)?;
        validate_required("request hash", &self.request_hash, 128)?;
        let key = self.caller_key.as_bytes();
        if key.is_empty() || key.len() > MAX_WORKFLOW_LAUNCH_IDEMPOTENCY_KEY_BYTES {
            return Err(anyhow!(
                "workflow launch idempotency key must contain 1..={} bytes",
                MAX_WORKFLOW_LAUNCH_IDEMPOTENCY_KEY_BYTES
            ));
        }
        if !self
            .caller_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
        {
            return Err(anyhow!(
                "workflow launch idempotency key may contain only ASCII letters, digits, '.', '_', ':', and '-'"
            ));
        }
        Ok(())
    }
}

fn validate_required(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max_bytes || value.bytes().any(|byte| byte == 0) {
        return Err(anyhow!("workflow launch {label} must contain 1..={max_bytes} non-NUL bytes"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowLaunchClaim {
    request: WorkflowLaunchIdempotencyRequest,
    pub workflow_id: String,
    lease_token: String,
    pub recovered: bool,
}

impl WorkflowLaunchClaim {
    pub fn request(&self) -> &WorkflowLaunchIdempotencyRequest {
        &self.request
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowLaunchReplay {
    pub workflow: OrchestratorWorkflow,
}

#[derive(Debug)]
pub enum WorkflowLaunchBegin {
    Acquired(Box<WorkflowLaunchClaim>),
    Replay(Box<WorkflowLaunchReplay>),
    InProgress,
    Conflict,
}

#[derive(Debug, Clone)]
pub struct WorkflowLaunchIdempotencyStore {
    db_path: PathBuf,
    project_scope: String,
    lease_secs: i64,
}

impl WorkflowLaunchIdempotencyStore {
    pub fn for_project(project_root: &Path) -> Self {
        Self {
            db_path: super::state_manager::db_path_for_project(project_root),
            project_scope: protocol::repository_scope_for_path(project_root),
            lease_secs: DEFAULT_WORKFLOW_LAUNCH_LEASE_SECS,
        }
    }

    #[cfg(test)]
    fn at_path(db_path: PathBuf, project_scope: impl Into<String>) -> Self {
        Self { db_path, project_scope: project_scope.into(), lease_secs: DEFAULT_WORKFLOW_LAUNCH_LEASE_SECS }
    }

    pub fn request(
        &self,
        workspace_id: impl Into<String>,
        actor_id: impl Into<String>,
        caller_key: impl Into<String>,
        request_hash: impl Into<String>,
        workflow_ref: impl Into<String>,
        subject_id: impl Into<String>,
    ) -> WorkflowLaunchIdempotencyRequest {
        WorkflowLaunchIdempotencyRequest {
            project_scope: self.project_scope.clone(),
            workspace_id: workspace_id.into(),
            actor_id: actor_id.into(),
            caller_key: caller_key.into(),
            request_hash: request_hash.into(),
            workflow_ref: workflow_ref.into(),
            subject_id: subject_id.into(),
        }
    }

    pub fn begin(&self, request: WorkflowLaunchIdempotencyRequest) -> Result<WorkflowLaunchBegin> {
        self.begin_at(request, Utc::now().timestamp())
    }

    fn begin_at(&self, request: WorkflowLaunchIdempotencyRequest, now: i64) -> Result<WorkflowLaunchBegin> {
        request.validate()?;
        let conn = self.open()?;
        let tx = immediate_transaction_with_busy_retry(&conn)
            .context("failed to begin workflow launch idempotency reservation")?;
        let workflow_id = Uuid::new_v4().to_string();
        let lease_token = Uuid::new_v4().to_string();
        let lease_expires_at = now.saturating_add(self.lease_secs);
        let inserted = tx.execute(
            "INSERT INTO workflow_launch_idempotency (
                project_scope, workspace_id, actor_id, caller_key, request_hash,
                workflow_ref, subject_id, workflow_id, state, lease_token,
                lease_expires_at, response_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?10, NULL, ?11, ?11)
             ON CONFLICT(project_scope, workspace_id, actor_id, caller_key) DO NOTHING",
            params![
                request.project_scope,
                request.workspace_id,
                request.actor_id,
                request.caller_key,
                request.request_hash,
                request.workflow_ref,
                request.subject_id,
                workflow_id,
                lease_token,
                lease_expires_at,
                now,
            ],
        )? == 1;

        let row = tx
            .query_row(
                "SELECT request_hash, workflow_ref, subject_id, workflow_id, state,
                        lease_token, lease_expires_at, response_json
                 FROM workflow_launch_idempotency
                 WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3 AND caller_key = ?4",
                params![request.project_scope, request.workspace_id, request.actor_id, request.caller_key],
                |row| {
                    Ok(StoredLaunch {
                        request_hash: row.get(0)?,
                        workflow_ref: row.get(1)?,
                        subject_id: row.get(2)?,
                        workflow_id: row.get(3)?,
                        state: row.get(4)?,
                        lease_token: row.get(5)?,
                        lease_expires_at: row.get(6)?,
                        response_json: row.get(7)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("workflow launch reservation disappeared during transaction"))?;

        if row.request_hash != request.request_hash
            || row.workflow_ref != request.workflow_ref
            || row.subject_id != request.subject_id
        {
            tx.commit()?;
            return Ok(WorkflowLaunchBegin::Conflict);
        }
        if row.state == "completed" {
            let response = row
                .response_json
                .ok_or_else(|| anyhow!("completed workflow launch reservation has no canonical response"))?;
            let workflow = serde_json::from_str(&response)
                .context("completed workflow launch reservation has an invalid canonical response")?;
            tx.commit()?;
            return Ok(WorkflowLaunchBegin::Replay(Box::new(WorkflowLaunchReplay { workflow })));
        }
        if row.state != "pending" {
            return Err(anyhow!("workflow launch reservation has unknown state"));
        }
        if inserted {
            tx.commit()?;
            return Ok(WorkflowLaunchBegin::Acquired(Box::new(WorkflowLaunchClaim {
                request,
                workflow_id: row.workflow_id,
                lease_token,
                recovered: false,
            })));
        }
        if row.lease_expires_at.unwrap_or(i64::MAX) > now {
            tx.commit()?;
            return Ok(WorkflowLaunchBegin::InProgress);
        }

        let previous_token = row.lease_token.unwrap_or_default();
        let updated = tx.execute(
            "UPDATE workflow_launch_idempotency
             SET lease_token = ?5, lease_expires_at = ?6, updated_at = ?7
             WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3 AND caller_key = ?4
               AND state = 'pending' AND COALESCE(lease_token, '') = ?8
               AND COALESCE(lease_expires_at, 0) <= ?7",
            params![
                request.project_scope,
                request.workspace_id,
                request.actor_id,
                request.caller_key,
                lease_token,
                lease_expires_at,
                now,
                previous_token,
            ],
        )?;
        tx.commit()?;
        if updated == 1 {
            Ok(WorkflowLaunchBegin::Acquired(Box::new(WorkflowLaunchClaim {
                request,
                workflow_id: row.workflow_id,
                lease_token,
                recovered: true,
            })))
        } else {
            Ok(WorkflowLaunchBegin::InProgress)
        }
    }

    /// Extend a claim before the irreversible runner spawn. A claimant that
    /// lost its lease must stop; the new owner will reconcile the preallocated
    /// workflow id instead of allowing a duplicate spawn.
    pub fn renew(&self, claim: &WorkflowLaunchClaim) -> Result<bool> {
        self.renew_at(claim, Utc::now().timestamp())
    }

    fn renew_at(&self, claim: &WorkflowLaunchClaim, now: i64) -> Result<bool> {
        let conn = self.open()?;
        let updated = conn.execute(
            "UPDATE workflow_launch_idempotency
             SET lease_expires_at = ?6, updated_at = ?7
             WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3 AND caller_key = ?4
               AND state = 'pending' AND lease_token = ?5",
            params![
                claim.request.project_scope,
                claim.request.workspace_id,
                claim.request.actor_id,
                claim.request.caller_key,
                claim.lease_token,
                now.saturating_add(self.lease_secs),
                now,
            ],
        )?;
        Ok(updated == 1)
    }

    pub fn complete(&self, claim: &WorkflowLaunchClaim, workflow: &OrchestratorWorkflow) -> Result<bool> {
        let response_json = serde_json::to_string(workflow).context("failed to encode canonical workflow launch")?;
        let conn = self.open()?;
        let updated = conn.execute(
            "UPDATE workflow_launch_idempotency
             SET state = 'completed', response_json = ?6, lease_token = NULL,
                 lease_expires_at = NULL, updated_at = ?7
             WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3 AND caller_key = ?4
               AND workflow_id = ?5 AND state = 'pending' AND lease_token = ?8",
            params![
                claim.request.project_scope,
                claim.request.workspace_id,
                claim.request.actor_id,
                claim.request.caller_key,
                claim.workflow_id,
                response_json,
                Utc::now().timestamp(),
                claim.lease_token,
            ],
        )?;
        Ok(updated == 1)
    }

    fn open(&self) -> Result<Connection> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create idempotency store directory at {}", parent.display()))?;
        }
        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("failed to open idempotency store at {}", self.db_path.display()))?;
        // Install the wait policy before WAL/schema pragmas: several Portal
        // workers may cold-open the scope concurrently, and journal-mode DDL
        // otherwise fails immediately with SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        execute_batch_with_busy_retry(&conn, "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        execute_batch_with_busy_retry(&conn, SCHEMA).context("failed to migrate workflow launch idempotency schema")?;
        Ok(conn)
    }
}

fn execute_batch_with_busy_retry(conn: &Connection, sql: &str) -> Result<()> {
    for attempt in 0..8u32 {
        match conn.execute_batch(sql) {
            Ok(()) => return Ok(()),
            Err(error) if sqlite_is_busy(&error) && attempt < 7 => {
                std::thread::sleep(std::time::Duration::from_millis(5u64.saturating_mul(1u64 << attempt)));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("bounded SQLite retry loop always returns")
}

fn immediate_transaction_with_busy_retry(conn: &Connection) -> Result<rusqlite::Transaction<'_>> {
    for attempt in 0..8u32 {
        match rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate) {
            Ok(transaction) => return Ok(transaction),
            Err(error) if sqlite_is_busy(&error) && attempt < 7 => {
                std::thread::sleep(std::time::Duration::from_millis(5u64.saturating_mul(1u64 << attempt)));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("bounded SQLite retry loop always returns")
}

fn sqlite_is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

struct StoredLaunch {
    request_hash: String,
    workflow_ref: String,
    subject_id: String,
    workflow_id: String,
    state: String,
    lease_token: Option<String>,
    lease_expires_at: Option<i64>,
    response_json: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use tempfile::tempdir;

    use super::*;

    fn store() -> (tempfile::TempDir, WorkflowLaunchIdempotencyStore) {
        let dir = tempdir().expect("tempdir");
        let store = WorkflowLaunchIdempotencyStore::at_path(dir.path().join("workflow.db"), "project-a");
        (dir, store)
    }

    fn request(store: &WorkflowLaunchIdempotencyStore, key: &str, hash: &str) -> WorkflowLaunchIdempotencyRequest {
        store.request("workspace-a", "alice", key, hash, "animus.task/standard", "task:TASK-1")
    }

    #[test]
    fn concurrent_callers_get_one_atomic_reservation() {
        let (_dir, store) = store();
        let store = Arc::new(store);
        let barrier = Arc::new(Barrier::new(8));
        let mut joins = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            let barrier = barrier.clone();
            joins.push(thread::spawn(move || {
                barrier.wait();
                store.begin_at(request(&store, "launch-1", "hash-a"), 100).expect("begin")
            }));
        }
        let mut acquired = 0;
        let mut pending = 0;
        for join in joins {
            match join.join().expect("thread") {
                WorkflowLaunchBegin::Acquired(_) => acquired += 1,
                WorkflowLaunchBegin::InProgress => pending += 1,
                other => panic!("unexpected reservation result: {other:?}"),
            }
        }
        assert_eq!(acquired, 1);
        assert_eq!(pending, 7);
    }

    #[test]
    fn scope_partitions_prevent_cross_actor_and_workspace_collisions() {
        let (_dir, store) = store();
        for (workspace, actor) in [("workspace-a", "alice"), ("workspace-a", "bob"), ("workspace-b", "alice")] {
            let request = store.request(workspace, actor, "same-key", "same-hash", "wf", "task:TASK-1");
            assert!(matches!(store.begin_at(request, 100).unwrap(), WorkflowLaunchBegin::Acquired(_)));
        }
    }

    #[test]
    fn changed_effective_request_conflicts_without_disclosing_run() {
        let (_dir, store) = store();
        assert!(matches!(
            store.begin_at(request(&store, "same-key", "hash-a"), 100).unwrap(),
            WorkflowLaunchBegin::Acquired(_)
        ));
        assert!(matches!(
            store.begin_at(request(&store, "same-key", "hash-b"), 101).unwrap(),
            WorkflowLaunchBegin::Conflict
        ));
    }

    #[test]
    fn completed_retry_replays_byte_equivalent_canonical_snapshot_after_restart() {
        let (dir, store) = store();
        let claim = match store.begin_at(request(&store, "replay", "hash-a"), 100).unwrap() {
            WorkflowLaunchBegin::Acquired(claim) => claim,
            other => panic!("unexpected: {other:?}"),
        };
        let workflow = crate::WorkflowLifecycleExecutor::default().bootstrap(
            claim.workflow_id.clone(),
            crate::WorkflowRunInput::for_task("TASK-1".to_string(), Some("animus.task/standard".to_string())),
        );
        let canonical = serde_json::to_string(&workflow).unwrap();
        assert!(store.complete(&claim, &workflow).unwrap());

        let restarted = WorkflowLaunchIdempotencyStore::at_path(dir.path().join("workflow.db"), "project-a");
        let replay = match restarted.begin_at(request(&restarted, "replay", "hash-a"), 999).unwrap() {
            WorkflowLaunchBegin::Replay(replay) => replay,
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(serde_json::to_string(&replay.workflow).unwrap(), canonical);
        assert_eq!(replay.workflow.id, claim.workflow_id);
    }

    #[test]
    fn project_scope_is_explicit_even_if_two_stores_share_one_database() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("workflow.db");
        let project_a = WorkflowLaunchIdempotencyStore::at_path(db.clone(), "project-a");
        let project_b = WorkflowLaunchIdempotencyStore::at_path(db, "project-b");
        assert!(matches!(
            project_a.begin_at(request(&project_a, "same-key", "same-hash"), 100).unwrap(),
            WorkflowLaunchBegin::Acquired(_)
        ));
        assert!(matches!(
            project_b.begin_at(request(&project_b, "same-key", "same-hash"), 100).unwrap(),
            WorkflowLaunchBegin::Acquired(_)
        ));
    }

    #[test]
    fn pending_survives_restart_and_expired_claim_is_reconciled_with_same_run_id() {
        let (dir, store) = store();
        let first = match store.begin_at(request(&store, "restart", "hash-a"), 100).unwrap() {
            WorkflowLaunchBegin::Acquired(claim) => claim,
            other => panic!("unexpected: {other:?}"),
        };
        let restarted = WorkflowLaunchIdempotencyStore::at_path(dir.path().join("workflow.db"), "project-a");
        assert!(matches!(
            restarted.begin_at(request(&restarted, "restart", "hash-a"), 101).unwrap(),
            WorkflowLaunchBegin::InProgress
        ));
        let recovered = match restarted
            .begin_at(request(&restarted, "restart", "hash-a"), 100 + DEFAULT_WORKFLOW_LAUNCH_LEASE_SECS)
            .unwrap()
        {
            WorkflowLaunchBegin::Acquired(claim) => claim,
            other => panic!("unexpected: {other:?}"),
        };
        assert!(recovered.recovered);
        assert_eq!(recovered.workflow_id, first.workflow_id);
        assert!(!restarted.renew_at(&first, 999).unwrap(), "superseded process must lose its spawn authority");
        let workflow = crate::WorkflowLifecycleExecutor::default().bootstrap(
            recovered.workflow_id.clone(),
            crate::WorkflowRunInput::for_task("TASK-1".to_string(), Some("animus.task/standard".to_string())),
        );
        assert!(!restarted.complete(&first, &workflow).unwrap(), "superseded claim cannot publish a response");
        assert!(restarted.complete(&recovered, &workflow).unwrap());
    }

    #[test]
    fn validation_matches_portal_key_contract_and_rejects_actorless_scope() {
        let (_dir, store) = store();
        assert!(request(&store, "ok._:-09", "hash").validate().is_ok());
        let oversized = "x".repeat(MAX_WORKFLOW_LAUNCH_IDEMPOTENCY_KEY_BYTES + 1);
        assert!(request(&store, &oversized, "hash").validate().is_err());
        assert!(request(&store, "has space", "hash").validate().is_err());
        let actorless = store.request("workspace-a", "", "key", "hash", "wf", "task:TASK-1");
        assert!(actorless.validate().is_err());
        let workspaceless = store.request("", "alice", "key", "hash", "wf", "task:TASK-1");
        assert!(workspaceless.validate().is_err());
    }
}
