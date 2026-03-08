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

    async fn bootstrap_from_vision(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
        _startup_cleanup: bool,
        _ai_task_generation: bool,
    ) -> Result<()> {
        Ok(())
    }

    async fn resume_interrupted(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
    ) -> Result<(usize, usize)> {
        Ok((0, 0))
    }

    async fn recover_orphaned_running_workflows(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn reconcile_stale_tasks(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
        _stale_threshold_hours: u64,
    ) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_dependency_tasks(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
    ) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_merge_tasks(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
    ) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_completed_processes(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
    ) -> Result<(usize, usize)> {
        Ok((0, 0))
    }

    async fn retry_failed_task_workflows(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn promote_backlog_tasks_to_ready(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn dispatch_ready_tasks(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
        _limit: usize,
    ) -> Result<ReadyTaskWorkflowStartSummary> {
        Ok(ReadyTaskWorkflowStartSummary::default())
    }

    async fn refresh_runtime_binaries(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
    ) -> Result<()> {
        Ok(())
    }
}
