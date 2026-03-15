use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_core::{
    project_schedule_dispatch_attempt, services::ServiceHub, DaemonStatus, DaemonTickMetrics, FileServiceHub,
    OrchestratorTask,
};
use serde_json::Value;

use crate::{
    CompletedProcess, DaemonRuntimeOptions, DispatchNotice, DispatchWorkflowStart, DispatchWorkflowStartSummary,
    ProcessManager, ProjectTickHooks, ProjectTickSnapshot, ProjectTickSummary, ProjectTickSummaryInput,
    ScheduleDispatch, TaskStateChangeEvent, TickSummaryBuilder,
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
    ) -> Result<(usize, usize)>;

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

    async fn dispatch_ready_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
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
    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>) {
        let outcomes = ScheduleDispatch::process_due_schedules(root, now, |schedule_id, dispatch| {
            match self.process_manager.spawn_workflow_runner(dispatch, root) {
                Ok(()) => {
                    self.services.dispatch_notice(DispatchNotice::ScheduleDispatched {
                        schedule_id: schedule_id.to_string(),
                        dispatch: dispatch.clone(),
                    });
                    Ok(())
                }
                Err(error) => {
                    self.services.dispatch_notice(DispatchNotice::ScheduleDispatchFailed {
                        schedule_id: schedule_id.to_string(),
                        dispatch: dispatch.clone(),
                        error: error.to_string(),
                    });
                    Err(error)
                }
            }
        });
        for outcome in outcomes {
            self.services.record_schedule_dispatch_attempt(root, &outcome.schedule_id, now, &outcome.status);
        }
    }

    async fn capture_snapshot(&mut self, root: &str) -> Result<ProjectTickSnapshot> {
        self.services.capture_snapshot(root).await
    }

    async fn reconcile_completed_processes(&mut self, root: &str) -> Result<(usize, usize)> {
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

    async fn dispatch_ready_tasks(&mut self, root: &str, limit: usize) -> Result<DispatchWorkflowStartSummary> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        self.services.dispatch_ready_tasks(hub, root, limit, Some(self.process_manager)).await
    }

    async fn collect_health(&mut self, root: &str) -> Result<Value> {
        self.services.collect_health(root).await
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
