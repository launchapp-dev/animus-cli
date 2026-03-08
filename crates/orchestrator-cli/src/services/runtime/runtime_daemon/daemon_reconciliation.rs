use super::*;
use orchestrator_core::{services::ServiceHub, WorkflowMachineState, WorkflowStatus};
use orchestrator_daemon_runtime::remove_terminal_dispatch_queue_entry_non_fatal;
use std::path::Path;

pub async fn recover_orphaned_running_workflows_on_startup(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> usize {
    let workflows = match hub.workflows().list().await {
        Ok(w) => w,
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

        eprintln!(
            "{}: startup orphan detection — cancelling orphaned workflow {} (task {})",
            protocol::ACTOR_DAEMON,
            workflow.id,
            workflow.task_id
        );
        if let Ok(_updated) = hub.workflows().cancel(&workflow.id).await {
            remove_terminal_dispatch_queue_entry_non_fatal(
                project_root,
                workflow.subject.id(),
                workflow.workflow_ref.as_deref(),
                Some(workflow.id.as_str()),
            );
        }
        recovered = recovered.saturating_add(1);
    }

    recovered
}

fn workflow_is_waiting_on_manual_phase(
    project_root: &str,
    workflow: &orchestrator_core::OrchestratorWorkflow,
) -> bool {
    let Some(phase_id) = workflow
        .current_phase
        .clone()
        .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
    else {
        return false;
    };

    orchestrator_core::load_agent_runtime_config(Path::new(project_root))
        .ok()
        .and_then(|config| config.phase_execution(&phase_id).cloned())
        .map(|definition| matches!(definition.mode, orchestrator_core::PhaseExecutionMode::Manual))
        .unwrap_or(false)
}
