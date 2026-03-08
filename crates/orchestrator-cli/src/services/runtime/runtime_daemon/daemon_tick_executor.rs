use super::*;
#[path = "daemon_default_project_tick_driver.rs"]
mod default_project_tick_driver;

#[cfg(test)]
use default_project_tick_driver::{default_full_project_tick_driver, DefaultFullProjectTickDriver};
use default_project_tick_driver::{
    default_slim_project_tick_driver, DefaultProjectTickServices, DefaultSlimProjectTickDriver,
};
use orchestrator_daemon_runtime::{
    reconcile_completed_processes, CompletedProcess, ProcessManager,
};
use orchestrator_git_ops as git_ops;

pub(crate) struct CliProjectTickServices;

#[async_trait::async_trait(?Send)]
impl DefaultProjectTickServices for CliProjectTickServices {
    fn flush_git_outbox(&mut self, root: &str) {
        let _ = git_ops::flush_git_integration_outbox(root);
    }

    async fn reconcile_completed_processes(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        completed_processes: Vec<CompletedProcess>,
    ) -> Result<(usize, usize)> {
        Ok(reconcile_completed_processes(hub, root, completed_processes).await)
    }

    async fn dispatch_ready_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
        process_manager: Option<&mut ProcessManager>,
    ) -> Result<ReadyTaskWorkflowStartSummary> {
        let _ = retry_failed_task_workflows(hub.clone()).await;
        let _ = promote_backlog_tasks_to_ready(hub.clone(), root).await;
        match process_manager {
            Some(process_manager) => {
                dispatch_ready_tasks_via_runner(hub, root, process_manager, limit).await
            }
            None => run_ready_task_workflows_for_project(hub, root, limit).await,
        }
    }
}

#[cfg(test)]
pub(super) type FullProjectTickDriver = DefaultFullProjectTickDriver<CliProjectTickServices>;

pub(crate) type SlimProjectTickDriver<'a> =
    DefaultSlimProjectTickDriver<'a, CliProjectTickServices>;

#[cfg(test)]
pub(super) fn full_project_tick_driver() -> FullProjectTickDriver {
    default_full_project_tick_driver(CliProjectTickServices)
}

pub(crate) fn slim_project_tick_driver(
    process_manager: &mut ProcessManager,
) -> SlimProjectTickDriver<'_> {
    default_slim_project_tick_driver(CliProjectTickServices, process_manager)
}
