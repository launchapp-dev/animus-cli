use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{
    DaemonRuntimeOptions, DispatchWorkflowStartSummary, ProjectTickSnapshot, ProjectTickSummary,
    ProjectTickSummaryInput,
};

#[async_trait::async_trait(?Send)]
pub trait ProjectTickHooks {
    /// Process due cron schedules, dispatching up to `schedule_headroom`
    /// additional workflow-runner processes.  When `schedule_headroom` is
    /// `Some(0)` the implementation must skip all dispatches.
    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>, schedule_headroom: Option<usize>);

    /// Process pending file-watcher trigger events, dispatching up to
    /// `trigger_headroom` additional workflow-runner processes.  When
    /// `trigger_headroom` is `Some(0)` the implementation must skip all
    /// dispatches.  Default implementation is a no-op.
    fn process_due_triggers(&mut self, _root: &str, _now: DateTime<Utc>, _trigger_headroom: Option<usize>) {}

    /// Return the current number of active workflow-runner child processes.
    /// Used to recompute headroom after schedule dispatches.
    fn active_process_count(&mut self) -> usize {
        let _ = self;
        0
    }

    async fn capture_snapshot(&mut self, root: &str) -> Result<ProjectTickSnapshot>;

    async fn reconcile_completed_processes(&mut self, root: &str) -> Result<(usize, usize)>;

    async fn reconcile_zombie_workflows(&mut self, _root: &str) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_manual_timeouts(&mut self, _root: &str) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_stale_in_progress_tasks(&mut self, _root: &str) -> Result<usize> {
        Ok(0)
    }

    async fn cleanup_stale_workflows(&mut self, _root: &str, _max_age_hours: u64) -> Result<usize> {
        Ok(0)
    }

    async fn dispatch_ready_tasks(&mut self, root: &str, _limit: usize) -> Result<DispatchWorkflowStartSummary>;

    async fn collect_health(&mut self, root: &str) -> Result<Value>;

    async fn build_summary(
        &mut self,
        args: &DaemonRuntimeOptions,
        input: ProjectTickSummaryInput,
    ) -> Result<ProjectTickSummary>;
}
