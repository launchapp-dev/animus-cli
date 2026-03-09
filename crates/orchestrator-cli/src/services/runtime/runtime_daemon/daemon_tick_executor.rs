use super::*;
use crate::services::runtime::runtime_daemon::daemon_reconciliation::reconcile_manual_phase_timeouts;
use crate::services::runtime::execution_fact_projection::reconcile_completed_processes;
use orchestrator_core::{promote_backlog_tasks_to_ready, retry_failed_task_workflows};
use orchestrator_daemon_runtime::{
    default_slim_project_tick_driver, CompletedProcess, DefaultProjectTickServices,
    DefaultSlimProjectTickDriver, DispatchNotice, DispatchWorkflowStartSummary, ProcessManager,
};

pub(crate) struct CliProjectTickServices;

#[async_trait::async_trait(?Send)]
impl DefaultProjectTickServices for CliProjectTickServices {
    async fn reconcile_completed_processes(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        completed_processes: Vec<CompletedProcess>,
    ) -> Result<(usize, usize)> {
        Ok(reconcile_completed_processes(hub, root, completed_processes).await)
    }

    async fn reconcile_manual_timeouts(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<usize> {
        reconcile_manual_phase_timeouts(hub, root).await
    }

    async fn dispatch_ready_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
        process_manager: Option<&mut ProcessManager>,
    ) -> Result<DispatchWorkflowStartSummary> {
        let _ = retry_failed_task_workflows(hub.clone()).await;
        let _ = promote_backlog_tasks_to_ready(hub.clone(), root).await;
        match process_manager {
            Some(process_manager) => {
                dispatch_ready_tasks_via_runner(hub, root, process_manager, limit).await
            }
            None => run_ready_task_workflows_for_project(hub, root, limit).await,
        }
    }

    fn dispatch_notice(&mut self, notice: DispatchNotice) {
        match notice {
            DispatchNotice::ScheduleDispatched {
                schedule_id,
                dispatch,
            } => {
                eprintln!(
                    "{}: schedule '{}' fired workflow '{}'",
                    protocol::ACTOR_DAEMON,
                    schedule_id,
                    dispatch.workflow_ref
                );
            }
            DispatchNotice::ScheduleDispatchFailed {
                schedule_id,
                dispatch,
                error,
            } => {
                eprintln!(
                    "{}: schedule '{}' workflow '{}' dispatch failed: {}",
                    protocol::ACTOR_DAEMON,
                    schedule_id,
                    dispatch.workflow_ref,
                    error
                );
            }
            DispatchNotice::QueueAssignmentFailed { dispatch, error } => {
                eprintln!(
                    "{}: failed to mark dispatch queue entry assigned for subject {}: {}",
                    protocol::ACTOR_DAEMON,
                    dispatch.subject_id(),
                    error
                );
            }
            DispatchNotice::Failed { dispatch, error } => {
                eprintln!(
                    "{}: failed to start workflow runner for subject {}: {}",
                    protocol::ACTOR_DAEMON,
                    dispatch.subject_id(),
                    error
                );
            }
            DispatchNotice::Started { .. } => {}
        }
    }
}

pub(crate) type SlimProjectTickDriver<'a> =
    DefaultSlimProjectTickDriver<'a, CliProjectTickServices>;

pub(crate) fn slim_project_tick_driver(
    process_manager: &mut ProcessManager,
) -> SlimProjectTickDriver<'_> {
    default_slim_project_tick_driver(CliProjectTickServices, process_manager)
}
