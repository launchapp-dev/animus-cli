use super::*;
use crate::services::runtime::execution_fact_projection::reconcile_completed_processes;
use crate::services::runtime::runtime_daemon::daemon_reconciliation::reconcile_manual_phase_timeouts;
use anyhow::Result;
use orchestrator_core::services::ServiceHub;
use orchestrator_daemon_runtime::{
    default_slim_project_tick_driver, CompletedProcess, DefaultProjectTickServices, DefaultSlimProjectTickDriver,
    DispatchNotice, DispatchWorkflowStartSummary, ProcessManager, ProjectTickSnapshot,
};
use std::sync::Arc;

pub(crate) struct CliProjectTickServices;

impl CliProjectTickServices {
    fn new(_args: &DaemonRuntimeOptions) -> Self {
        Self
    }
}

#[async_trait::async_trait(?Send)]
impl DefaultProjectTickServices for CliProjectTickServices {
    async fn capture_snapshot(&mut self, root: &str) -> Result<ProjectTickSnapshot> {
        let hub: Arc<dyn ServiceHub> = Arc::new(orchestrator_core::FileServiceHub::new(root)?);
        let requirements_before = hub.planning().list_requirements().await?;
        let tasks_before = hub.tasks().list().await?;
        let daemon = hub.daemon();
        let daemon_health = daemon.health().await.ok();

        Ok(ProjectTickSnapshot { requirements_before, tasks_before, started_daemon: false, daemon_health })
    }

    async fn reconcile_completed_processes(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        completed_processes: Vec<CompletedProcess>,
    ) -> Result<(usize, usize)> {
        Ok(reconcile_completed_processes(hub, root, completed_processes).await)
    }

    async fn reconcile_manual_timeouts(&mut self, hub: Arc<dyn ServiceHub>, root: &str) -> Result<usize> {
        reconcile_manual_phase_timeouts(hub, root).await
    }

    async fn dispatch_ready_tasks(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
        process_manager: Option<&mut ProcessManager>,
    ) -> Result<DispatchWorkflowStartSummary> {
        match process_manager {
            Some(process_manager) => dispatch_queued_entries_via_runner(root, process_manager, limit),
            None => Ok(DispatchWorkflowStartSummary::default()),
        }
    }

    fn dispatch_notice(&mut self, notice: DispatchNotice) {
        match notice {
            DispatchNotice::ScheduleDispatched { schedule_id, dispatch } => {
                eprintln!(
                    "{}: schedule '{}' fired workflow '{}'",
                    protocol::ACTOR_DAEMON,
                    schedule_id,
                    dispatch.workflow_ref
                );
            }
            DispatchNotice::ScheduleDispatchFailed { schedule_id, dispatch, error } => {
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

pub(crate) type SlimProjectTickDriver<'a> = DefaultSlimProjectTickDriver<'a, CliProjectTickServices>;

pub(crate) fn slim_project_tick_driver<'a>(
    args: &DaemonRuntimeOptions,
    process_manager: &'a mut ProcessManager,
) -> SlimProjectTickDriver<'a> {
    default_slim_project_tick_driver(CliProjectTickServices::new(args), process_manager)
}
