use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{
    BudgetBreachEvent, CompletedProcessReconciliation, DaemonRuntimeOptions, DispatchWorkflowStartSummary,
    ProjectTickSnapshot, ProjectTickSummary, ProjectTickSummaryInput, TickBudget,
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

    /// `true` when the fleet daily spend cap is latched and ALL new dispatch
    /// for this project must be suppressed this tick (schedules, triggers,
    /// ready tasks, queue drain). Default `false`.
    fn dispatch_suppressed(&self, _root: &str) -> bool {
        false
    }

    async fn capture_snapshot(&mut self, root: &str) -> Result<ProjectTickSnapshot>;

    async fn reconcile_completed_processes(&mut self, root: &str) -> Result<CompletedProcessReconciliation>;

    async fn reconcile_zombie_workflows(&mut self, _root: &str) -> Result<usize> {
        Ok(0)
    }

    /// TASK-1466: tear down environment leases whose workflow run is TERMINAL
    /// in the journal but whose owner died before teardown ran (the broker
    /// only retries those at startup otherwise). Housekeeping cadence, like
    /// the other reconciliation legs. Returns the number of leases torn down
    /// this sweep. Default is a no-op.
    async fn reconcile_terminal_environment_leases(&mut self, _root: &str) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_manual_timeouts(&mut self, _root: &str) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_stale_in_progress_tasks(&mut self, _root: &str, _stale_threshold_hours: u64) -> Result<usize> {
        Ok(0)
    }

    async fn cleanup_stale_workflows(&mut self, _root: &str, _max_age_hours: u64) -> Result<usize> {
        Ok(0)
    }

    /// Evaluate declared workflow/phase budget caps against observed spend
    /// and act on any newly crossed cap (record + pause). Runs on the
    /// housekeeping cadence only — never per-nudge — because it rescans run
    /// state on disk. Returns one event per breach enforced THIS sweep so
    /// the run host can notify exactly once per breach. Default is a no-op.
    async fn enforce_budget_caps(&mut self, _root: &str) -> Result<Vec<BudgetBreachEvent>> {
        Ok(Vec::new())
    }

    /// Dispatch work into free pool headroom. `queue_drain_limit` caps the
    /// explicitly enqueued dispatch-queue entries leased this tick.
    async fn dispatch_ready_tasks(
        &mut self,
        root: &str,
        _queue_drain_limit: usize,
    ) -> Result<DispatchWorkflowStartSummary>;

    async fn collect_health(&mut self, root: &str) -> Result<Value>;

    async fn build_summary(
        &mut self,
        args: &DaemonRuntimeOptions,
        input: ProjectTickSummaryInput,
    ) -> Result<ProjectTickSummary>;
}
