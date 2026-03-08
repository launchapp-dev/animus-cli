use std::sync::Arc;

use orchestrator_core::{
    project_task_execution_fact, project_task_status, services::ServiceHub, TaskStatus,
    WorkflowStatus,
};
use orchestrator_daemon_runtime::remove_terminal_dispatch_queue_entry_non_fatal;
use protocol::SubjectExecutionFact;

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

    if matches!(
        workflow_status,
        WorkflowStatus::Completed
            | WorkflowStatus::Failed
            | WorkflowStatus::Escalated
            | WorkflowStatus::Cancelled
    ) {
        remove_terminal_dispatch_queue_entry_non_fatal(
            project_root,
            task_id,
            workflow_ref.as_deref(),
            workflow_id,
        );
    }

    match workflow_status {
        WorkflowStatus::Completed => {
            project_task_execution_fact(
                hub,
                project_root,
                &SubjectExecutionFact {
                    subject_id: task_id.to_string(),
                    task_id: Some(task_id.to_string()),
                    workflow_id: workflow_id.map(ToOwned::to_owned),
                    workflow_ref,
                    workflow_status: Some(workflow_status),
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
            project_task_execution_fact(
                hub,
                project_root,
                &SubjectExecutionFact {
                    subject_id: task_id.to_string(),
                    task_id: Some(task_id.to_string()),
                    workflow_id: workflow_id.map(ToOwned::to_owned),
                    workflow_ref,
                    workflow_status: Some(workflow_status),
                    schedule_id: None,
                    exit_code: None,
                    success: false,
                    failure_reason: Some(format!(
                        "workflow ended with status {}",
                        format!("{workflow_status:?}").to_ascii_lowercase()
                    )),
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
