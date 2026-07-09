//! Task read/write surface the execution-projection layer drives.
//!
//! The status/annotation projections used to write directly through
//! `hub.tasks()` (the legacy in-tree task store). On a plugin-backed / portal
//! deployment that store is EMPTY — tasks live in the installed
//! `subject_backend` plugin — so a finished/failed/cancelled/escalated run
//! projected the task's terminal status into a store nothing reads, leaving the
//! real (plugin-backed) subject stuck `InProgress` until the 24h stale sweep.
//!
//! [`TaskProjectionStore`] abstracts the exact task operations the projections
//! need so the same projection LOGIC can target either the in-tree store
//! ([`HubTaskProjectionStore`], the stock scaffold with no subject plugin) or
//! the installed subject backend via the router (the CLI/daemon-runtime layer's
//! `RouterTaskProjectionStore`, chosen when a plugin owns `task`). This mirrors
//! the already-correct `RouterStaleTaskStore` pattern the daemon's stale
//! reconciler uses.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use crate::{services::ServiceHub, TaskStatus};

/// Projection-relevant snapshot of a task, sourced from either the in-tree
/// store or a subject backend plugin. Only the fields the status/annotation
/// projections branch on are carried.
#[derive(Debug, Clone)]
pub struct TaskProjectionView {
    /// Current lifecycle status.
    pub status: TaskStatus,
    /// Current `blocked_reason` annotation, if any.
    pub blocked_reason: Option<String>,
    /// Current `blocked_by` marker, if any.
    pub blocked_by: Option<String>,
}

/// Task status/annotation write + read surface the execution projections drive.
///
/// The DECISION logic (terminal Blocked-vs-Cancelled, pause-marker
/// upgrade/skip) stays in the projection functions; this trait only abstracts
/// the IO so those writes reach whichever store actually backs `task` on the
/// current deployment.
#[async_trait]
pub trait TaskProjectionStore: Send + Sync {
    /// Read the projection-relevant fields for a task. `Err` when the task
    /// cannot be read (absent / backend error) — callers treat that as
    /// "task not present, skip", matching the old `hub.tasks().get` gating.
    async fn get(&self, id: &str) -> Result<TaskProjectionView>;

    /// Set the lifecycle status. No state-machine validation is applied,
    /// matching the projection's historical `set_status(.., false)` contract.
    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<()>;

    /// Transition to `Blocked` and record the failure reason + optional blocker.
    async fn block_with_reason(&self, id: &str, reason: String, blocked_by: Option<String>) -> Result<()>;

    /// Write the informational `blocked_reason` (and, when provided,
    /// `blocked_by`) annotation WITHOUT changing the lifecycle status.
    async fn annotate_blocked_reason(&self, id: &str, reason: String, set_blocked_by: Option<String>) -> Result<()>;

    /// Clear the `blocked_reason` annotation. Also clears `blocked_by` when
    /// `clear_blocked_by` is true.
    async fn clear_blocked_reason(&self, id: &str, clear_blocked_by: bool) -> Result<()>;

    /// Transition to `InProgress` and record the assigned agent role/model.
    async fn start_workflow(&self, id: &str, role: String, model: Option<String>, updated_by: String) -> Result<()>;
}

/// In-tree task-store view. Behaviour is byte-identical to the pre-routing
/// projections (metadata bumps, `updated_by` actor, `paused`/`blocked_at`
/// bookkeeping); used by the stock scaffold (no subject plugin owning `task`)
/// and the existing hub-based projection tests.
pub struct HubTaskProjectionStore {
    hub: Arc<dyn ServiceHub>,
}

impl HubTaskProjectionStore {
    #[must_use]
    pub fn new(hub: Arc<dyn ServiceHub>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl TaskProjectionStore for HubTaskProjectionStore {
    async fn get(&self, id: &str) -> Result<TaskProjectionView> {
        let task = self.hub.tasks().get(id).await?;
        Ok(TaskProjectionView { status: task.status, blocked_reason: task.blocked_reason, blocked_by: task.blocked_by })
    }

    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        self.hub.tasks().set_status(id, status, false).await.map(|_| ())
    }

    async fn block_with_reason(&self, id: &str, reason: String, blocked_by: Option<String>) -> Result<()> {
        let mut task = self.hub.tasks().get(id).await?;
        task.status = TaskStatus::Blocked;
        task.paused = true;
        task.blocked_reason = Some(reason);
        task.blocked_at = Some(Utc::now());
        task.blocked_phase = None;
        task.blocked_by = blocked_by;
        task.metadata.updated_at = Utc::now();
        task.metadata.updated_by = protocol::ACTOR_DAEMON.to_string();
        task.metadata.version = task.metadata.version.saturating_add(1);
        self.hub.tasks().replace(task).await.map(|_| ())
    }

    async fn annotate_blocked_reason(&self, id: &str, reason: String, set_blocked_by: Option<String>) -> Result<()> {
        let mut task = self.hub.tasks().get(id).await?;
        task.blocked_reason = Some(reason);
        if let Some(blocked_by) = set_blocked_by {
            task.blocked_by = Some(blocked_by);
        }
        task.metadata.updated_at = Utc::now();
        task.metadata.updated_by = protocol::ACTOR_CORE.to_string();
        task.metadata.version = task.metadata.version.saturating_add(1);
        self.hub.tasks().replace(task).await.map(|_| ())
    }

    async fn clear_blocked_reason(&self, id: &str, clear_blocked_by: bool) -> Result<()> {
        let mut task = self.hub.tasks().get(id).await?;
        task.blocked_reason = None;
        if clear_blocked_by {
            task.blocked_by = None;
        }
        task.metadata.updated_at = Utc::now();
        task.metadata.updated_by = protocol::ACTOR_CORE.to_string();
        task.metadata.version = task.metadata.version.saturating_add(1);
        self.hub.tasks().replace(task).await.map(|_| ())
    }

    async fn start_workflow(&self, id: &str, role: String, model: Option<String>, updated_by: String) -> Result<()> {
        self.hub.tasks().set_status(id, TaskStatus::InProgress, false).await?;
        self.hub.tasks().assign_agent(id, role, model, updated_by).await.map(|_| ())
    }
}

/// Convenience: box a hub-backed store for callers that want a
/// `Box<dyn TaskProjectionStore>` fallback.
#[must_use]
pub fn hub_task_projection_store(hub: Arc<dyn ServiceHub>) -> Box<dyn TaskProjectionStore> {
    Box::new(HubTaskProjectionStore::new(hub))
}
