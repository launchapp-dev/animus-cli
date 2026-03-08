use std::sync::Arc;

use orchestrator_core::{services::ServiceHub, OrchestratorWorkflow};

use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;

pub(crate) async fn cancel_orphaned_running_workflow(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    workflow: &OrchestratorWorkflow,
) -> bool {
    if hub.workflows().cancel(&workflow.id).await.is_err() {
        return false;
    }

    project_terminal_workflow_result(
        hub,
        project_root,
        workflow.subject.id(),
        Some(workflow.task_id.as_str()),
        workflow.workflow_ref.as_deref(),
        Some(workflow.id.as_str()),
        orchestrator_core::WorkflowStatus::Cancelled,
        workflow.failure_reason.as_deref(),
    )
    .await;
    true
}
