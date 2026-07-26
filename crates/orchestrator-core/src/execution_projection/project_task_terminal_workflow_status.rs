use crate::{TaskStatus, WorkflowStatus};

use super::{project_task_blocked_with_reason, project_task_status, TaskProjectionStore};

fn default_failure_reason(workflow_status: WorkflowStatus) -> String {
    format!("workflow ended with status {}", format!("{workflow_status:?}").to_ascii_lowercase())
}

pub async fn project_task_terminal_workflow_status(
    store: &dyn TaskProjectionStore,
    task_id: &str,
    workflow_status: WorkflowStatus,
    failure_reason: Option<String>,
) {
    if !matches!(
        workflow_status,
        WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Escalated | WorkflowStatus::Cancelled
    ) {
        return;
    }

    match workflow_status {
        WorkflowStatus::Completed => {
            // Task completion is intentionally NOT automatic.
            // Only an agent (via MCP tool) or a human (via CLI) should mark a task done
            // after verifying the work actually landed. Workflow completion means the
            // workflow ran — not that the task is finished.
        }
        WorkflowStatus::Failed | WorkflowStatus::Escalated => {
            let reason = failure_reason.unwrap_or_else(|| default_failure_reason(workflow_status));

            if store.get(task_id).await.is_ok() {
                let _ = project_task_blocked_with_reason(store, task_id, reason, None).await;
            } else {
                let _ = project_task_status(store, task_id, TaskStatus::Blocked).await;
            }
        }
        WorkflowStatus::Cancelled => {
            if let Ok(view) = store.get(task_id).await {
                if !matches!(view.status, TaskStatus::Done | TaskStatus::Cancelled) {
                    let _ = project_task_status(store, task_id, TaskStatus::Cancelled).await;
                }
            } else {
                let _ = project_task_status(store, task_id, TaskStatus::Cancelled).await;
            }
        }
        WorkflowStatus::Paused | WorkflowStatus::Running | WorkflowStatus::Pending => {}
    }
}
