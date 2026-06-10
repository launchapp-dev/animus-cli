use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{
    DaemonRuntimeOptions, DispatchWorkflowStartSummary, ProjectTickSnapshot, ProjectTickSummary,
    ProjectTickSummaryInput, TickBudget,
};

#[async_trait::async_trait(?Send)]
pub trait ProjectTickHooks {
    /// Process due cron schedules, claiming dispatch slots from the shared
    /// `budget`. Each successful spawn must call [`TickBudget::try_take`]
    /// before committing so the trigger hook (called next within the same
    /// tick) sees the remaining headroom. When the budget is exhausted the
    /// implementation must skip remaining dispatches.
    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>, budget: &mut TickBudget);

    /// Process pending file-watcher and webhook trigger events, claiming
    /// dispatch slots from the shared `budget`. Default implementation is
    /// a no-op.
    fn process_due_triggers(&mut self, _root: &str, _now: DateTime<Utc>, _budget: &mut TickBudget) {}

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

    /// Dispatch work into free pool headroom. `limit` caps auto-dispatched
    /// Ready tasks (0 when `auto_run_ready` is off); `queue_drain_limit`
    /// caps explicitly enqueued dispatch-queue entries, which drain even
    /// when ready-task auto-dispatch is disabled.
    async fn dispatch_ready_tasks(
        &mut self,
        root: &str,
        _limit: usize,
        _queue_drain_limit: usize,
    ) -> Result<DispatchWorkflowStartSummary>;

    async fn collect_health(&mut self, root: &str) -> Result<Value>;

    async fn build_summary(
        &mut self,
        args: &DaemonRuntimeOptions,
        input: ProjectTickSummaryInput,
    ) -> Result<ProjectTickSummary>;
}
