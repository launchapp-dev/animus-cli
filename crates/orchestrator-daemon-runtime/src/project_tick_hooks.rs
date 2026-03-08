use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_core::services::ServiceHub;

use crate::ReadyTaskWorkflowStartSummary;

#[async_trait::async_trait(?Send)]
pub trait ProjectTickHooks {
    fn build_hub(&mut self, root: &str) -> Result<Arc<dyn ServiceHub>>;

    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>);

    fn flush_git_outbox(&mut self, root: &str);

    fn active_process_count(&self) -> usize {
        0
    }

    fn emit_notice(&mut self, _message: &str) {}

    async fn reconcile_completed_processes(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
    ) -> Result<(usize, usize)> {
        Ok((0, 0))
    }

    async fn dispatch_ready_tasks(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
        _limit: usize,
    ) -> Result<ReadyTaskWorkflowStartSummary> {
        Ok(ReadyTaskWorkflowStartSummary::default())
    }
}
