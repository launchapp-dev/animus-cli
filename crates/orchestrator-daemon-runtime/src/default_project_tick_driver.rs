use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_core::{services::ServiceHub, FileServiceHub};
use tokio::process::Command as TokioCommand;

use crate::{
    CompletedProcess, HookBackedProjectTickDriver, ProcessManager, ProjectTickHooks,
    ReadyTaskWorkflowStartSummary, ScheduleDispatch, WorkflowSubjectArgs,
};

#[async_trait::async_trait(?Send)]
pub trait DefaultProjectTickServices {
    fn flush_git_outbox(&mut self, root: &str);

    async fn bootstrap_from_vision(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        startup_cleanup: bool,
        ai_task_generation: bool,
    ) -> Result<()>;

    async fn ensure_ai_generated_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()>;

    async fn resume_interrupted(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<(usize, usize)>;

    async fn recover_orphaned_running_workflows(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        active_subject_ids: &HashSet<String>,
    ) -> Result<()>;

    async fn reconcile_stale_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        stale_threshold_hours: u64,
    ) -> Result<usize>;

    async fn reconcile_dependency_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<usize>;

    async fn reconcile_merge_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<usize>;

    async fn reconcile_completed_processes(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        completed_processes: Vec<CompletedProcess>,
    ) -> Result<(usize, usize)>;

    async fn retry_failed_task_workflows(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()>;

    async fn promote_backlog_tasks_to_ready(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()>;

    async fn dispatch_ready_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
        process_manager: Option<&mut ProcessManager>,
    ) -> Result<ReadyTaskWorkflowStartSummary>;

    async fn refresh_runtime_binaries(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()>;
}

pub type DefaultFullProjectTickDriver<S> =
    HookBackedProjectTickDriver<DefaultFullProjectTickHooks<S>>;
pub type DefaultSlimProjectTickDriver<'a, S> =
    HookBackedProjectTickDriver<DefaultSlimProjectTickHooks<'a, S>>;

pub fn default_full_project_tick_driver<S>(services: S) -> DefaultFullProjectTickDriver<S>
where
    S: DefaultProjectTickServices,
{
    HookBackedProjectTickDriver::new(DefaultFullProjectTickHooks {
        services,
        schedule_process_manager: ProcessManager::new(),
    })
}

pub fn default_slim_project_tick_driver<'a, S>(
    services: S,
    process_manager: &'a mut ProcessManager,
) -> DefaultSlimProjectTickDriver<'a, S>
where
    S: DefaultProjectTickServices,
{
    HookBackedProjectTickDriver::new(DefaultSlimProjectTickHooks {
        services,
        process_manager,
    })
}

pub struct DefaultFullProjectTickHooks<S> {
    services: S,
    schedule_process_manager: ProcessManager,
}

pub struct DefaultSlimProjectTickHooks<'a, S> {
    services: S,
    process_manager: &'a mut ProcessManager,
}

fn spawn_schedule_pipeline(
    process_manager: &mut ProcessManager,
    project_root: &str,
    schedule_id: &str,
    pipeline_id: &str,
    input_json: Option<&str>,
) -> Result<()> {
    let subject = WorkflowSubjectArgs::Custom {
        title: format!("schedule:{schedule_id}"),
        description: Some(format!("Triggered by schedule '{schedule_id}'")),
        input_json: input_json.map(String::from),
    };
    process_manager.spawn_workflow_runner(&subject, pipeline_id, project_root)?;

    eprintln!(
        "{}: schedule '{}' fired pipeline '{}'",
        protocol::ACTOR_DAEMON,
        schedule_id,
        pipeline_id
    );
    Ok(())
}

fn spawn_schedule_command(project_root: &str, schedule_id: &str, command: &str) -> Result<()> {
    let mut child = TokioCommand::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(project_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(anyhow::Error::from)?;

    eprintln!(
        "{}: schedule '{}' fired command: {}",
        protocol::ACTOR_DAEMON,
        schedule_id,
        command
    );

    let root = project_root.to_string();
    let sched_id = schedule_id.to_string();
    tokio::spawn(async move {
        let status = match child.wait().await {
            Ok(exit) if exit.success() => "completed",
            Ok(_) => "failed",
            Err(_) => "failed",
        };
        ScheduleDispatch::update_completion_state(&root, &sched_id, status);
    });

    Ok(())
}

#[async_trait::async_trait(?Send)]
impl<S> ProjectTickHooks for DefaultFullProjectTickHooks<S>
where
    S: DefaultProjectTickServices,
{
    fn build_hub(&mut self, root: &str) -> Result<Arc<dyn ServiceHub>> {
        Ok(Arc::new(FileServiceHub::new(root)?))
    }

    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>) {
        ScheduleDispatch::process_due_schedules(
            root,
            now,
            |schedule_id, pipeline_id, input_json| {
                spawn_schedule_pipeline(
                    &mut self.schedule_process_manager,
                    root,
                    schedule_id,
                    pipeline_id,
                    input_json,
                )
            },
            |schedule_id, command| spawn_schedule_command(root, schedule_id, command),
        );
    }

    fn flush_git_outbox(&mut self, root: &str) {
        self.services.flush_git_outbox(root);
    }

    async fn bootstrap_from_vision(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        startup_cleanup: bool,
        ai_task_generation: bool,
    ) -> Result<()> {
        self.services
            .bootstrap_from_vision(hub, root, startup_cleanup, ai_task_generation)
            .await
    }

    async fn ensure_ai_generated_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        self.services.ensure_ai_generated_tasks(hub, root).await
    }

    async fn resume_interrupted(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<(usize, usize)> {
        self.services.resume_interrupted(hub, root).await
    }

    async fn recover_orphaned_running_workflows(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        self.services
            .recover_orphaned_running_workflows(hub, root, &HashSet::new())
            .await
    }

    async fn reconcile_stale_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        stale_threshold_hours: u64,
    ) -> Result<usize> {
        self.services
            .reconcile_stale_tasks(hub, root, stale_threshold_hours)
            .await
    }

    async fn reconcile_dependency_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<usize> {
        self.services.reconcile_dependency_tasks(hub, root).await
    }

    async fn reconcile_merge_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<usize> {
        self.services.reconcile_merge_tasks(hub, root).await
    }

    async fn retry_failed_task_workflows(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        self.services.retry_failed_task_workflows(hub, root).await
    }

    async fn promote_backlog_tasks_to_ready(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        self.services
            .promote_backlog_tasks_to_ready(hub, root)
            .await
    }

    async fn dispatch_ready_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
    ) -> Result<ReadyTaskWorkflowStartSummary> {
        self.services
            .dispatch_ready_tasks(hub, root, limit, None)
            .await
    }

    async fn refresh_runtime_binaries(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        self.services.refresh_runtime_binaries(hub, root).await
    }
}

#[async_trait::async_trait(?Send)]
impl<S> ProjectTickHooks for DefaultSlimProjectTickHooks<'_, S>
where
    S: DefaultProjectTickServices,
{
    fn build_hub(&mut self, root: &str) -> Result<Arc<dyn ServiceHub>> {
        Ok(Arc::new(FileServiceHub::new(root)?))
    }

    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>) {
        ScheduleDispatch::process_due_schedules(
            root,
            now,
            |schedule_id, pipeline_id, input_json| {
                spawn_schedule_pipeline(
                    self.process_manager,
                    root,
                    schedule_id,
                    pipeline_id,
                    input_json,
                )
            },
            |schedule_id, command| spawn_schedule_command(root, schedule_id, command),
        );
    }

    fn flush_git_outbox(&mut self, root: &str) {
        self.services.flush_git_outbox(root);
    }

    fn active_process_count(&self) -> usize {
        self.process_manager.active_count()
    }

    fn emit_notice(&mut self, message: &str) {
        eprintln!("{}", message);
    }

    async fn bootstrap_from_vision(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        startup_cleanup: bool,
        ai_task_generation: bool,
    ) -> Result<()> {
        self.services
            .bootstrap_from_vision(hub, root, startup_cleanup, ai_task_generation)
            .await
    }

    async fn ensure_ai_generated_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        self.services.ensure_ai_generated_tasks(hub, root).await
    }

    async fn resume_interrupted(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<(usize, usize)> {
        self.services.resume_interrupted(hub, root).await
    }

    async fn recover_orphaned_running_workflows(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        let active_subject_ids = self.process_manager.active_subject_ids();
        self.services
            .recover_orphaned_running_workflows(hub, root, &active_subject_ids)
            .await
    }

    async fn reconcile_stale_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        stale_threshold_hours: u64,
    ) -> Result<usize> {
        self.services
            .reconcile_stale_tasks(hub, root, stale_threshold_hours)
            .await
    }

    async fn reconcile_dependency_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<usize> {
        self.services.reconcile_dependency_tasks(hub, root).await
    }

    async fn reconcile_merge_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<usize> {
        self.services.reconcile_merge_tasks(hub, root).await
    }

    async fn reconcile_completed_processes(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<(usize, usize)> {
        let completed_processes = self.process_manager.check_running();
        self.services
            .reconcile_completed_processes(hub, root, completed_processes)
            .await
    }

    async fn retry_failed_task_workflows(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        self.services.retry_failed_task_workflows(hub, root).await
    }

    async fn promote_backlog_tasks_to_ready(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        self.services
            .promote_backlog_tasks_to_ready(hub, root)
            .await
    }

    async fn dispatch_ready_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
    ) -> Result<ReadyTaskWorkflowStartSummary> {
        self.services
            .dispatch_ready_tasks(hub, root, limit, Some(self.process_manager))
            .await
    }

    async fn refresh_runtime_binaries(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
    ) -> Result<()> {
        self.services.refresh_runtime_binaries(hub, root).await
    }
}
