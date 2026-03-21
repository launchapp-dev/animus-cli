use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{
    DaemonRuntimeOptions, DispatchWorkflowStartSummary, ProjectTickSnapshot, ProjectTickSummary,
    ProjectTickSummaryInput,
};

#[async_trait::async_trait(?Send)]
pub trait ProjectTickHooks {
    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>);

    async fn capture_snapshot(&mut self, root: &str) -> Result<ProjectTickSnapshot>;

    async fn reconcile_completed_processes(&mut self, root: &str) -> Result<(usize, usize)>;

    async fn reconcile_zombie_workflows(&mut self, _root: &str) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_manual_timeouts(&mut self, _root: &str) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_runner_blocked_tasks(&mut self, _root: &str) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_stale_in_progress_tasks(&mut self, _root: &str) -> Result<usize> {
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
