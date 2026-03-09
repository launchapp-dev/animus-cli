use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::{
    project_task_workflow_start, services::ServiceHub, OrchestratorTask, OrchestratorWorkflow,
    WorkflowRunInput,
};

use super::daemon_workflow_assignment;

pub(crate) async fn start_task_workflow_for_dispatch(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &OrchestratorTask,
    workflow_ref: &str,
) -> Result<OrchestratorWorkflow> {
    let workflow = hub
        .workflows()
        .run(WorkflowRunInput::for_task(
            task.id.clone(),
            Some(workflow_ref.to_string()),
        ))
        .await?;
    let (role, model) = daemon_workflow_assignment(project_root, &workflow, task);
    project_task_workflow_start(
        hub,
        &task.id,
        role,
        model,
        protocol::ACTOR_DAEMON.to_string(),
    )
    .await?;
    Ok(workflow)
}
