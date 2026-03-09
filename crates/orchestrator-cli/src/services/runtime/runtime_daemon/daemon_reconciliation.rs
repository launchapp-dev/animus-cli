use super::*;
use crate::services::runtime::workflow_mutation_surface::cancel_orphaned_running_workflow;
use orchestrator_core::{services::ServiceHub, WorkflowMachineState, WorkflowStatus};
use std::collections::HashSet;

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
        let _ = cancel_orphaned_running_workflow(hub.clone(), project_root, &workflow).await;
        recovered = recovered.saturating_add(1);
    }

    recovered
}
