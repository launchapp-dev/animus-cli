use std::sync::Arc;

use super::runtime_daemon::daemon_reconciliation::project_terminal_workflow_status;
use orchestrator_core::{services::ServiceHub, WorkflowStatus};

pub(crate) async fn sync_task_status_for_workflow_result(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task_id: &str,
    workflow_status: WorkflowStatus,
    workflow_id: Option<&str>,
) {
    let workflow_ref = if let Some(id) = workflow_id {
        hub.workflows()
            .get(id)
            .await
            .ok()
            .and_then(|workflow| workflow.workflow_ref)
    } else {
        None
    };

    project_terminal_workflow_status(
        hub,
        project_root,
        task_id,
        Some(task_id),
        workflow_ref.as_deref(),
        workflow_id,
        workflow_status,
        None,
    )
    .await;
}
