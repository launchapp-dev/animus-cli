use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_core::{services::ServiceHub, FileServiceHub};
use orchestrator_daemon_runtime::{
    CompletedProcess, HookBackedProjectTickDriver, ProcessManager, ProjectTickHooks,
    ReadyTaskWorkflowStartSummary, ScheduleDispatch, SubjectDispatch,
};
use tokio::process::Command as TokioCommand;

#[async_trait::async_trait(?Send)]
pub trait DefaultProjectTickServices {
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
    ) -> Result<ReadyTaskWorkflowStartSummary>;
}

pub type DefaultSlimProjectTickDriver<'a, S> =
    HookBackedProjectTickDriver<DefaultSlimProjectTickHooks<'a, S>>;

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

pub struct DefaultSlimProjectTickHooks<'a, S> {
    services: S,
    process_manager: &'a mut ProcessManager,
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
            |schedule_id, dispatch| {
                spawn_schedule_pipeline(self.process_manager, root, schedule_id, dispatch)
            },
            |schedule_id, command| spawn_schedule_command(root, schedule_id, command),
        );
    }

    fn active_process_count(&self) -> usize {
        self.process_manager.active_count()
    }

    fn emit_notice(&mut self, message: &str) {
        eprintln!("{}", message);
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
}
