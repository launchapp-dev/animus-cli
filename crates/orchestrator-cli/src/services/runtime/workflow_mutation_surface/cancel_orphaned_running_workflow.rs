use std::sync::Arc;

use orchestrator_core::{dispatch_workflow_event, services::ServiceHub, OrchestratorWorkflow, WorkflowEvent};

use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;

pub(crate) async fn cancel_orphaned_running_workflow(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    workflow: &OrchestratorWorkflow,
) -> bool {
    let task_store = orchestrator_daemon_runtime::resolve_task_projection_store(project_root, hub.clone()).await;
    let outcome = match dispatch_workflow_event(
        hub.clone(),
        task_store.as_ref(),
        project_root,
        WorkflowEvent::Cancel { workflow_id: workflow.id.clone() },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => return false,
    };
    let updated = match outcome.workflow {
        Some(workflow) => workflow,
        None => return false,
    };

    project_terminal_workflow_result(
        hub,
        project_root,
        updated.subject.as_ref().map(|s| s.id()).unwrap_or_default(),
        Some(updated.task_id.as_str()),
        updated.workflow_ref.as_deref(),
        Some(updated.id.as_str()),
        orchestrator_core::WorkflowStatus::Cancelled,
        updated.failure_reason.as_deref(),
    )
    .await;
    true
}
