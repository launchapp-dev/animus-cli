use super::*;
use crate::services::runtime::workflow_mutation_surface::cancel_orphaned_running_workflow;
use orchestrator_core::{services::ServiceHub, WorkflowMachineState, WorkflowStatus};
use std::collections::HashSet;
use std::path::Path;

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
        if workflow_is_waiting_on_manual_phase(project_root, &workflow) {
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

fn workflow_is_waiting_on_manual_phase(
    project_root: &str,
    workflow: &orchestrator_core::OrchestratorWorkflow,
) -> bool {
    let Some(phase_id) = workflow.current_phase.clone().or_else(|| {
        workflow
            .phases
            .get(workflow.current_phase_index)
            .map(|phase| phase.phase_id.clone())
    }) else {
        return false;
    };

    orchestrator_core::load_agent_runtime_config(Path::new(project_root))
        .ok()
        .and_then(|config| config.phase_execution(&phase_id).cloned())
        .map(|definition| {
            matches!(
                definition.mode,
                orchestrator_core::PhaseExecutionMode::Manual
            )
        })
        .unwrap_or(false)
}
