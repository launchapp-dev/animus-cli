use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use orchestrator_core::services::ServiceHub;

use super::config::{manual_approvals_path, title_case_phase_id};
use super::emit_daemon_event;
use crate::dry_run_envelope;
use crate::services::runtime::sync_task_status_for_workflow_result;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManualApprovalRecord {
    workflow_id: String,
    phase_id: String,
    note: String,
    approved_at: String,
    approved_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManualApprovalsStore {
    #[serde(default)]
    approvals: Vec<ManualApprovalRecord>,
}

pub(crate) fn resumability_to_json(status: &orchestrator_core::ResumabilityStatus) -> Value {
    match status {
        orchestrator_core::ResumabilityStatus::Resumable {
            workflow_id,
            reason,
        } => serde_json::json!({
            "kind": "resumable",
            "workflow_id": workflow_id,
            "reason": reason,
        }),
        orchestrator_core::ResumabilityStatus::Stale {
            workflow_id,
            age_hours,
            max_age_hours,
        } => serde_json::json!({
            "kind": "stale",
            "workflow_id": workflow_id,
            "age_hours": age_hours,
            "max_age_hours": max_age_hours,
        }),
        orchestrator_core::ResumabilityStatus::InvalidState {
            workflow_id,
            status,
            reason,
        } => serde_json::json!({
            "kind": "invalid_state",
            "workflow_id": workflow_id,
            "status": status,
            "reason": reason,
        }),
    }
}

fn read_manual_approvals(project_root: &str) -> Result<ManualApprovalsStore> {
    let path = manual_approvals_path(project_root);
    if !path.exists() {
        return Ok(ManualApprovalsStore::default());
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

fn write_manual_approvals(project_root: &str, store: &ManualApprovalsStore) -> Result<()> {
    orchestrator_core::write_json_pretty(&manual_approvals_path(project_root), store)
}

pub(crate) fn upsert_phase_definition(
    project_root: &str,
    phase_id: &str,
    definition: orchestrator_core::PhaseExecutionDefinition,
) -> Result<Value> {
    let mut workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    if workflow
        .phase_catalog
        .keys()
        .all(|existing| !existing.eq_ignore_ascii_case(phase_id))
    {
        workflow.phase_catalog.insert(
            phase_id.to_string(),
            orchestrator_core::PhaseUiDefinition {
                label: title_case_phase_id(phase_id),
                description: String::new(),
                category: "custom".to_string(),
                icon: None,
                docs_url: None,
                tags: Vec::new(),
                visible: true,
            },
        );
    }

    let mut runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    runtime
        .phases
        .insert(phase_id.to_string(), definition.clone());

    orchestrator_core::validate_workflow_and_runtime_configs(&workflow, &runtime)?;
    orchestrator_core::write_agent_runtime_config(Path::new(project_root), &runtime)?;
    orchestrator_core::write_workflow_config(Path::new(project_root), &workflow)?;

    Ok(serde_json::json!({
        "phase_id": phase_id,
        "phase": definition,
        "agent_runtime_hash": orchestrator_core::agent_runtime_config::agent_runtime_config_hash(&runtime),
    }))
}

pub(crate) fn remove_phase_definition(project_root: &str, phase_id: &str) -> Result<Value> {
    let workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    if workflow.workflows.iter().any(|pipeline| {
        pipeline
            .phases
            .iter()
            .any(|phase| phase.phase_id().eq_ignore_ascii_case(phase_id))
    }) {
        return Err(anyhow!(
            "cannot remove phase '{}' because at least one pipeline references it",
            phase_id
        ));
    }

    let mut runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    let normalized_phase_id = runtime
        .phases
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(phase_id))
        .cloned()
        .ok_or_else(|| anyhow!("phase '{}' does not exist", phase_id))?;
    runtime.phases.remove(&normalized_phase_id);

    orchestrator_core::write_agent_runtime_config(Path::new(project_root), &runtime)?;
    Ok(serde_json::json!({
        "removed": normalized_phase_id,
        "agent_runtime_hash": orchestrator_core::agent_runtime_config::agent_runtime_config_hash(&runtime),
    }))
}

pub(crate) fn preview_phase_removal(project_root: &str, phase_id: &str) -> Result<Value> {
    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    let normalized_phase_id = runtime
        .phases
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(phase_id))
        .cloned()
        .ok_or_else(|| anyhow!("phase '{}' does not exist", phase_id))?;

    let mut envelope = dry_run_envelope(
        "workflow.phases.remove",
        serde_json::json!({"phase_id": &normalized_phase_id}),
        "workflow.phases.remove",
        vec!["remove phase runtime definition".to_string()],
        &format!(
            "rerun 'ao workflow phases remove --phase {} --confirm {}' to apply",
            phase_id, phase_id
        ),
    );
    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("can_remove".to_string(), serde_json::json!(true));
    }
    Ok(envelope)
}

pub(crate) fn upsert_pipeline(
    project_root: &str,
    pipeline: orchestrator_core::WorkflowDefinition,
) -> Result<Value> {
    let mut workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    if let Some(existing) = workflow
        .workflows
        .iter_mut()
        .find(|existing| existing.id.eq_ignore_ascii_case(pipeline.id.as_str()))
    {
        *existing = pipeline.clone();
    } else {
        workflow.workflows.push(pipeline.clone());
    }

    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    orchestrator_core::validate_workflow_and_runtime_configs(&workflow, &runtime)?;
    orchestrator_core::write_workflow_config(Path::new(project_root), &workflow)?;

    Ok(serde_json::json!({
        "pipeline": pipeline,
        "workflow_config_hash": orchestrator_core::workflow_config_hash(&workflow),
    }))
}

pub(crate) fn phase_payload(project_root: &str, phase_id: &str) -> Result<Value> {
    let workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;

    let ui = workflow
        .phase_catalog
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(phase_id))
        .map(|(_, value)| value.clone());
    let runtime_definition = runtime
        .phases
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(phase_id))
        .map(|(_, value)| value.clone());

    Ok(serde_json::json!({
        "phase_id": phase_id,
        "ui": ui,
        "runtime": runtime_definition,
    }))
}

pub(crate) fn list_phase_payload(project_root: &str) -> Result<Value> {
    let workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;

    let mut phases = Vec::new();
    for (phase_id, ui) in &workflow.phase_catalog {
        let runtime_definition = runtime
            .phases
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(phase_id.as_str()))
            .map(|(_, value)| value.clone());
        phases.push(serde_json::json!({
            "phase_id": phase_id,
            "ui": ui,
            "runtime": runtime_definition,
        }));
    }

    Ok(serde_json::json!({
        "phases": phases,
    }))
}

pub(crate) async fn approve_manual_phase(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    note: &str,
) -> Result<Value> {
    let workflow = hub.workflows().get(workflow_id).await?;
    let current_phase = workflow
        .current_phase
        .clone()
        .or_else(|| {
            workflow
                .phases
                .get(workflow.current_phase_index)
                .map(|phase| phase.phase_id.clone())
        })
        .ok_or_else(|| anyhow!("workflow '{}' has no active phase", workflow_id))?;

    if !current_phase.eq_ignore_ascii_case(phase_id) {
        return Err(anyhow!(
            "workflow '{}' active phase is '{}' (requested '{}')",
            workflow_id,
            current_phase,
            phase_id
        ));
    }

    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    let definition = runtime
        .phase_execution(phase_id)
        .ok_or_else(|| anyhow!("phase '{}' is not configured", phase_id))?;

    if !matches!(
        definition.mode,
        orchestrator_core::PhaseExecutionMode::Manual
    ) {
        return Err(anyhow!("phase '{}' is not in manual mode", phase_id));
    }

    let manual = definition
        .manual
        .as_ref()
        .ok_or_else(|| anyhow!("phase '{}' missing manual configuration", phase_id))?;

    if manual.approval_note_required && note.trim().is_empty() {
        return Err(anyhow!(
            "phase '{}' requires a non-empty approval note",
            phase_id
        ));
    }

    let mut store = read_manual_approvals(project_root)?;
    store.approvals.push(ManualApprovalRecord {
        workflow_id: workflow_id.to_string(),
        phase_id: phase_id.to_string(),
        note: note.to_string(),
        approved_at: Utc::now().to_rfc3339(),
        approved_by: protocol::ACTOR_CLI.to_string(),
    });
    write_manual_approvals(project_root, &store)?;

    let updated = hub.workflows().complete_current_phase(workflow_id).await?;
    if !workflow.task_id.trim().is_empty() {
        sync_task_status_for_workflow_result(
            hub.clone(),
            project_root,
            &workflow.task_id,
            updated.status,
            Some(updated.id.as_str()),
        )
        .await;
    }
    emit_daemon_event(
        project_root,
        "workflow-phase-manual-approved",
        serde_json::json!({
            "workflow_id": workflow_id,
            "task_id": workflow.task_id,
            "phase_id": phase_id,
            "note": note,
        }),
    )?;

    Ok(serde_json::json!({
        "workflow": updated,
        "manual_approval": {
            "phase_id": phase_id,
            "note": note,
            "approved_at": Utc::now().to_rfc3339(),
        },
    }))
}
