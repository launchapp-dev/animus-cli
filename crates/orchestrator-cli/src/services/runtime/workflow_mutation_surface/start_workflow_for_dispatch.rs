use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::{project_task_workflow_start, services::ServiceHub, WorkflowSubject};

use super::daemon_workflow_assignment;

pub(crate) async fn start_workflow_for_dispatch(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    dispatch: &protocol::SubjectDispatch,
) -> Result<orchestrator_core::OrchestratorWorkflow> {
    let workflow = hub
        .workflows()
        .run(dispatch.to_workflow_run_input())
        .await?;

    if let WorkflowSubject::Task { id } = &dispatch.subject {
        let task = hub.tasks().get(id).await?;
        let (role, model) = daemon_workflow_assignment(project_root, &workflow, &task);
        project_task_workflow_start(hub, id, role, model, protocol::ACTOR_DAEMON.to_string())
            .await?;
    }

    Ok(workflow)
}
