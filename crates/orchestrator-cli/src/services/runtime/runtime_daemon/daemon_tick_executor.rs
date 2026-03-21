use super::*;
use crate::services::runtime::execution_fact_projection::reconcile_completed_processes;
use crate::services::runtime::runtime_daemon::daemon_reconciliation::{
    reconcile_manual_phase_timeouts, reconcile_runner_blocked_tasks, recover_orphaned_running_workflows,
};
use anyhow::Result;
use orchestrator_core::services::ServiceHub;
use orchestrator_core::{TaskStatus, WorkflowRunInput, WorkflowStatus};
use orchestrator_daemon_runtime::{
    default_slim_project_tick_driver, CompletedProcess, DefaultProjectTickServices, DefaultSlimProjectTickDriver,
    DispatchNotice, DispatchSelectionSource, DispatchWorkflowStart, DispatchWorkflowStartSummary, ProcessManager,
    ProjectTickSnapshot,
};
use std::sync::Arc;

pub(crate) struct CliProjectTickServices {
    last_auto_rebalance: Option<std::time::Instant>,
}

impl CliProjectTickServices {
    fn new(_args: &DaemonRuntimeOptions) -> Self {
        Self { last_auto_rebalance: None }
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

    async fn reconcile_runner_blocked_tasks(&mut self, hub: Arc<dyn ServiceHub>, root: &str) -> Result<usize> {
        reconcile_runner_blocked_tasks(hub, root).await
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

    async fn auto_rebalance_priorities(&mut self, hub: Arc<dyn ServiceHub>, _root: &str) -> Result<usize> {
        const REBALANCE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
        let now = std::time::Instant::now();
        if let Some(last) = self.last_auto_rebalance {
            if now.duration_since(last) < REBALANCE_INTERVAL {
                return Ok(0);
            }
        }

        let tasks = hub.tasks().list().await?;
        let options = orchestrator_core::TaskPriorityRebalanceOptions::default();
        let plan = orchestrator_core::plan_task_priority_rebalance(&tasks, options)?;

        self.last_auto_rebalance = Some(now);

        if plan.changes.is_empty() {
            return Ok(0);
        }

        let tasks_by_id: std::collections::HashMap<String, orchestrator_core::OrchestratorTask> =
            tasks.into_iter().map(|task| (task.id.clone(), task)).collect();

        let mut changed = 0usize;
        for change in &plan.changes {
            if let Some(mut task) = tasks_by_id.get(change.task_id.as_str()).cloned() {
                task.priority = change.to;
                task.metadata.updated_by = protocol::ACTOR_DAEMON.to_string();
                if hub.tasks().replace(task).await.is_ok() {
                    changed += 1;
                }
            }
        }

        eprintln!(
            "{}: auto-rebalanced priority for {} tasks (high budget: {}%, critical budget: {}%, overflow was: high={} critical={})",
            protocol::ACTOR_DAEMON,
            changed,
            plan.high_budget_percent,
            plan.critical_budget_percent,
            plan.before.high_budget_overflow,
            plan.before.critical_budget_overflow,
        );
        Ok(changed)
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
                    dispatch.subject_key(),
                    error
                );
            }
            DispatchNotice::Failed { dispatch, error } => {
                eprintln!(
                    "{}: failed to start workflow runner for subject {}: {}",
                    protocol::ACTOR_DAEMON,
                    dispatch.subject_key(),
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
