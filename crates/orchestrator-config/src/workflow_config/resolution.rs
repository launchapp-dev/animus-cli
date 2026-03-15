use std::collections::HashMap;

use super::types::*;

pub fn resolve_workflow_phase_plan(config: &WorkflowConfig, workflow_ref: Option<&str>) -> Option<Vec<String>> {
    let requested =
        workflow_ref.map(str::trim).filter(|value| !value.is_empty()).unwrap_or(config.default_workflow_ref.trim());

    if requested.is_empty() {
        return None;
    }

    config.workflows.iter().find(|workflow| workflow.id.eq_ignore_ascii_case(requested))?;

    let expanded = expand_workflow_phases(&config.workflows, requested).ok()?;

    let phases: Vec<String> = expanded
        .iter()
        .map(|entry| entry.phase_id())
        .map(str::trim)
        .filter(|phase| !phase.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    if phases.is_empty() {
        None
    } else {
        Some(phases)
    }
}

pub fn resolve_workflow_verdict_routing(
    config: &WorkflowConfig,
    workflow_ref: Option<&str>,
) -> HashMap<String, HashMap<String, PhaseTransitionConfig>> {
    let requested =
        workflow_ref.map(str::trim).filter(|value| !value.is_empty()).unwrap_or(config.default_workflow_ref.trim());

    if requested.is_empty() {
        return HashMap::new();
    }

    let expanded = match expand_workflow_phases(&config.workflows, requested) {
        Ok(phases) => phases,
        Err(_) => return HashMap::new(),
    };

    let mut routing = HashMap::new();
    for entry in &expanded {
        if let Some(verdicts) = entry.on_verdict() {
            if !verdicts.is_empty() {
                routing.insert(entry.phase_id().to_owned(), verdicts.clone());
            }
        }
    }
    routing
}

pub fn resolve_workflow_rework_attempts(config: &WorkflowConfig, workflow_ref: Option<&str>) -> HashMap<String, u32> {
    let requested =
        workflow_ref.map(str::trim).filter(|value| !value.is_empty()).unwrap_or(config.default_workflow_ref.trim());

    if requested.is_empty() {
        return HashMap::new();
    }

    let expanded = match expand_workflow_phases(&config.workflows, requested) {
        Ok(phases) => phases,
        Err(_) => return HashMap::new(),
    };

    let mut limits = HashMap::new();
    for entry in &expanded {
        if let Some(max_rework_attempts) =
            entry.max_rework_attempts().filter(|value| *value != default_max_rework_attempts())
        {
            limits.insert(entry.phase_id().to_owned(), max_rework_attempts);
        }
    }
    limits
}

pub fn resolve_workflow_skip_guards(
    config: &WorkflowConfig,
    workflow_ref: Option<&str>,
) -> HashMap<String, Vec<String>> {
    let requested =
        workflow_ref.map(str::trim).filter(|value| !value.is_empty()).unwrap_or(config.default_workflow_ref.trim());

    if requested.is_empty() {
        return HashMap::new();
    }

    let expanded = match expand_workflow_phases(&config.workflows, requested) {
        Ok(phases) => phases,
        Err(_) => return HashMap::new(),
    };

    let mut guards = HashMap::new();
    for entry in &expanded {
        let skip_if = entry.skip_if();
        if !skip_if.is_empty() {
            guards.insert(entry.phase_id().trim().to_owned(), skip_if.to_vec());
        }
    }
    guards
}
