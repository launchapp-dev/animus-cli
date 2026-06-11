use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_core::{
    project_schedule_dispatch_attempt, project_schedule_dispatch_missed, services::ServiceHub, DaemonStatus,
    DaemonTickMetrics, FileServiceHub, OrchestratorTask,
};
use serde_json::Value;

use crate::{
    CompletedProcess, CompletedProcessReconciliation, DaemonRuntimeOptions, DispatchNotice, DispatchWorkflowStart,
    DispatchWorkflowStartSummary, ProcessManager, ProjectTickHooks, ProjectTickSnapshot, ProjectTickSummary,
    ProjectTickSummaryInput, ScheduleDispatch, TaskStateChangeEvent, TickBudget, TickSummaryBuilder, TriggerDispatch,
};

#[async_trait::async_trait(?Send)]
pub trait DefaultProjectTickServices {
    async fn capture_snapshot(&mut self, root: &str) -> Result<ProjectTickSnapshot> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        let requirements_before = hub.planning().list_requirements().await?;
        let tasks_before = hub.tasks().list().await?;
        let daemon = hub.daemon();
        let status = daemon.status().await?;
        let mut started_daemon = false;
        if !matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
            daemon.start(Default::default()).await?;
            started_daemon = true;
        }
        let daemon_health = daemon.health().await.ok();

        Ok(ProjectTickSnapshot { requirements_before, tasks_before, started_daemon, daemon_health })
    }

    async fn reconcile_completed_processes(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        completed_processes: Vec<CompletedProcess>,
    ) -> Result<CompletedProcessReconciliation>;

    async fn reconcile_zombie_workflows(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
        _active_subject_ids: &std::collections::HashSet<String>,
    ) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_manual_timeouts(&mut self, _hub: Arc<dyn ServiceHub>, _root: &str) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_stale_in_progress_tasks(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
        _active_subject_ids: &std::collections::HashSet<String>,
        _stale_threshold_hours: u64,
    ) -> Result<usize> {
        Ok(0)
    }

    async fn cleanup_stale_workflows(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        _root: &str,
        _max_age_hours: u64,
    ) -> Result<usize> {
        Ok(0)
    }

    async fn dispatch_ready_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
        queue_drain_limit: usize,
        process_manager: Option<&mut ProcessManager>,
    ) -> Result<DispatchWorkflowStartSummary>;

    async fn collect_health(&mut self, root: &str) -> Result<Value> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        Ok(serde_json::to_value(hub.daemon().health().await?)?)
    }

    async fn build_summary(
        &mut self,
        root: &str,
        args: &DaemonRuntimeOptions,
        input: ProjectTickSummaryInput,
    ) -> Result<ProjectTickSummary> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        let task_state_changes =
            collect_task_state_changes(&input.tasks_before, &hub.tasks().list().await?, &input.ready_started_workflows);
        let metrics = DaemonTickMetrics::collect(hub, args.stale_threshold_hours).await?;
        let mut summary = TickSummaryBuilder::build(args, input, metrics)?;
        summary.task_state_changes = task_state_changes;
        Ok(summary)
    }

    fn record_schedule_dispatch_attempt(
        &mut self,
        project_root: &str,
        schedule_id: &str,
        run_at: DateTime<Utc>,
        status: &str,
    ) {
        project_schedule_dispatch_attempt(project_root, schedule_id, run_at, status);
    }

    /// Record a schedule attempt that the dispatcher refused (e.g. pool at
    /// capacity). Default forwards to `project_schedule_dispatch_missed`,
    /// which leaves `last_run` untouched so the schedule re-fires on the
    /// next tick.
    fn record_schedule_dispatch_missed(&mut self, project_root: &str, schedule_id: &str, status: &str) {
        project_schedule_dispatch_missed(project_root, schedule_id, status);
    }

    fn dispatch_notice(&mut self, _notice: DispatchNotice) {}
}

fn collect_task_state_changes(
    tasks_before: &[OrchestratorTask],
    tasks_after: &[OrchestratorTask],
    started_workflows: &[DispatchWorkflowStart],
) -> Vec<TaskStateChangeEvent> {
    let before_by_id: std::collections::HashMap<&str, &OrchestratorTask> =
        tasks_before.iter().map(|task| (task.id.as_str(), task)).collect();
    let selection_by_task_id: std::collections::HashMap<&str, crate::DispatchSelectionSource> = started_workflows
        .iter()
        .filter_map(|started| started.task_id().map(|task_id| (task_id, started.selection_source)))
        .collect();

    tasks_after
        .iter()
        .filter_map(|task| {
            let previous = before_by_id.get(task.id.as_str())?;
            if previous.status == task.status {
                return None;
            }

            Some(TaskStateChangeEvent {
                task_id: task.id.clone(),
                from_status: previous.status.to_string(),
                to_status: task.status.to_string(),
                changed_at: task.metadata.updated_at.to_rfc3339(),
                selection_source: selection_by_task_id.get(task.id.as_str()).copied(),
                blocked_reason: if task.status == orchestrator_core::TaskStatus::Blocked {
                    task.blocked_reason.clone()
                } else {
                    None
                },
            })
        })
        .collect()
}

pub type DefaultSlimProjectTickDriver<'a, S> = DefaultSlimProjectTickHooks<'a, S>;

pub fn default_slim_project_tick_driver<'a, S>(
    services: S,
    process_manager: &'a mut ProcessManager,
) -> DefaultSlimProjectTickDriver<'a, S>
where
    S: DefaultProjectTickServices,
{
    DefaultSlimProjectTickHooks { services, process_manager }
}

pub struct DefaultSlimProjectTickHooks<'a, S> {
    services: S,
    process_manager: &'a mut ProcessManager,
}

impl<S> DefaultSlimProjectTickHooks<'_, S> {
    pub fn active_process_count(&self) -> usize {
        self.process_manager.active_count()
    }
}

#[async_trait::async_trait(?Send)]
impl<S> ProjectTickHooks for DefaultSlimProjectTickHooks<'_, S>
where
    S: DefaultProjectTickServices,
{
    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>, budget: &mut TickBudget) {
        // Skip entirely when the shared tick budget is already exhausted.
        if budget.is_exhausted() {
            return;
        }

        // Track per-schedule outcomes so we can split projection writes:
        // success → record_schedule_dispatch_attempt (updates last_run + run_count)
        // budget-rejected → record_schedule_dispatch_missed (NO last_run update,
        //                   increments missed_count so the schedule re-fires
        //                   on the next tick)
        // other failures → record_schedule_dispatch_attempt (last_run updates so
        //                   we don't retry every tick within the same minute)
        let mut budget_rejected: Vec<String> = Vec::new();

        let outcomes = ScheduleDispatch::process_due_schedules(root, now, |schedule_id, dispatch| {
            // Claim a budget slot BEFORE attempting the spawn. If the budget
            // is exhausted, surface a sentinel error so the outer loop records
            // a "missed" outcome instead of an "attempted" one.
            if !budget.try_take() {
                budget_rejected.push(schedule_id.to_string());
                return Err(anyhow::anyhow!("schedule dispatch skipped: tick budget exhausted"));
            }
            match self.process_manager.spawn_workflow_runner(dispatch, root) {
                Ok(()) => {
                    self.services.dispatch_notice(DispatchNotice::ScheduleDispatched {
                        schedule_id: schedule_id.to_string(),
                        dispatch: dispatch.clone(),
                    });
                    Ok(())
                }
                Err(error) => {
                    // Spawn failed for a reason OTHER than our pre-check (e.g.
                    // ProcessManager's own capacity guard or runner-command
                    // build failed). Return the slot to the budget so the
                    // remaining schedules + triggers can still use it.
                    budget.release();
                    if error.downcast_ref::<crate::WorkflowConcurrencyCapReached>().is_some() {
                        // Recoverable capacity rejection: record as missed so
                        // last_run stays untouched and the occurrence re-fires
                        // on the next tick instead of being consumed.
                        budget_rejected.push(schedule_id.to_string());
                    } else {
                        self.services.dispatch_notice(DispatchNotice::ScheduleDispatchFailed {
                            schedule_id: schedule_id.to_string(),
                            dispatch: dispatch.clone(),
                            error: error.to_string(),
                        });
                    }
                    Err(error)
                }
            }
        });

        let budget_rejected: std::collections::HashSet<String> = budget_rejected.into_iter().collect();
        for outcome in outcomes {
            if budget_rejected.contains(&outcome.schedule_id) {
                // Pool was full — DO NOT update last_run, so the schedule
                // gets another shot on the next tick within the same cron
                // minute.
                self.services.record_schedule_dispatch_missed(root, &outcome.schedule_id, &outcome.status);
            } else {
                self.services.record_schedule_dispatch_attempt(
                    root,
                    &outcome.schedule_id,
                    outcome.run_at,
                    &outcome.status,
                );
            }
        }
    }

    fn process_due_triggers(&mut self, root: &str, now: DateTime<Utc>, budget: &mut TickBudget) {
        if budget.is_exhausted() {
            return;
        }

        let _outcomes = TriggerDispatch::process_due_triggers(root, now, |_trigger_id, dispatch| {
            // Webhook events stay queued (not popped) until the spawn
            // succeeds; trigger_dispatch::process_due_triggers handles the
            // peek-vs-pop. Here we just gate on the shared tick budget so
            // schedule and trigger paths share one pool of headroom.
            if !budget.try_take() {
                return Err(anyhow::anyhow!("trigger dispatch skipped: tick budget exhausted"));
            }
            match self.process_manager.spawn_workflow_runner(dispatch, root) {
                Ok(()) => Ok(()),
                Err(error) => {
                    // Spawn failed for non-budget reasons; release the slot.
                    budget.release();
                    Err(error)
                }
            }
        });
    }

    fn active_process_count(&mut self) -> usize {
        self.process_manager.active_count()
    }

    async fn capture_snapshot(&mut self, root: &str) -> Result<ProjectTickSnapshot> {
        self.services.capture_snapshot(root).await
    }

    async fn reconcile_completed_processes(&mut self, root: &str) -> Result<CompletedProcessReconciliation> {
        let completed_processes = self.process_manager.check_running().await;
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        self.services.reconcile_completed_processes(hub, root, completed_processes).await
    }

    async fn reconcile_zombie_workflows(&mut self, root: &str) -> Result<usize> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        let active_subject_ids = self.process_manager.active_subject_ids();
        self.services.reconcile_zombie_workflows(hub, root, &active_subject_ids).await
    }

    async fn reconcile_manual_timeouts(&mut self, root: &str) -> Result<usize> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        self.services.reconcile_manual_timeouts(hub, root).await
    }

    async fn reconcile_stale_in_progress_tasks(&mut self, root: &str, stale_threshold_hours: u64) -> Result<usize> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        let active_subject_ids = self.process_manager.active_subject_ids();
        self.services.reconcile_stale_in_progress_tasks(hub, root, &active_subject_ids, stale_threshold_hours).await
    }

    async fn cleanup_stale_workflows(&mut self, root: &str, max_age_hours: u64) -> Result<usize> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        self.services.cleanup_stale_workflows(hub, root, max_age_hours).await
    }

    async fn dispatch_ready_tasks(
        &mut self,
        root: &str,
        limit: usize,
        queue_drain_limit: usize,
    ) -> Result<DispatchWorkflowStartSummary> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        self.services.dispatch_ready_tasks(hub, root, limit, queue_drain_limit, Some(self.process_manager)).await
    }

    async fn collect_health(&mut self, root: &str) -> Result<Value> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        let process_count = self.process_manager.active_count();
        hub.daemon().set_active_process_count(process_count).await?;
        Ok(serde_json::to_value(hub.daemon().health().await?)?)
    }

    async fn build_summary(
        &mut self,
        args: &DaemonRuntimeOptions,
        input: ProjectTickSummaryInput,
    ) -> Result<ProjectTickSummary> {
        let root = input.project_root.clone();
        self.services.build_summary(&root, args, input).await
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orchestrator_core::{OrchestratorTask, Priority, TaskStatus, TaskType};

    use super::collect_task_state_changes;

    fn task(id: &str, status: TaskStatus, blocked_reason: Option<&str>) -> OrchestratorTask {
        let now = Utc::now();
        OrchestratorTask {
            id: id.to_string(),
            title: format!("Task {id}"),
            description: String::new(),
            task_type: TaskType::Feature,
            status,
            blocked_reason: blocked_reason.map(ToOwned::to_owned),
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            priority: Priority::Medium,
            risk: orchestrator_core::RiskLevel::Medium,
            scope: orchestrator_core::Scope::Medium,
            complexity: orchestrator_core::Complexity::default(),
            impact_area: Vec::new(),
            assignee: orchestrator_core::Assignee::Unassigned,
            estimated_effort: None,
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
            dependencies: Vec::new(),
            checklist: Vec::new(),
            tags: Vec::new(),
            workflow_metadata: orchestrator_core::WorkflowMetadata::default(),
            worktree_path: None,
            branch_name: None,
            metadata: orchestrator_core::TaskMetadata {
                created_at: now,
                updated_at: now,
                created_by: "test".to_string(),
                updated_by: "test".to_string(),
                started_at: None,
                completed_at: None,
                status_changed_at: None,
                version: 1,
            },
            deadline: None,
            paused: false,
            cancelled: false,
            resolution: None,
            resource_requirements: orchestrator_core::ResourceRequirements::default(),
            consecutive_dispatch_failures: None,
            last_dispatch_failure_at: None,
            dispatch_history: Vec::new(),
        }
    }

    #[test]
    fn blocked_transition_carries_blocked_reason() {
        let before = vec![task("TASK-1", TaskStatus::InProgress, None)];
        let after = vec![task("TASK-1", TaskStatus::Blocked, Some("workflow runner failed: phase impl exited 1"))];

        let changes = collect_task_state_changes(&before, &after, &[]);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].task_id, "TASK-1");
        assert_eq!(changes[0].from_status, "in-progress");
        assert_eq!(changes[0].to_status, "blocked");
        assert_eq!(changes[0].blocked_reason.as_deref(), Some("workflow runner failed: phase impl exited 1"));
    }

    #[test]
    fn non_blocked_transition_has_no_blocked_reason() {
        // A stale blocked_reason left on the task record must not leak into
        // transitions that do not land on Blocked.
        let before = vec![task("TASK-2", TaskStatus::Blocked, Some("old reason"))];
        let after = vec![task("TASK-2", TaskStatus::Ready, Some("old reason"))];

        let changes = collect_task_state_changes(&before, &after, &[]);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].to_status, "ready");
        assert!(changes[0].blocked_reason.is_none());
    }

    #[test]
    fn unchanged_status_emits_no_transition() {
        // Blocked-state reconciliation re-runs every tick for completed
        // workflows; an already-blocked task must not re-emit (and thus not
        // re-notify) on subsequent ticks.
        let before = vec![task("TASK-3", TaskStatus::Blocked, Some("reason"))];
        let after = vec![task("TASK-3", TaskStatus::Blocked, Some("reason"))];

        assert!(collect_task_state_changes(&before, &after, &[]).is_empty());
    }
}
