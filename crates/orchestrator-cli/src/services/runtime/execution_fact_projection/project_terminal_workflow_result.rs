use std::sync::Arc;

use orchestrator_core::{
    project_task_terminal_workflow_status, services::ServiceHub, WorkflowStatus,
};
use orchestrator_daemon_runtime::remove_terminal_dispatch_queue_entry_non_fatal;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn project_terminal_workflow_result(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    subject_id: &str,
    task_id: Option<&str>,
    workflow_ref: Option<&str>,
    workflow_id: Option<&str>,
    workflow_status: WorkflowStatus,
    failure_reason: Option<&str>,
) {
    if !matches!(
        workflow_status,
        WorkflowStatus::Completed
            | WorkflowStatus::Failed
            | WorkflowStatus::Escalated
            | WorkflowStatus::Cancelled
    ) {
        return;
    }

    remove_terminal_dispatch_queue_entry_non_fatal(
        project_root,
        subject_id,
        workflow_ref,
        workflow_id,
    );

    let Some(task_id) = task_id.filter(|task_id| !task_id.trim().is_empty()) else {
        return;
    };

    project_task_terminal_workflow_status(
        hub,
        task_id,
        workflow_status,
        failure_reason.map(ToOwned::to_owned),
    )
    .await;
}
