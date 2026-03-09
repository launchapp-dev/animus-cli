use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_core::{
    project_schedule_dispatch_attempt, services::ServiceHub, DaemonStatus, DaemonTickMetrics,
    FileServiceHub,
};
use orchestrator_daemon_runtime::{
    CompletedProcess, DaemonRuntimeOptions, DispatchWorkflowStartSummary, ProcessManager,
    ProjectTickHooks, ProjectTickSnapshot, ProjectTickSummary, ProjectTickSummaryInput,
    ScheduleDispatch, SubjectDispatch, TickSummaryBuilder,
};
use serde_json::Value;

#[async_trait::async_trait(?Send)]
pub trait DefaultProjectTickServices {
    async fn capture_snapshot(&mut self, root: &str) -> Result<ProjectTickSnapshot> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        let requirements_before = hub.planning().list_requirements().await.unwrap_or_default();
        let tasks_before = hub.tasks().list().await.unwrap_or_default();
        let daemon = hub.daemon();
        let status = daemon.status().await?;
        let mut started_daemon = false;
        if !matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
            daemon.start().await?;
            started_daemon = true;
        }
        let daemon_health = daemon.health().await.ok();

        Ok(ProjectTickSnapshot {
            requirements_before,
            tasks_before,
            started_daemon,
            daemon_health,
        })
    }

    async fn reconcile_completed_processes(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        completed_processes: Vec<CompletedProcess>,
    ) -> Result<(usize, usize)>;

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
        let metrics = DaemonTickMetrics::collect(hub, args.stale_threshold_hours).await?;
        TickSummaryBuilder::build(args, input, metrics)
    }
}

pub type DefaultSlimProjectTickDriver<'a, S> = DefaultSlimProjectTickHooks<'a, S>;

pub fn default_slim_project_tick_driver<'a, S>(
    services: S,
    process_manager: &'a mut ProcessManager,
) -> DefaultSlimProjectTickDriver<'a, S>
where
    S: DefaultProjectTickServices,
{
    DefaultSlimProjectTickHooks {
        services,
        process_manager,
    }
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

fn spawn_schedule_pipeline(
    process_manager: &mut ProcessManager,
    project_root: &str,
    schedule_id: &str,
    dispatch: &SubjectDispatch,
) -> Result<()> {
    process_manager.spawn_workflow_runner(dispatch, project_root)?;

    eprintln!(
        "{}: schedule '{}' fired workflow '{}'",
        protocol::ACTOR_DAEMON,
        schedule_id,
        dispatch.workflow_ref
    );
    Ok(())
}

#[async_trait::async_trait(?Send)]
impl<S> ProjectTickHooks for DefaultSlimProjectTickHooks<'_, S>
where
    S: DefaultProjectTickServices,
{
    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>) {
        let outcomes =
            ScheduleDispatch::process_due_schedules(root, now, |schedule_id, dispatch| {
                spawn_schedule_pipeline(self.process_manager, root, schedule_id, dispatch)
            });
        for outcome in outcomes {
            project_schedule_dispatch_attempt(root, &outcome.schedule_id, now, &outcome.status);
        }
    }

    async fn capture_snapshot(&mut self, root: &str) -> Result<ProjectTickSnapshot> {
        self.services.capture_snapshot(root).await
    }

    async fn reconcile_completed_processes(&mut self, root: &str) -> Result<(usize, usize)> {
        let completed_processes = self.process_manager.check_running();
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        self.services
            .reconcile_completed_processes(hub, root, completed_processes)
            .await
    }

    async fn dispatch_ready_tasks(
        &mut self,
        root: &str,
        limit: usize,
    ) -> Result<DispatchWorkflowStartSummary> {
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root)?);
        self.services
            .dispatch_ready_tasks(hub, root, limit, Some(self.process_manager))
            .await
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
