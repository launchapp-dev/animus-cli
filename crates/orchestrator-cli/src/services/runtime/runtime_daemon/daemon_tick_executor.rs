use super::*;
use crate::services::runtime::execution_fact_projection::reconcile_completed_processes;
use crate::services::runtime::runtime_daemon::daemon_reconciliation::{
    reconcile_manual_phase_timeouts, recover_orphaned_running_workflows,
};
use anyhow::Result;
use orchestrator_core::services::ServiceHub;
use orchestrator_core::{TaskStatus, WorkflowRunInput, WorkflowStateManager, WorkflowStatus};
use orchestrator_daemon_runtime::{
    default_slim_project_tick_driver, CompletedProcess, DefaultProjectTickServices, DefaultSlimProjectTickDriver,
    DispatchNotice, DispatchSelectionSource, DispatchWorkflowStart, DispatchWorkflowStartSummary, ProcessManager,
    ProjectTickSnapshot,
};
use orchestrator_logging::Logger;
use std::sync::Arc;

pub(crate) struct CliProjectTickServices {
    logger: Arc<Logger>,
}

impl CliProjectTickServices {
    fn new(_args: &DaemonRuntimeOptions, logger: Arc<Logger>) -> Self {
        Self { logger }
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

    async fn reconcile_zombie_workflows(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        active_subject_ids: &std::collections::HashSet<String>,
    ) -> Result<usize> {
        Ok(recover_orphaned_running_workflows(hub, root, active_subject_ids).await)
    }

    async fn reconcile_manual_timeouts(&mut self, hub: Arc<dyn ServiceHub>, root: &str) -> Result<usize> {
        reconcile_manual_phase_timeouts(hub, root).await
    }

    async fn reconcile_stale_in_progress_tasks(&mut self, hub: Arc<dyn ServiceHub>, _root: &str) -> Result<usize> {
        let tasks = hub.tasks().list().await?;
        let in_progress_tasks: Vec<_> = tasks.iter().filter(|t| t.status == TaskStatus::InProgress).collect();
        if in_progress_tasks.is_empty() {
            return Ok(0);
        }

        let workflows = hub.workflows().list().await?;
        let mut reconciled = 0usize;
        for task in in_progress_tasks {
            let task_workflows: Vec<_> = workflows.iter().filter(|w| w.task_id == task.id).collect();
            if task_workflows.is_empty() {
                continue;
            }
            let all_terminal = task_workflows.iter().all(|w| {
                matches!(
                    w.status,
                    WorkflowStatus::Completed
                        | WorkflowStatus::Failed
                        | WorkflowStatus::Cancelled
                        | WorkflowStatus::Escalated
                )
            });
            if all_terminal {
                let _ = hub.tasks().set_status(&task.id, TaskStatus::Done, false).await;
                reconciled += 1;
            }
        }
        Ok(reconciled)
    }

    async fn cleanup_stale_workflows(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        root: &str,
        max_age_hours: u64,
    ) -> Result<usize> {
        let manager = WorkflowStateManager::new(root);
        let deleted = match manager.cleanup_terminal_workflows(max_age_hours) {
            Ok(result) => {
                if result.deleted > 0 {
                    self.logger
                        .info(
                            "cleanup",
                            format!("cleaned up {} stale workflows (older than {}h)", result.deleted, max_age_hours),
                        )
                        .emit();
                }
                result.deleted
            }
            Err(e) => {
                self.logger.error("cleanup", "workflow cleanup failed").err(e.to_string()).emit();
                0
            }
        };
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["worktree", "prune"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        Ok(deleted)
    }

    async fn dispatch_ready_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
        process_manager: Option<&mut ProcessManager>,
    ) -> Result<DispatchWorkflowStartSummary> {
        let mut summary = match process_manager {
            Some(process_manager) => dispatch_queued_entries_via_runner(root, process_manager, limit)?,
            None => DispatchWorkflowStartSummary::default(),
        };

        let remaining = limit.saturating_sub(summary.started);
        if remaining > 0 {
            let tasks = hub.tasks().list_prioritized().await?;
            let ready_tasks: Vec<_> = tasks.iter().filter(|t| t.status == TaskStatus::Ready).take(remaining).collect();
            for task in ready_tasks {
                if let Ok(workflow) = hub.workflows().run(WorkflowRunInput::for_task(task.id.clone(), None)).await {
                    let _ = hub.tasks().set_status(&task.id, TaskStatus::InProgress, false).await;
                    summary.started += 1;
                    summary.started_workflows.push(DispatchWorkflowStart {
                        dispatch: protocol::SubjectDispatch::for_task(
                            task.id.clone(),
                            workflow.workflow_ref.unwrap_or_default(),
                        ),
                        workflow_id: Some(workflow.id),
                        selection_source: DispatchSelectionSource::ReadyQueue,
                    });
                }
            }
        }

        Ok(summary)
    }

    fn dispatch_notice(&mut self, notice: DispatchNotice) {
        match notice {
            DispatchNotice::ScheduleDispatched { schedule_id, dispatch } => {
                self.logger.info("schedule", format!("fired '{}'", dispatch.workflow_ref)).schedule(schedule_id).emit();
            }
            DispatchNotice::ScheduleDispatchFailed { schedule_id, dispatch, error } => {
                self.logger
                    .error("schedule", format!("dispatch failed for '{}'", dispatch.workflow_ref))
                    .schedule(schedule_id)
                    .err(error)
                    .emit();
            }
            DispatchNotice::QueueAssignmentFailed { dispatch, error } => {
                self.logger.error("queue", format!("failed to assign {}", dispatch.subject_key())).err(error).emit();
            }
            DispatchNotice::Failed { dispatch, error } => {
                self.logger
                    .error("process", format!("failed to start runner for {}", dispatch.subject_key()))
                    .err(error)
                    .emit();
            }
            DispatchNotice::Started { dispatch, .. } => {
                self.logger
                    .info("queue.dispatch", format!("dispatched {}", dispatch.subject_key()))
                    .subject(dispatch.subject_id())
                    .meta(serde_json::json!({"workflow_ref": dispatch.workflow_ref}))
                    .emit();
            }
        }
    }
}

pub(crate) type SlimProjectTickDriver<'a> = DefaultSlimProjectTickDriver<'a, CliProjectTickServices>;

pub(crate) fn slim_project_tick_driver<'a>(
    args: &DaemonRuntimeOptions,
    process_manager: &'a mut ProcessManager,
    logger: Arc<Logger>,
) -> SlimProjectTickDriver<'a> {
    default_slim_project_tick_driver(CliProjectTickServices::new(args, logger), process_manager)
}
