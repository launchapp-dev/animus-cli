use std::collections::HashSet;

use anyhow::{anyhow, Result};

use crate::types::{WorkflowMachineEvent, WorkflowMachineState};

use super::schema::{
    RegistryEntry, RequirementLifecycleDefinition, StateMachinesDocument, WorkflowMachineDefinition,
    STATE_MACHINES_SCHEMA_ID, STATE_MACHINES_VERSION,
};

const EXECUTOR_REQUIRED_WORKFLOW_TRANSITIONS: &[(WorkflowMachineState, WorkflowMachineEvent)] = &[
    (WorkflowMachineState::EvaluateTransition, WorkflowMachineEvent::PhaseStarted),
    (WorkflowMachineState::EvaluateTransition, WorkflowMachineEvent::NoMorePhases),
    (WorkflowMachineState::RunPhase, WorkflowMachineEvent::PhaseSucceeded),
    (WorkflowMachineState::RunPhase, WorkflowMachineEvent::PhaseFailed),
    (WorkflowMachineState::RunPhase, WorkflowMachineEvent::PhaseSkipped),
    (WorkflowMachineState::RunPhase, WorkflowMachineEvent::CancelRequested),
    (WorkflowMachineState::EvaluateGates, WorkflowMachineEvent::GatesPassed),
    (WorkflowMachineState::EvaluateGates, WorkflowMachineEvent::GatesFailed),
    (WorkflowMachineState::EvaluateGates, WorkflowMachineEvent::PolicyDecisionReady),
    (WorkflowMachineState::EvaluateGates, WorkflowMachineEvent::PhaseTargetSelected),
    (WorkflowMachineState::ApplyTransition, WorkflowMachineEvent::Start),
    (WorkflowMachineState::ApplyTransition, WorkflowMachineEvent::PhaseStarted),
    (WorkflowMachineState::ApplyTransition, WorkflowMachineEvent::NoMorePhases),
    (WorkflowMachineState::ApplyTransition, WorkflowMachineEvent::RetryPhaseStarted),
    // TODO(codex-p2): legacy custom state-machine JSON written before the
    // merge-conflict transitions existed now fails validation and falls back
    // to the builtin machine (with a logged warning). Consider a migration
    // shim if real configs surface without these two transitions.
    (WorkflowMachineState::Completed, WorkflowMachineEvent::MergeConflictDetected),
    (WorkflowMachineState::MergeConflict, WorkflowMachineEvent::MergeConflictResolved),
];

pub fn validate_state_machines_document(document: &StateMachinesDocument) -> Result<()> {
    let mut errors = Vec::new();

    if document.schema.trim() != STATE_MACHINES_SCHEMA_ID {
        errors.push(format!("schema must be '{}' (got '{}')", STATE_MACHINES_SCHEMA_ID, document.schema));
    }

    if document.version != STATE_MACHINES_VERSION {
        errors.push(format!("version must be {} (got {})", STATE_MACHINES_VERSION, document.version));
    }

    if let Err(error) = validate_workflow_machine_definition(&document.workflow) {
        errors.push(format!("workflow: {error}"));
    }

    if let Err(error) = validate_requirement_lifecycle_definition(&document.requirements_lifecycle) {
        errors.push(format!("requirements_lifecycle: {error}"));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

fn validate_workflow_machine_definition(definition: &WorkflowMachineDefinition) -> Result<()> {
    let mut errors = Vec::new();

    if definition.transitions.is_empty() {
        errors.push("transitions must not be empty".to_string());
    }

    if definition.terminal_states.is_empty() {
        errors.push("terminal_states must include at least one state".to_string());
    }

    let mut required_transitions = vec![(definition.initial_state, WorkflowMachineEvent::Start)];
    required_transitions.extend(EXECUTOR_REQUIRED_WORKFLOW_TRANSITIONS.iter().copied());
    for (from, event) in required_transitions {
        if !definition.transitions.iter().any(|transition| transition.from == from && transition.event == event) {
            errors.push(format!("missing executor-required transition from {from:?} on {event:?}"));
        }
    }

    let guard_ids = registry_ids(&definition.guards, "guards", &mut errors);
    let action_ids = registry_ids(&definition.actions, "actions", &mut errors);

    for transition in &definition.transitions {
        if let Some(guard_id) = transition.guard.as_deref() {
            if !guard_ids.contains(guard_id) {
                errors.push(format!(
                    "transition {:?} --{:?}--> {:?} references unknown guard '{}'",
                    transition.from, transition.event, transition.to, guard_id
                ));
            }
        }

        if let Some(action_id) = transition.action.as_deref() {
            if !action_ids.contains(action_id) {
                errors.push(format!(
                    "transition {:?} --{:?}--> {:?} references unknown action '{}'",
                    transition.from, transition.event, transition.to, action_id
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

fn validate_requirement_lifecycle_definition(definition: &RequirementLifecycleDefinition) -> Result<()> {
    let mut errors = Vec::new();

    if definition.transitions.is_empty() {
        errors.push("transitions must not be empty".to_string());
    }

    if definition.terminal_states.is_empty() {
        errors.push("terminal_states must include at least one state".to_string());
    }

    if definition.policy.max_rework_rounds == 0 {
        errors.push("policy.max_rework_rounds must be greater than 0".to_string());
    }

    let guard_ids = registry_ids(&definition.guards, "guards", &mut errors);
    let action_ids = registry_ids(&definition.actions, "actions", &mut errors);

    for transition in &definition.transitions {
        if let Some(guard_id) = transition.guard.as_deref() {
            if !guard_ids.contains(guard_id) {
                errors.push(format!(
                    "transition {:?} --{:?}--> {:?} references unknown guard '{}'",
                    transition.from, transition.event, transition.to, guard_id
                ));
            }
        }

        if let Some(action_id) = transition.action.as_deref() {
            if !action_ids.contains(action_id) {
                errors.push(format!(
                    "transition {:?} --{:?}--> {:?} references unknown action '{}'",
                    transition.from, transition.event, transition.to, action_id
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

fn registry_ids(entries: &[RegistryEntry], label: &str, errors: &mut Vec<String>) -> HashSet<String> {
    let mut seen = HashSet::new();
    for entry in entries {
        let id = entry.id.trim();
        if id.is_empty() {
            errors.push(format!("{label} contains an empty id"));
            continue;
        }

        if !seen.insert(id.to_string()) {
            errors.push(format!("{label} contains duplicate id '{id}'"));
        }
    }

    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machines::schema::builtin_state_machines_document;

    #[test]
    fn builtin_definition_validates() {
        let document = builtin_state_machines_document();
        validate_state_machines_document(&document).expect("builtin config should validate");
    }

    #[test]
    fn machine_missing_executor_required_transition_is_rejected() {
        let mut document = builtin_state_machines_document();
        document.workflow.transitions.retain(|transition| {
            !(transition.from == WorkflowMachineState::RunPhase
                && transition.event == WorkflowMachineEvent::PhaseSucceeded)
        });
        let error = validate_state_machines_document(&document).expect_err("should fail");
        assert!(
            error.to_string().contains("missing executor-required transition from RunPhase on PhaseSucceeded"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn invalid_guard_reference_is_rejected() {
        let mut document = builtin_state_machines_document();
        document.workflow.transitions[0].guard = Some("missing_guard".to_string());
        let error = validate_state_machines_document(&document).expect_err("should fail");
        assert!(error.to_string().contains("references unknown guard 'missing_guard'"), "unexpected error: {error}");
    }
}
