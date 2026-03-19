use super::*;
use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;
use crate::services::runtime::workflow_mutation_surface::cancel_orphaned_running_workflow;
use anyhow::Result;
use orchestrator_core::{
    active_workflow_runner_ids, dispatch_workflow_event, load_agent_runtime_config_or_default, services::ServiceHub,
    WorkflowEvent, WorkflowMachineState, WorkflowStatus,
};
use std::collections::HashSet;
use std::path::Path;

pub async fn recover_orphaned_running_workflows(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_subject_ids: &HashSet<String>,
) -> usize {
    let workflows = match hub.workflows().list().await {
        Ok(workflows) => workflows,
        Err(error) => {
            eprintln!("{}: failed to list workflows for orphan recovery: {}", protocol::ACTOR_DAEMON, error);
            return 0;
        }
    };
    let externally_active_workflows = match active_workflow_runner_ids(Path::new(project_root)) {
        Ok(ids) => ids,
        Err(error) => {
            eprintln!("{}: failed to read active workflow runner ids: {}", protocol::ACTOR_DAEMON, error);
            HashSet::new()
        }
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
            || externally_active_workflows.contains(&workflow.id)
            || active_subject_ids.contains(workflow.subject.id())
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
        let cancelled = cancel_orphaned_running_workflow(hub.clone(), project_root, &workflow).await;
        if cancelled {
            recovered = recovered.saturating_add(1);
        } else {
            eprintln!("{}: failed to cancel orphaned workflow {}", protocol::ACTOR_DAEMON, workflow.id);
        }
    }

    recovered
}

pub async fn reconcile_manual_phase_timeouts(hub: Arc<dyn ServiceHub>, project_root: &str) -> Result<usize> {
    let runtime = load_agent_runtime_config_or_default(Path::new(project_root));
    let workflows = match hub.workflows().list().await {
        Ok(workflows) => workflows,
        Err(error) => {
            eprintln!(
                "{}: failed to list workflows for manual phase timeout reconciliation: {}",
                protocol::ACTOR_DAEMON,
                error
            );
            return Ok(0);
        }
    };
    let mut reconciled = 0usize;
    let now = chrono::Utc::now();

    for workflow in workflows {
        if workflow.status != WorkflowStatus::Paused {
            continue;
        }

        let phase_id = workflow
            .current_phase
            .clone()
            .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
            .unwrap_or_default();
        if phase_id.is_empty() {
            continue;
        }

        let definition = match runtime.phase_execution(&phase_id) {
            Some(definition) => definition,
            None => continue,
        };
        if !matches!(definition.mode, orchestrator_core::PhaseExecutionMode::Manual) {
            continue;
        }
        let manual = match definition.manual.as_ref() {
            Some(manual) => manual,
            None => continue,
        };
        let timeout_secs = match manual.timeout_secs {
            Some(timeout_secs) => timeout_secs,
            None => continue,
        };
        if timeout_secs == 0 {
            continue;
        }

        let started_at = workflow
            .phases
            .get(workflow.current_phase_index)
            .and_then(|phase| phase.started_at)
            .or(Some(workflow.started_at));
        let Some(started_at) = started_at else {
            continue;
        };
        let elapsed = now.signed_duration_since(started_at).num_seconds().max(0) as u64;
        if elapsed < timeout_secs {
            continue;
        }

        let reason = format!("manual phase '{}' timed out after {} seconds", phase_id, timeout_secs);
        let outcome = dispatch_workflow_event(
            hub.clone(),
            project_root,
            WorkflowEvent::RejectManualPhase {
                workflow_id: workflow.id.clone(),
                phase_id: phase_id.clone(),
                note: Some(reason.clone()),
            },
        )
        .await?;
        if let Some(updated) = outcome.workflow {
            project_terminal_workflow_result(
                hub.clone(),
                project_root,
                updated.subject.id(),
                Some(updated.task_id.as_str()),
                updated.workflow_ref.as_deref(),
                Some(updated.id.as_str()),
                updated.status,
                updated.failure_reason.as_deref(),
            )
            .await;
        }
        reconciled = reconciled.saturating_add(1);
    }

    Ok(reconciled)
}

fn workflow_is_waiting_on_manual_phase(project_root: &str, workflow: &orchestrator_core::OrchestratorWorkflow) -> bool {
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

pub async fn reconcile_runner_blocked_tasks(
    hub: Arc<dyn ServiceHub>,
    _project_root: &str,
) -> Result<usize> {
    let tasks = match hub.tasks().list().await {
        Ok(tasks) => tasks,
        Err(error) => {
            eprintln!(
                "{}: failed to list tasks for runner-blocked reconciliation: {}",
                protocol::ACTOR_DAEMON,
                error
            );
            return Ok(0);
        }
    };

    let mut reconciled = 0usize;
    for task in tasks {
        if !orchestrator_core::is_workflow_runner_blocked(&task) {
            continue;
        }
        match orchestrator_core::reconcile_runner_blocked_task(hub.clone(), &task).await {
            Ok(true) => {
                reconciled = reconciled.saturating_add(1);
            }
            Ok(false) => {
                // Escalated to human review — task left blocked
            }
            Err(error) => {
                eprintln!(
                    "{}: failed to reconcile runner-blocked task {}: {}",
                    protocol::ACTOR_DAEMON,
                    task.id,
                    error
                );
            }
        }
    }

    Ok(reconciled)
}
