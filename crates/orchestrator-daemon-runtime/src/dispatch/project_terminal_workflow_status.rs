use std::sync::Arc;

use orchestrator_core::{
    project_task_execution_fact, project_task_status, services::ServiceHub, TaskStatus,
    WorkflowStatus,
};
use protocol::SubjectExecutionFact;

use crate::remove_terminal_dispatch_queue_entry_non_fatal;

pub async fn project_terminal_workflow_status(
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

    match workflow_status {
        WorkflowStatus::Completed => {
            project_task_execution_fact(
                hub,
                project_root,
                &SubjectExecutionFact {
                    subject_id: subject_id.to_string(),
                    task_id: Some(task_id.to_string()),
                    workflow_ref: workflow_ref.map(ToOwned::to_owned),
                    schedule_id: None,
                    exit_code: Some(0),
                    success: true,
                    failure_reason: None,
                    runner_events: Vec::new(),
                },
            )
            .await;
        }
        WorkflowStatus::Failed | WorkflowStatus::Escalated => {
            let failure_reason = failure_reason.map(ToOwned::to_owned).unwrap_or_else(|| {
                format!(
                    "workflow ended with status {}",
                    format!("{workflow_status:?}").to_ascii_lowercase()
                )
            });

            project_task_execution_fact(
                hub,
                project_root,
                &SubjectExecutionFact {
                    subject_id: subject_id.to_string(),
                    task_id: Some(task_id.to_string()),
                    workflow_ref: workflow_ref.map(ToOwned::to_owned),
                    schedule_id: None,
                    exit_code: None,
                    success: false,
                    failure_reason: Some(failure_reason),
                    runner_events: Vec::new(),
                },
            )
            .await;
        }
        WorkflowStatus::Cancelled => {
            let _ = project_task_status(hub, task_id, TaskStatus::Cancelled).await;
        }
        WorkflowStatus::Paused | WorkflowStatus::Running | WorkflowStatus::Pending => {}
    }
}
