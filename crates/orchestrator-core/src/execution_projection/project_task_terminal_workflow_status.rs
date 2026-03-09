use std::sync::Arc;

use crate::{services::ServiceHub, TaskStatus, WorkflowStatus};

use super::{project_task_blocked_with_reason, project_task_status};

fn default_failure_reason(workflow_status: WorkflowStatus) -> String {
    format!(
        "workflow ended with status {}",
        format!("{workflow_status:?}").to_ascii_lowercase()
    )
}

pub async fn project_task_terminal_workflow_status(
    hub: Arc<dyn ServiceHub>,
    task_id: &str,
    workflow_status: WorkflowStatus,
    failure_reason: Option<String>,
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

    match workflow_status {
        WorkflowStatus::Completed => {
            let _ = project_task_status(hub, task_id, TaskStatus::Done).await;
        }
        WorkflowStatus::Failed | WorkflowStatus::Escalated => {
            let reason = failure_reason.unwrap_or_else(|| default_failure_reason(workflow_status));

            if let Ok(task) = hub.tasks().get(task_id).await {
                let _ = project_task_blocked_with_reason(hub, &task, reason, None).await;
            } else {
                let _ = project_task_status(hub, task_id, TaskStatus::Blocked).await;
            }
        }
        WorkflowStatus::Cancelled => {
            let _ = project_task_status(hub, task_id, TaskStatus::Cancelled).await;
        }
        WorkflowStatus::Paused | WorkflowStatus::Running | WorkflowStatus::Pending => {}
    }
}
