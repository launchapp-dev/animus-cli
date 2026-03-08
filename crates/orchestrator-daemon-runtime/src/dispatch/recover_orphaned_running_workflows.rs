use std::collections::HashSet;
use std::sync::Arc;

use orchestrator_core::{services::ServiceHub, WorkflowMachineState, WorkflowStatus};

use crate::project_terminal_workflow_status;

pub async fn recover_orphaned_running_workflows(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_subject_ids: &HashSet<String>,
) -> usize {
    let workflows = match hub.workflows().list().await {
        Ok(workflows) => workflows,
        Err(_) => return 0,
    };

    let mut recovered = 0usize;
    for workflow in workflows {
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        if workflow.machine_state == WorkflowMachineState::MergeConflict {
            continue;
        }
        if active_subject_ids.contains(&workflow.id)
            || active_subject_ids.contains(workflow.subject.id())
            || (!workflow.task_id.is_empty() && active_subject_ids.contains(&workflow.task_id))
        {
            continue;
        }

        eprintln!(
            "{}: recovering orphaned running workflow {} subject={} task={}",
            protocol::ACTOR_DAEMON,
            workflow.id,
            workflow.subject.id(),
            workflow.task_id
        );
        if let Ok(_updated) = hub.workflows().cancel(&workflow.id).await {
            project_terminal_workflow_status(
                hub.clone(),
                project_root,
                workflow.subject.id(),
                Some(workflow.task_id.as_str()),
                workflow.workflow_ref.as_deref(),
                Some(workflow.id.as_str()),
                WorkflowStatus::Cancelled,
                workflow.failure_reason.as_deref(),
            )
            .await;
        }
        recovered = recovered.saturating_add(1);
    }

    recovered
}
