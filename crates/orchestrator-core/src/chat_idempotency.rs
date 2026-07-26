//! Durable admission and outcome receipts for actor-initiated chat turns.
//!
//! A caller key is scoped by repository, workspace, actor, and conversation.
//! The row is reserved before the user message is persisted.  A short lease
//! lets a crashed pre-persistence claimant be recovered without admitting two
//! live providers for the same operation.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_CHAT_IDEMPOTENCY_KEY_BYTES: usize = 128;
pub const DEFAULT_CHAT_OPERATION_LEASE_SECS: i64 = 300;
pub const MAX_CHAT_OPERATION_ERROR_BYTES: usize = 1024;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS chat_operations (
    project_scope      TEXT NOT NULL,
    workspace_id       TEXT NOT NULL,
    actor_id           TEXT NOT NULL,
    conversation_id    TEXT NOT NULL,
    caller_key         TEXT NOT NULL,
    request_hash       TEXT NOT NULL,
    execution_hash     TEXT,
    operation_id       TEXT NOT NULL,
    user_message_id    TEXT NOT NULL,
    assistant_message_id TEXT NOT NULL,
    state               TEXT NOT NULL CHECK (state IN (
        'pending', 'user_accepted', 'completed', 'assistant_failed', 'assistant_interrupted'
    )),
    user_seq            INTEGER,
    assistant_seq       INTEGER,
    error_code          TEXT,
    error_message       TEXT,
    lease_token         TEXT,
    lease_expires_at    INTEGER,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    PRIMARY KEY (project_scope, workspace_id, actor_id, conversation_id, caller_key),
    UNIQUE (project_scope, operation_id)
);
CREATE INDEX IF NOT EXISTS idx_chat_operations_pending
    ON chat_operations(project_scope, state, lease_expires_at);
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatOperationStatus {
    Pending,
    UserAccepted,
    Completed,
    AssistantFailed,
    AssistantInterrupted,
}

impl ChatOperationStatus {
    fn as_db(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::UserAccepted => "user_accepted",
            Self::Completed => "completed",
            Self::AssistantFailed => "assistant_failed",
            Self::AssistantInterrupted => "assistant_interrupted",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "user_accepted" => Ok(Self::UserAccepted),
            "completed" => Ok(Self::Completed),
            "assistant_failed" => Ok(Self::AssistantFailed),
            "assistant_interrupted" => Ok(Self::AssistantInterrupted),
            other => Err(anyhow!("chat operation has unknown state '{other}'")),
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::AssistantFailed | Self::AssistantInterrupted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatOperationRequest {
    pub project_scope: String,
    pub workspace_id: String,
    pub actor_id: String,
    pub conversation_id: String,
    pub caller_key: String,
    pub request_hash: String,
}

impl ChatOperationRequest {
    pub fn validate(&self) -> Result<()> {
        validate_required("project scope", &self.project_scope, 512)?;
        validate_required("workspace id", &self.workspace_id, 512)?;
        validate_required("actor id", &self.actor_id, 512)?;
        validate_required("conversation id", &self.conversation_id, 512)?;
        validate_required("request hash", &self.request_hash, 128)?;
        let key = self.caller_key.as_bytes();
        if key.is_empty() || key.len() > MAX_CHAT_IDEMPOTENCY_KEY_BYTES {
            return Err(anyhow!("chat idempotency key must contain 1..={} bytes", MAX_CHAT_IDEMPOTENCY_KEY_BYTES));
        }
        if !self
            .caller_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
        {
            return Err(anyhow!("chat idempotency key may contain only ASCII letters, digits, '.', '_', ':', and '-'"));
        }
        Ok(())
    }
}

fn validate_required(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max_bytes || value.bytes().any(|byte| byte == 0) {
        return Err(anyhow!("chat operation {label} must contain 1..={max_bytes} non-NUL bytes"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatOperationClaim {
    request: ChatOperationRequest,
    pub operation_id: String,
    pub user_message_id: String,
    pub assistant_message_id: String,
    pub status: ChatOperationStatus,
    pub user_seq: Option<u64>,
    /// Hash of the resolved provider/profile execution snapshot. Bound after
    /// caller admission and reused by recovered pending operations.
    pub execution_hash: Option<String>,
    lease_token: String,
    pub recovered: bool,
}

impl ChatOperationClaim {
    pub fn request(&self) -> &ChatOperationRequest {
        &self.request
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatOperationReceipt {
    pub operation_id: String,
    pub conversation_id: String,
    pub user_message_id: String,
    pub user_seq: Option<u64>,
    pub assistant_message_id: String,
    pub assistant_seq: Option<u64>,
    pub status: ChatOperationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug)]
pub enum ChatOperationBegin {
    Acquired(Box<ChatOperationClaim>),
    Replay(Box<ChatOperationReceipt>),
    InProgress,
    Conflict,
}

#[derive(Debug, Clone)]
pub struct ChatOperationStore {
    db_path: PathBuf,
    project_scope: String,
    lease_secs: i64,
}

impl ChatOperationStore {
    pub fn for_project(project_root: &Path) -> Result<Self> {
        let scoped = protocol::scoped_state_root(project_root)
            .ok_or_else(|| anyhow!("could not resolve scoped runtime root for chat operation storage"))?;
        Ok(Self {
            db_path: scoped.join("chat-operations.db"),
            project_scope: protocol::repository_scope_for_path(project_root),
            lease_secs: DEFAULT_CHAT_OPERATION_LEASE_SECS,
        })
    }

    /// Construct a store at an explicit path. Primarily useful for embedded
    /// runtimes and deterministic tests; normal CLI callers use `for_project`.
    pub fn at_path(db_path: PathBuf, project_scope: impl Into<String>) -> Self {
        Self { db_path, project_scope: project_scope.into(), lease_secs: DEFAULT_CHAT_OPERATION_LEASE_SECS }
    }

    #[cfg(test)]
    fn with_path_and_lease(db_path: PathBuf, project_scope: impl Into<String>, lease_secs: i64) -> Self {
        Self { db_path, project_scope: project_scope.into(), lease_secs }
    }

    pub fn request(
        &self,
        workspace_id: impl Into<String>,
        actor_id: impl Into<String>,
        conversation_id: impl Into<String>,
        caller_key: impl Into<String>,
        request_hash: impl Into<String>,
    ) -> ChatOperationRequest {
        ChatOperationRequest {
            project_scope: self.project_scope.clone(),
            workspace_id: workspace_id.into(),
            actor_id: actor_id.into(),
            conversation_id: conversation_id.into(),
            caller_key: caller_key.into(),
            request_hash: request_hash.into(),
        }
    }

    pub fn begin(&self, request: ChatOperationRequest) -> Result<ChatOperationBegin> {
        self.begin_at(request, Utc::now().timestamp())
    }

    fn begin_at(&self, request: ChatOperationRequest, now: i64) -> Result<ChatOperationBegin> {
        request.validate()?;
        let conn = self.open()?;
        let tx = immediate_transaction_with_busy_retry(&conn)?;
        let operation_id = Uuid::new_v4().to_string();
        let user_message_id = format!("msg-{}", Uuid::new_v4());
        let assistant_message_id = format!("msg-{}", Uuid::new_v4());
        let lease_token = Uuid::new_v4().to_string();
        let lease_expires_at = now.saturating_add(self.lease_secs);
        let inserted = tx.execute(
            "INSERT INTO chat_operations (
                project_scope, workspace_id, actor_id, conversation_id, caller_key, request_hash,
                operation_id, user_message_id, assistant_message_id, state, user_seq, assistant_seq,
                error_code, error_message, lease_token, lease_expires_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', NULL, NULL,
                       NULL, NULL, ?10, ?11, ?12, ?12)
             ON CONFLICT(project_scope, workspace_id, actor_id, conversation_id, caller_key) DO NOTHING",
            params![
                request.project_scope,
                request.workspace_id,
                request.actor_id,
                request.conversation_id,
                request.caller_key,
                request.request_hash,
                operation_id,
                user_message_id,
                assistant_message_id,
                lease_token,
                lease_expires_at,
                now,
            ],
        )? == 1;

        let row = load_row(&tx, &request)?.ok_or_else(|| anyhow!("chat operation disappeared during transaction"))?;
        if row.request_hash != request.request_hash {
            tx.commit()?;
            return Ok(ChatOperationBegin::Conflict);
        }
        if row.status.is_terminal() {
            tx.commit()?;
            return Ok(ChatOperationBegin::Replay(Box::new(row.receipt(&request.conversation_id)?)));
        }
        if inserted {
            tx.commit()?;
            return Ok(ChatOperationBegin::Acquired(Box::new(row.claim(request, lease_token, false)?)));
        }
        if row.lease_expires_at.unwrap_or(i64::MAX) > now {
            tx.commit()?;
            return Ok(ChatOperationBegin::InProgress);
        }

        let previous_token = row.lease_token.clone().unwrap_or_default();
        let updated = tx.execute(
            "UPDATE chat_operations
             SET lease_token = ?6, lease_expires_at = ?7, updated_at = ?8
             WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3
               AND conversation_id = ?4 AND caller_key = ?5
               AND state IN ('pending', 'user_accepted')
               AND COALESCE(lease_token, '') = ?9 AND COALESCE(lease_expires_at, 0) <= ?8",
            params![
                request.project_scope,
                request.workspace_id,
                request.actor_id,
                request.conversation_id,
                request.caller_key,
                lease_token,
                lease_expires_at,
                now,
                previous_token,
            ],
        )?;
        tx.commit()?;
        if updated == 1 {
            Ok(ChatOperationBegin::Acquired(Box::new(row.claim(request, lease_token, true)?)))
        } else {
            Ok(ChatOperationBegin::InProgress)
        }
    }

    pub fn renew(&self, claim: &ChatOperationClaim) -> Result<bool> {
        let now = Utc::now().timestamp();
        let conn = self.open()?;
        let updated = conn.execute(
            "UPDATE chat_operations SET lease_expires_at = ?7, updated_at = ?8
             WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3
               AND conversation_id = ?4 AND caller_key = ?5 AND operation_id = ?6
               AND state IN ('pending', 'user_accepted') AND lease_token = ?9",
            params![
                claim.request.project_scope,
                claim.request.workspace_id,
                claim.request.actor_id,
                claim.request.conversation_id,
                claim.request.caller_key,
                claim.operation_id,
                now.saturating_add(self.lease_secs),
                now,
                claim.lease_token,
            ],
        )?;
        Ok(updated == 1)
    }

    pub fn bind_execution_hash(&self, claim: &mut ChatOperationClaim, execution_hash: &str) -> Result<bool> {
        validate_required("execution hash", execution_hash, 128)?;
        let conn = self.open()?;
        let updated = conn.execute(
            "UPDATE chat_operations SET execution_hash = ?7, updated_at = ?8
             WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3
               AND conversation_id = ?4 AND caller_key = ?5 AND operation_id = ?6
               AND state = 'pending' AND lease_token = ?9
               AND (execution_hash IS NULL OR execution_hash = ?7)",
            params![
                claim.request.project_scope,
                claim.request.workspace_id,
                claim.request.actor_id,
                claim.request.conversation_id,
                claim.request.caller_key,
                claim.operation_id,
                execution_hash,
                Utc::now().timestamp(),
                claim.lease_token,
            ],
        )?;
        if updated == 1 {
            claim.execution_hash = Some(execution_hash.to_string());
        }
        Ok(updated == 1)
    }

    /// Rebind an expired, recovered pending claim after the caller has proved
    /// under the conversation lock that no canonical user row exists. Once a
    /// user row exists the operation must be reconciled instead of changing
    /// its execution snapshot.
    pub fn rebind_recovered_execution_hash(
        &self,
        claim: &mut ChatOperationClaim,
        execution_hash: &str,
    ) -> Result<bool> {
        validate_required("execution hash", execution_hash, 128)?;
        if !claim.recovered {
            return Ok(false);
        }
        let conn = self.open()?;
        let updated = conn.execute(
            "UPDATE chat_operations SET execution_hash = ?7, updated_at = ?8
             WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3
               AND conversation_id = ?4 AND caller_key = ?5 AND operation_id = ?6
               AND state = 'pending' AND lease_token = ?9 AND user_seq IS NULL
               AND execution_hash IS NOT NULL AND execution_hash <> ?7",
            params![
                claim.request.project_scope,
                claim.request.workspace_id,
                claim.request.actor_id,
                claim.request.conversation_id,
                claim.request.caller_key,
                claim.operation_id,
                execution_hash,
                Utc::now().timestamp(),
                claim.lease_token,
            ],
        )?;
        if updated == 1 {
            claim.execution_hash = Some(execution_hash.to_string());
        }
        Ok(updated == 1)
    }

    /// Release a lease-owned pending admission only when no user acceptance
    /// was journaled. The caller must hold the conversation lock and prove no
    /// canonical user row exists before invoking this method.
    pub fn release_pending(&self, claim: &ChatOperationClaim) -> Result<bool> {
        let conn = self.open()?;
        let deleted = conn.execute(
            "DELETE FROM chat_operations
             WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3
               AND conversation_id = ?4 AND caller_key = ?5 AND operation_id = ?6
               AND state = 'pending' AND user_seq IS NULL AND lease_token = ?7",
            params![
                claim.request.project_scope,
                claim.request.workspace_id,
                claim.request.actor_id,
                claim.request.conversation_id,
                claim.request.caller_key,
                claim.operation_id,
                claim.lease_token,
            ],
        )?;
        Ok(deleted == 1)
    }

    pub fn mark_user_accepted(&self, claim: &mut ChatOperationClaim, user_seq: u64) -> Result<bool> {
        let conn = self.open()?;
        let now = Utc::now().timestamp();
        let updated = conn.execute(
            "UPDATE chat_operations SET state = 'user_accepted', user_seq = ?7, updated_at = ?8
             WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3
               AND conversation_id = ?4 AND caller_key = ?5 AND operation_id = ?6
               AND state = 'pending' AND lease_token = ?9",
            params![
                claim.request.project_scope,
                claim.request.workspace_id,
                claim.request.actor_id,
                claim.request.conversation_id,
                claim.request.caller_key,
                claim.operation_id,
                i64::try_from(user_seq).context("user message sequence exceeds SQLite range")?,
                now,
                claim.lease_token,
            ],
        )?;
        if updated == 1 {
            claim.status = ChatOperationStatus::UserAccepted;
            claim.user_seq = Some(user_seq);
        }
        Ok(updated == 1)
    }

    pub fn complete(&self, claim: &ChatOperationClaim, assistant_seq: u64) -> Result<bool> {
        self.finish(claim, ChatOperationStatus::Completed, Some(assistant_seq), None, None)
    }

    pub fn fail(&self, claim: &ChatOperationClaim, code: &str, message: &str) -> Result<bool> {
        self.finish(claim, ChatOperationStatus::AssistantFailed, None, Some(code), Some(&bound_error(message)))
    }

    pub fn interrupt(&self, claim: &ChatOperationClaim, message: &str) -> Result<bool> {
        self.finish(
            claim,
            ChatOperationStatus::AssistantInterrupted,
            None,
            Some("assistant_interrupted"),
            Some(&bound_error(message)),
        )
    }

    pub fn receipt(&self, claim: &ChatOperationClaim) -> Result<ChatOperationReceipt> {
        let conn = self.open()?;
        load_row(&conn, &claim.request)?
            .ok_or_else(|| anyhow!("chat operation disappeared"))?
            .receipt(&claim.request.conversation_id)
    }

    fn finish(
        &self,
        claim: &ChatOperationClaim,
        status: ChatOperationStatus,
        assistant_seq: Option<u64>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<bool> {
        debug_assert!(status.is_terminal());
        let conn = self.open()?;
        let updated = conn.execute(
            "UPDATE chat_operations SET state = ?7, assistant_seq = ?8, error_code = ?9,
                    error_message = ?10, lease_token = NULL, lease_expires_at = NULL, updated_at = ?11
             WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3
               AND conversation_id = ?4 AND caller_key = ?5 AND operation_id = ?6
               AND state = 'user_accepted' AND lease_token = ?12",
            params![
                claim.request.project_scope,
                claim.request.workspace_id,
                claim.request.actor_id,
                claim.request.conversation_id,
                claim.request.caller_key,
                claim.operation_id,
                status.as_db(),
                assistant_seq.map(i64::try_from).transpose().context("assistant sequence exceeds SQLite range")?,
                error_code,
                error_message,
                Utc::now().timestamp(),
                claim.lease_token,
            ],
        )?;
        Ok(updated == 1)
    }

    fn open(&self) -> Result<Connection> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating chat operation store directory {}", parent.display()))?;
        }
        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("opening chat operation store {}", self.db_path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        execute_batch_with_busy_retry(&conn, "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        execute_batch_with_busy_retry(&conn, SCHEMA)?;
        let has_execution_hash = {
            let mut statement = conn.prepare("PRAGMA table_info(chat_operations)")?;
            let columns =
                statement.query_map([], |row| row.get::<_, String>(1))?.collect::<std::result::Result<Vec<_>, _>>()?;
            columns.iter().any(|name| name == "execution_hash")
        };
        if !has_execution_hash {
            conn.execute("ALTER TABLE chat_operations ADD COLUMN execution_hash TEXT", [])?;
        }
        Ok(conn)
    }
}

fn bound_error(value: &str) -> String {
    if value.len() <= MAX_CHAT_OPERATION_ERROR_BYTES {
        return value.to_string();
    }
    let mut end = MAX_CHAT_OPERATION_ERROR_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

struct StoredOperation {
    request_hash: String,
    execution_hash: Option<String>,
    operation_id: String,
    user_message_id: String,
    assistant_message_id: String,
    status: ChatOperationStatus,
    user_seq: Option<i64>,
    assistant_seq: Option<i64>,
    error_code: Option<String>,
    error_message: Option<String>,
    lease_token: Option<String>,
    lease_expires_at: Option<i64>,
}

impl StoredOperation {
    fn claim(&self, request: ChatOperationRequest, lease_token: String, recovered: bool) -> Result<ChatOperationClaim> {
        Ok(ChatOperationClaim {
            request,
            operation_id: self.operation_id.clone(),
            user_message_id: self.user_message_id.clone(),
            assistant_message_id: self.assistant_message_id.clone(),
            status: self.status,
            user_seq: self.user_seq.map(u64::try_from).transpose().context("negative user sequence in chat journal")?,
            execution_hash: self.execution_hash.clone(),
            lease_token,
            recovered,
        })
    }

    fn receipt(&self, conversation_id: &str) -> Result<ChatOperationReceipt> {
        Ok(ChatOperationReceipt {
            operation_id: self.operation_id.clone(),
            conversation_id: conversation_id.to_string(),
            user_message_id: self.user_message_id.clone(),
            user_seq: self.user_seq.map(u64::try_from).transpose().context("negative user sequence in chat journal")?,
            assistant_message_id: self.assistant_message_id.clone(),
            assistant_seq: self
                .assistant_seq
                .map(u64::try_from)
                .transpose()
                .context("negative assistant sequence in chat journal")?,
            status: self.status,
            error_code: self.error_code.clone(),
            error_message: self.error_message.clone(),
        })
    }
}

fn load_row(conn: &Connection, request: &ChatOperationRequest) -> Result<Option<StoredOperation>> {
    conn.query_row(
        "SELECT request_hash, execution_hash, operation_id, user_message_id, assistant_message_id, state,
                user_seq, assistant_seq, error_code, error_message, lease_token, lease_expires_at
         FROM chat_operations
         WHERE project_scope = ?1 AND workspace_id = ?2 AND actor_id = ?3
           AND conversation_id = ?4 AND caller_key = ?5",
        params![
            request.project_scope,
            request.workspace_id,
            request.actor_id,
            request.conversation_id,
            request.caller_key,
        ],
        |row| {
            let state: String = row.get(5)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                state,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
            ))
        },
    )
    .optional()?
    .map(|tuple| {
        Ok(StoredOperation {
            request_hash: tuple.0,
            execution_hash: tuple.1,
            operation_id: tuple.2,
            user_message_id: tuple.3,
            assistant_message_id: tuple.4,
            status: ChatOperationStatus::parse(&tuple.5)?,
            user_seq: tuple.6,
            assistant_seq: tuple.7,
            error_code: tuple.8,
            error_message: tuple.9,
            lease_token: tuple.10,
            lease_expires_at: tuple.11,
        })
    })
    .transpose()
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
            if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(store: &ChatOperationStore, actor: &str, key: &str, hash: &str) -> ChatOperationRequest {
        store.request("tenant-a", actor, "conv-1", key, hash)
    }

    #[test]
    fn exact_replay_conflict_and_actor_partition() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChatOperationStore::with_path_and_lease(tmp.path().join("ops.db"), "repo", 60);
        let ChatOperationBegin::Acquired(mut claim) = store.begin(request(&store, "alice", "key", "hash")).unwrap()
        else {
            panic!("first caller should acquire");
        };
        assert!(store.mark_user_accepted(&mut claim, 7).unwrap());
        assert!(store.complete(&claim, 8).unwrap());
        let ChatOperationBegin::Replay(receipt) = store.begin(request(&store, "alice", "key", "hash")).unwrap() else {
            panic!("exact retry should replay");
        };
        assert_eq!(receipt.status, ChatOperationStatus::Completed);
        assert_eq!(receipt.user_seq, Some(7));
        assert_eq!(receipt.assistant_seq, Some(8));
        assert!(matches!(
            store.begin(request(&store, "alice", "key", "different")).unwrap(),
            ChatOperationBegin::Conflict
        ));
        assert!(matches!(
            store.begin(request(&store, "bob", "key", "different")).unwrap(),
            ChatOperationBegin::Acquired(_)
        ));
    }

    #[test]
    fn concurrent_admission_has_one_claimant() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ops.db");
        let mut joins = Vec::new();
        for _ in 0..12 {
            let path = path.clone();
            joins.push(std::thread::spawn(move || {
                let store = ChatOperationStore::with_path_and_lease(path, "repo", 60);
                matches!(
                    store.begin(request(&store, "alice", "same", "hash")).unwrap(),
                    ChatOperationBegin::Acquired(_)
                )
            }));
        }
        assert_eq!(joins.into_iter().map(|join| join.join().unwrap()).filter(|acquired| *acquired).count(), 1);
    }

    #[test]
    fn expired_claim_recovers_same_message_identity_across_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ops.db");
        let first = ChatOperationStore::with_path_and_lease(path.clone(), "repo", 1);
        let req = request(&first, "alice", "key", "hash");
        let ChatOperationBegin::Acquired(original) = first.begin_at(req.clone(), 10).unwrap() else {
            panic!("initial claim");
        };
        drop(first);
        let restarted = ChatOperationStore::with_path_and_lease(path, "repo", 1);
        let ChatOperationBegin::Acquired(recovered) = restarted.begin_at(req, 12).unwrap() else {
            panic!("expired claim should recover");
        };
        assert!(recovered.recovered);
        assert_eq!(recovered.operation_id, original.operation_id);
        assert_eq!(recovered.user_message_id, original.user_message_id);
    }

    #[test]
    fn execution_hash_is_bound_once_and_survives_pending_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ops.db");
        let first = ChatOperationStore::with_path_and_lease(path.clone(), "repo", 1);
        let req = request(&first, "alice", "key", "caller-hash");
        let ChatOperationBegin::Acquired(mut original) = first.begin_at(req.clone(), 10).unwrap() else {
            panic!("initial claim");
        };

        assert!(first.bind_execution_hash(&mut original, "execution-hash").unwrap());
        assert_eq!(original.execution_hash.as_deref(), Some("execution-hash"));
        assert!(first.bind_execution_hash(&mut original, "execution-hash").unwrap());
        assert!(!first.bind_execution_hash(&mut original, "different-execution").unwrap());
        drop(first);

        let restarted = ChatOperationStore::with_path_and_lease(path, "repo", 1);
        let ChatOperationBegin::Acquired(mut recovered) = restarted.begin_at(req, 12).unwrap() else {
            panic!("expired claim should recover");
        };
        assert!(recovered.recovered);
        assert_eq!(recovered.execution_hash.as_deref(), Some("execution-hash"));
        assert!(restarted.rebind_recovered_execution_hash(&mut recovered, "replacement-execution").unwrap());
        assert_eq!(recovered.execution_hash.as_deref(), Some("replacement-execution"));
    }

    #[test]
    fn legacy_database_is_migrated_with_execution_hash_column() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ops.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE chat_operations (
                project_scope TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                actor_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                caller_key TEXT NOT NULL,
                request_hash TEXT NOT NULL,
                operation_id TEXT NOT NULL,
                user_message_id TEXT NOT NULL,
                assistant_message_id TEXT NOT NULL,
                state TEXT NOT NULL,
                user_seq INTEGER,
                assistant_seq INTEGER,
                error_code TEXT,
                error_message TEXT,
                lease_token TEXT,
                lease_expires_at INTEGER,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (project_scope, workspace_id, actor_id, conversation_id, caller_key),
                UNIQUE (project_scope, operation_id)
            );",
        )
        .unwrap();
        drop(conn);

        let store = ChatOperationStore::with_path_and_lease(path, "repo", 60);
        let ChatOperationBegin::Acquired(mut claim) =
            store.begin(request(&store, "alice", "key", "caller-hash")).unwrap()
        else {
            panic!("migrated store should admit a claim");
        };
        assert!(store.bind_execution_hash(&mut claim, "execution-hash").unwrap());
        assert_eq!(claim.execution_hash.as_deref(), Some("execution-hash"));
    }

    #[test]
    fn expired_user_accepted_claim_reconciles_without_new_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ops.db");
        let first = ChatOperationStore::with_path_and_lease(path.clone(), "repo", 1);
        let req = request(&first, "alice", "key", "hash");
        let ChatOperationBegin::Acquired(mut original) = first.begin_at(req.clone(), 10).unwrap() else {
            panic!("initial claim");
        };
        assert!(first.mark_user_accepted(&mut original, 4).unwrap());
        drop(first);

        let restarted = ChatOperationStore::with_path_and_lease(path, "repo", 1);
        let ChatOperationBegin::Acquired(recovered) = restarted.begin_at(req.clone(), 12).unwrap() else {
            panic!("expired accepted claim should be returned for read-side reconciliation");
        };
        assert!(recovered.recovered);
        assert_eq!(recovered.status, ChatOperationStatus::UserAccepted);
        assert_eq!(recovered.user_seq, Some(4));
        assert_eq!(recovered.operation_id, original.operation_id);
        assert!(restarted.interrupt(&recovered, "provider completion could not be proven after restart").unwrap());
        let ChatOperationBegin::Replay(receipt) = restarted.begin(req).unwrap() else {
            panic!("reconciled interruption should replay");
        };
        assert_eq!(receipt.status, ChatOperationStatus::AssistantInterrupted);
        assert_eq!(receipt.user_seq, Some(4));
    }

    #[test]
    fn tenant_and_conversation_are_independent_key_partitions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChatOperationStore::with_path_and_lease(tmp.path().join("ops.db"), "repo", 60);
        assert!(matches!(
            store.begin(store.request("tenant-a", "alice", "conv-a", "same", "hash-a")).unwrap(),
            ChatOperationBegin::Acquired(_)
        ));
        assert!(matches!(
            store.begin(store.request("tenant-b", "alice", "conv-a", "same", "hash-b")).unwrap(),
            ChatOperationBegin::Acquired(_)
        ));
        assert!(matches!(
            store.begin(store.request("tenant-a", "alice", "conv-b", "same", "hash-c")).unwrap(),
            ChatOperationBegin::Acquired(_)
        ));
    }

    #[test]
    fn lease_owner_can_release_only_a_not_yet_accepted_operation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChatOperationStore::with_path_and_lease(tmp.path().join("ops.db"), "repo", 60);
        let first_request = request(&store, "alice", "first", "hash");
        let ChatOperationBegin::Acquired(first) = store.begin(first_request.clone()).unwrap() else {
            panic!("first claim");
        };
        assert!(store.release_pending(&first).unwrap());
        assert!(matches!(store.begin(first_request).unwrap(), ChatOperationBegin::Acquired(_)));

        let second_request = request(&store, "alice", "second", "hash");
        let ChatOperationBegin::Acquired(mut second) = store.begin(second_request).unwrap() else {
            panic!("second claim");
        };
        assert!(store.mark_user_accepted(&mut second, 0).unwrap());
        assert!(!store.release_pending(&second).unwrap(), "accepted operations are never releasable");
    }

    #[test]
    fn failure_is_bounded_and_replayed() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChatOperationStore::with_path_and_lease(tmp.path().join("ops.db"), "repo", 60);
        let ChatOperationBegin::Acquired(mut claim) = store.begin(request(&store, "alice", "key", "hash")).unwrap()
        else {
            panic!("claim");
        };
        store.mark_user_accepted(&mut claim, 0).unwrap();
        store.fail(&claim, "provider_failed", &"é".repeat(800)).unwrap();
        let ChatOperationBegin::Replay(receipt) = store.begin(request(&store, "alice", "key", "hash")).unwrap() else {
            panic!("replay");
        };
        assert_eq!(receipt.status, ChatOperationStatus::AssistantFailed);
        assert!(receipt.error_message.unwrap().len() <= MAX_CHAT_OPERATION_ERROR_BYTES);
    }
}
