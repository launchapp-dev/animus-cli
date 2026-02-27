use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ops_common::project_state_dir;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use orchestrator_core::{services::ServiceHub, WorkflowResumeManager, WorkflowRunInput};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    ensure_destructive_confirmation, not_found_error, parse_input_json_or, print_value,
    WorkflowAgentRuntimeCommand, WorkflowCheckpointCommand, WorkflowCommand, WorkflowConfigCommand,
    WorkflowPhaseCommand, WorkflowPhasesCommand, WorkflowPipelinesCommand,
    WorkflowStateMachineCommand,
};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyWorkflowConfig {
    #[serde(default)]
    default_pipeline_id: String,
    #[serde(default)]
    pipelines: Vec<LegacyWorkflowPipeline>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyWorkflowPipeline {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    phases: Vec<String>,
    #[serde(default)]
    phase_settings: HashMap<String, LegacyPhaseRuntimeSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LegacyPhaseRuntimeSettings {
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    fallback_models: Vec<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    web_search: Option<bool>,
    #[serde(default)]
    network_access: Option<bool>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    max_attempts: Option<usize>,
    #[serde(default)]
    extra_args: Vec<String>,
    #[serde(default)]
    codex_config_overrides: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LegacyAgentRuntimeConfig {
    #[serde(default)]
    agents: BTreeMap<String, LegacyAgentProfile>,
    #[serde(default)]
    phase_agent_bindings: BTreeMap<String, String>,
    #[serde(default)]
    phase_directives: BTreeMap<String, String>,
    #[serde(default)]
    phase_output_contracts: BTreeMap<String, orchestrator_core::PhaseOutputContract>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LegacyAgentProfile {
    #[serde(default)]
    description: String,
    #[serde(default)]
    system_prompt: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    fallback_models: Vec<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    web_search: Option<bool>,
    #[serde(default)]
    network_access: Option<bool>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    max_attempts: Option<usize>,
    #[serde(default)]
    extra_args: Vec<String>,
    #[serde(default)]
    codex_config_overrides: Vec<String>,
    #[serde(default)]
    phase_directive: Option<String>,
    #[serde(default)]
    output_contract: Option<orchestrator_core::PhaseOutputContract>,
    #[serde(default)]
    output_json_schema: Option<Value>,
}

fn workflow_config_path(project_root: &str) -> PathBuf {
    orchestrator_core::workflow_config_path(Path::new(project_root))
}

fn agent_runtime_path(project_root: &str) -> PathBuf {
    orchestrator_core::agent_runtime_config_path(Path::new(project_root))
}

fn manual_approvals_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("manual-phase-approvals.v1.json")
}

fn legacy_workflow_config_paths(project_root: &str) -> [PathBuf; 2] {
    orchestrator_core::legacy_workflow_config_paths(Path::new(project_root))
}

fn legacy_agent_runtime_path(project_root: &str) -> PathBuf {
    orchestrator_core::agent_runtime_config::legacy_agent_runtime_config_path(Path::new(
        project_root,
    ))
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(value)?;
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        Uuid::new_v4()
    ));
    std::fs::write(&tmp_path, payload)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

fn emit_daemon_event(project_root: &str, event_type: &str, data: Value) -> Result<()> {
    let path = protocol::Config::global_config_dir().join("daemon-events.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let timestamp = Utc::now().to_rfc3339();
    let event = serde_json::json!({
        "schema": "ao.daemon.event.v1",
        "id": Uuid::new_v4().to_string(),
        "seq": 0,
        "timestamp": timestamp,
        "event_type": event_type,
        "project_root": project_root,
        "data": data,
    });
    let mut line = serde_json::to_string(&event)?;
    line.push('\n');
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn get_state_machine_payload(project_root: &str) -> Result<Value> {
    let loaded = orchestrator_core::load_state_machines_for_project(Path::new(project_root))?;
    Ok(serde_json::json!({
        "path": loaded.path.display().to_string(),
        "schema": loaded.compiled.metadata.schema,
        "version": loaded.compiled.metadata.version,
        "hash": loaded.compiled.metadata.hash,
        "source": loaded.compiled.metadata.source,
        "warnings": loaded.warnings,
        "state_machines": loaded.compiled.document,
    }))
}

fn validate_state_machine_payload(project_root: &str) -> Value {
    let path = orchestrator_core::state_machines_path(Path::new(project_root));
    if !path.exists() {
        return serde_json::json!({
            "path": path.display().to_string(),
            "valid": false,
            "errors": ["state machine metadata file is missing"],
            "warnings": [],
        });
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => {
            return serde_json::json!({
                "path": path.display().to_string(),
                "valid": false,
                "errors": [format!("failed to read metadata file: {error}")],
                "warnings": [],
            })
        }
    };

    let document = match serde_json::from_str::<orchestrator_core::StateMachinesDocument>(&content)
    {
        Ok(document) => document,
        Err(error) => {
            return serde_json::json!({
                "path": path.display().to_string(),
                "valid": false,
                "errors": [format!("invalid JSON: {error}")],
                "warnings": [],
            })
        }
    };

    match orchestrator_core::state_machines::compile_state_machines_document(
        document,
        orchestrator_core::MachineSource::Json,
    ) {
        Ok(compiled) => serde_json::json!({
            "path": path.display().to_string(),
            "valid": true,
            "errors": [],
            "warnings": [],
            "schema": compiled.metadata.schema,
            "version": compiled.metadata.version,
            "hash": compiled.metadata.hash,
            "source": compiled.metadata.source,
        }),
        Err(error) => serde_json::json!({
            "path": path.display().to_string(),
            "valid": false,
            "errors": [error.to_string()],
            "warnings": [],
        }),
    }
}

fn set_state_machine_payload(project_root: &str, input_json: &str) -> Result<Value> {
    let document: orchestrator_core::StateMachinesDocument =
        serde_json::from_str(input_json).with_context(|| {
            "invalid --input-json payload for workflow state-machine set; run 'ao workflow state-machine set --help' for schema"
        })?;
    let compiled =
        orchestrator_core::write_state_machines_document(Path::new(project_root), &document)?;
    let path = orchestrator_core::state_machines_path(Path::new(project_root));

    Ok(serde_json::json!({
        "path": path.display().to_string(),
        "schema": compiled.metadata.schema,
        "version": compiled.metadata.version,
        "hash": compiled.metadata.hash,
        "source": compiled.metadata.source,
        "state_machines": compiled.document,
    }))
}

fn get_agent_runtime_payload(project_root: &str) -> Value {
    let path = agent_runtime_path(project_root);
    match orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(
        Path::new(project_root),
    ) {
        Ok(loaded) => serde_json::json!({
            "path": path.display().to_string(),
            "source": loaded.metadata.source,
            "schema": loaded.metadata.schema,
            "version": loaded.metadata.version,
            "hash": loaded.metadata.hash,
            "warnings": [],
            "agent_runtime": loaded.config,
        }),
        Err(error) => serde_json::json!({
            "path": path.display().to_string(),
            "source": "error",
            "schema": orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_SCHEMA_ID,
            "version": orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_VERSION,
            "warnings": [error.to_string()],
            "agent_runtime": orchestrator_core::builtin_agent_runtime_config(),
        }),
    }
}

fn validate_agent_runtime_payload(project_root: &str) -> Value {
    let path = agent_runtime_path(project_root);
    match orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(
        Path::new(project_root),
    ) {
        Ok(loaded) => serde_json::json!({
            "path": path.display().to_string(),
            "valid": true,
            "errors": [],
            "warnings": [],
            "schema": loaded.metadata.schema,
            "version": loaded.metadata.version,
            "hash": loaded.metadata.hash,
            "source": loaded.metadata.source,
        }),
        Err(error) => serde_json::json!({
            "path": path.display().to_string(),
            "valid": false,
            "errors": [error.to_string()],
            "warnings": [],
        }),
    }
}

fn set_agent_runtime_payload(project_root: &str, input_json: &str) -> Result<Value> {
    let config: orchestrator_core::AgentRuntimeConfig =
        serde_json::from_str(input_json).with_context(|| {
            "invalid --input-json payload for workflow agent-runtime set; run 'ao workflow agent-runtime set --help' for schema"
        })?;
    orchestrator_core::write_agent_runtime_config(Path::new(project_root), &config)?;
    let path = agent_runtime_path(project_root);

    Ok(serde_json::json!({
        "path": path.display().to_string(),
        "schema": config.schema,
        "version": config.version,
        "hash": orchestrator_core::agent_runtime_config::agent_runtime_config_hash(&config),
        "agent_runtime": config,
    }))
}

fn get_workflow_config_payload(project_root: &str) -> Value {
    let path = workflow_config_path(project_root);
    match orchestrator_core::load_workflow_config_with_metadata(Path::new(project_root)) {
        Ok(loaded) => serde_json::json!({
            "path": path.display().to_string(),
            "source": loaded.metadata.source,
            "schema": loaded.metadata.schema,
            "version": loaded.metadata.version,
            "hash": loaded.metadata.hash,
            "workflow_config": loaded.config,
        }),
        Err(error) => serde_json::json!({
            "path": path.display().to_string(),
            "source": "error",
            "schema": orchestrator_core::WORKFLOW_CONFIG_SCHEMA_ID,
            "version": orchestrator_core::WORKFLOW_CONFIG_VERSION,
            "errors": [error.to_string()],
            "workflow_config": serde_json::Value::Null,
        }),
    }
}

fn validate_workflow_config_payload(project_root: &str) -> Value {
    let workflow_loaded =
        orchestrator_core::load_workflow_config_with_metadata(Path::new(project_root));
    let runtime_loaded =
        orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(
            Path::new(project_root),
        );

    match (workflow_loaded, runtime_loaded) {
        (Ok(workflow), Ok(runtime)) => {
            match orchestrator_core::validate_workflow_and_runtime_configs(
                &workflow.config,
                &runtime.config,
            ) {
                Ok(()) => serde_json::json!({
                    "valid": true,
                    "errors": [],
                    "workflow_config_path": workflow.path.display().to_string(),
                    "agent_runtime_path": runtime.path.display().to_string(),
                    "workflow_config_hash": workflow.metadata.hash,
                    "agent_runtime_hash": runtime.metadata.hash,
                }),
                Err(error) => serde_json::json!({
                    "valid": false,
                    "errors": [error.to_string()],
                    "workflow_config_path": workflow.path.display().to_string(),
                    "agent_runtime_path": runtime.path.display().to_string(),
                }),
            }
        }
        (Err(workflow_error), Err(runtime_error)) => serde_json::json!({
            "valid": false,
            "errors": [workflow_error.to_string(), runtime_error.to_string()],
        }),
        (Err(workflow_error), _) => serde_json::json!({
            "valid": false,
            "errors": [workflow_error.to_string()],
        }),
        (_, Err(runtime_error)) => serde_json::json!({
            "valid": false,
            "errors": [runtime_error.to_string()],
        }),
    }
}

fn title_case_phase_id(phase_id: &str) -> String {
    phase_id
        .split(['-', '_'])
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut label = first.to_ascii_uppercase().to_string();
                    label.push_str(chars.as_str());
                    label
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn migrate_v1_to_v2(project_root: &str) -> Result<Value> {
    let legacy_workflow_path = legacy_workflow_config_paths(project_root)
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| {
            not_found_error("legacy workflow config not found (expected workflow-config.json)")
        })?;
    let legacy_workflow: LegacyWorkflowConfig =
        serde_json::from_str(&std::fs::read_to_string(&legacy_workflow_path)?)?;

    let legacy_runtime = {
        let path = legacy_agent_runtime_path(project_root);
        if path.exists() {
            serde_json::from_str::<LegacyAgentRuntimeConfig>(&std::fs::read_to_string(path)?)?
        } else {
            LegacyAgentRuntimeConfig::default()
        }
    };

    let mut phase_ids = BTreeSet::new();
    for pipeline in &legacy_workflow.pipelines {
        for phase in &pipeline.phases {
            let normalized = phase.trim();
            if !normalized.is_empty() {
                phase_ids.insert(normalized.to_string());
            }
        }
    }
    for phase_id in legacy_runtime.phase_agent_bindings.keys() {
        let normalized = phase_id.trim();
        if !normalized.is_empty() {
            phase_ids.insert(normalized.to_string());
        }
    }
    for phase_id in legacy_runtime.phase_directives.keys() {
        let normalized = phase_id.trim();
        if !normalized.is_empty() {
            phase_ids.insert(normalized.to_string());
        }
    }

    let mut workflow_config = orchestrator_core::builtin_workflow_config();
    workflow_config.default_pipeline_id = legacy_workflow.default_pipeline_id.trim().to_string();
    workflow_config.pipelines = legacy_workflow
        .pipelines
        .iter()
        .map(|pipeline| orchestrator_core::PipelineDefinition {
            id: pipeline.id.trim().to_string(),
            name: if pipeline.name.trim().is_empty() {
                title_case_phase_id(pipeline.id.as_str())
            } else {
                pipeline.name.clone()
            },
            description: pipeline.description.clone().unwrap_or_default(),
            phases: pipeline
                .phases
                .iter()
                .map(String::as_str)
                .map(str::trim)
                .filter(|phase| !phase.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        })
        .collect();

    workflow_config.phase_catalog = phase_ids
        .iter()
        .map(|phase_id| {
            (
                phase_id.clone(),
                orchestrator_core::PhaseUiDefinition {
                    label: title_case_phase_id(phase_id),
                    description: String::new(),
                    category: "custom".to_string(),
                    icon: None,
                    docs_url: None,
                    tags: Vec::new(),
                    visible: true,
                },
            )
        })
        .collect();

    let mut runtime_config = orchestrator_core::builtin_agent_runtime_config();
    if !legacy_runtime.agents.is_empty() {
        runtime_config.agents = legacy_runtime
            .agents
            .iter()
            .map(|(agent_id, profile)| {
                (
                    agent_id.clone(),
                    orchestrator_core::AgentProfile {
                        description: profile.description.clone(),
                        system_prompt: if profile.system_prompt.trim().is_empty() {
                            "You are a workflow phase execution agent.".to_string()
                        } else {
                            profile.system_prompt.clone()
                        },
                        tool: profile.tool.clone(),
                        model: profile.model.clone(),
                        fallback_models: profile.fallback_models.clone(),
                        reasoning_effort: profile.reasoning_effort.clone(),
                        web_search: profile.web_search,
                        network_access: profile.network_access,
                        timeout_secs: profile.timeout_secs,
                        max_attempts: profile.max_attempts,
                        extra_args: profile.extra_args.clone(),
                        codex_config_overrides: profile.codex_config_overrides.clone(),
                    },
                )
            })
            .collect();
    }

    let default_pipeline_settings = legacy_workflow
        .pipelines
        .iter()
        .find(|pipeline| {
            pipeline
                .id
                .eq_ignore_ascii_case(legacy_workflow.default_pipeline_id.as_str())
        })
        .or_else(|| legacy_workflow.pipelines.first())
        .map(|pipeline| pipeline.phase_settings.clone())
        .unwrap_or_default();

    let default_agent = legacy_runtime
        .phase_agent_bindings
        .get("default")
        .cloned()
        .unwrap_or_else(|| "default".to_string());

    let mut phases = BTreeMap::new();
    phases.insert(
        "default".to_string(),
        orchestrator_core::PhaseExecutionDefinition {
            mode: orchestrator_core::PhaseExecutionMode::Agent,
            agent_id: Some(default_agent.clone()),
            directive: legacy_runtime.phase_directives.get("default").cloned(),
            runtime: None,
            output_contract: None,
            output_json_schema: None,
            command: None,
            manual: None,
        },
    );

    for phase_id in &phase_ids {
        let bound_agent = legacy_runtime
            .phase_agent_bindings
            .get(phase_id)
            .cloned()
            .unwrap_or_else(|| default_agent.clone());

        let runtime = default_pipeline_settings.get(phase_id).map(|settings| {
            orchestrator_core::AgentRuntimeOverrides {
                tool: settings.tool.clone(),
                model: settings.model.clone(),
                fallback_models: settings.fallback_models.clone(),
                reasoning_effort: settings.reasoning_effort.clone(),
                web_search: settings.web_search,
                network_access: settings.network_access,
                timeout_secs: settings.timeout_secs,
                max_attempts: settings.max_attempts,
                extra_args: settings.extra_args.clone(),
                codex_config_overrides: settings.codex_config_overrides.clone(),
            }
        });

        let output_contract = legacy_runtime
            .phase_output_contracts
            .get(phase_id)
            .cloned()
            .or_else(|| {
                legacy_runtime
                    .agents
                    .get(&bound_agent)
                    .and_then(|profile| profile.output_contract.clone())
            });

        let output_json_schema = legacy_runtime
            .agents
            .get(&bound_agent)
            .and_then(|profile| profile.output_json_schema.clone());

        phases.insert(
            phase_id.clone(),
            orchestrator_core::PhaseExecutionDefinition {
                mode: orchestrator_core::PhaseExecutionMode::Agent,
                agent_id: Some(bound_agent.clone()),
                directive: legacy_runtime
                    .phase_directives
                    .get(phase_id)
                    .cloned()
                    .or_else(|| {
                        legacy_runtime
                            .agents
                            .get(&bound_agent)
                            .and_then(|profile| profile.phase_directive.clone())
                    }),
                runtime,
                output_contract,
                output_json_schema,
                command: None,
                manual: None,
            },
        );
    }

    runtime_config.phases = phases;

    orchestrator_core::write_workflow_config(Path::new(project_root), &workflow_config)?;
    orchestrator_core::write_agent_runtime_config(Path::new(project_root), &runtime_config)?;

    Ok(serde_json::json!({
        "workflow_config_path": workflow_config_path(project_root).display().to_string(),
        "agent_runtime_path": agent_runtime_path(project_root).display().to_string(),
        "workflow_config_hash": orchestrator_core::workflow_config_hash(&workflow_config),
        "agent_runtime_hash": orchestrator_core::agent_runtime_config::agent_runtime_config_hash(&runtime_config),
    }))
}

fn resumability_to_json(status: &orchestrator_core::ResumabilityStatus) -> Value {
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
    let value = serde_json::to_value(store)?;
    write_json_atomic(&manual_approvals_path(project_root), &value)
}

fn upsert_phase_definition(
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

fn remove_phase_definition(project_root: &str, phase_id: &str) -> Result<Value> {
    let workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    if workflow.pipelines.iter().any(|pipeline| {
        pipeline
            .phases
            .iter()
            .any(|phase| phase.eq_ignore_ascii_case(phase_id))
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

fn preview_phase_removal(project_root: &str, phase_id: &str) -> Result<Value> {
    let workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    let normalized_phase_id = runtime
        .phases
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(phase_id))
        .cloned()
        .ok_or_else(|| anyhow!("phase '{}' does not exist", phase_id))?;

    let blocking_pipelines: Vec<String> = workflow
        .pipelines
        .iter()
        .filter(|pipeline| {
            pipeline
                .phases
                .iter()
                .any(|phase| phase.eq_ignore_ascii_case(normalized_phase_id.as_str()))
        })
        .map(|pipeline| pipeline.id.clone())
        .collect();

    Ok(serde_json::json!({
        "operation": "workflow.phases.remove",
        "target": {
            "phase_id": normalized_phase_id,
        },
        "action": "workflow.phases.remove",
        "dry_run": true,
        "destructive": true,
        "requires_confirmation": true,
        "planned_effects": [
            "remove phase runtime definition",
        ],
        "next_step": format!(
            "rerun 'ao workflow phases remove --phase {} --confirm {}' to apply",
            phase_id,
            phase_id
        ),
        "phase_id": normalized_phase_id,
        "can_remove": blocking_pipelines.is_empty(),
        "blocking_pipelines": blocking_pipelines,
    }))
}

fn upsert_pipeline(
    project_root: &str,
    pipeline: orchestrator_core::PipelineDefinition,
) -> Result<Value> {
    let mut workflow = orchestrator_core::load_workflow_config(Path::new(project_root))?;
    if let Some(existing) = workflow
        .pipelines
        .iter_mut()
        .find(|existing| existing.id.eq_ignore_ascii_case(pipeline.id.as_str()))
    {
        *existing = pipeline.clone();
    } else {
        workflow.pipelines.push(pipeline.clone());
    }

    let runtime = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    orchestrator_core::validate_workflow_and_runtime_configs(&workflow, &runtime)?;
    orchestrator_core::write_workflow_config(Path::new(project_root), &workflow)?;

    Ok(serde_json::json!({
        "pipeline": pipeline,
        "workflow_config_hash": orchestrator_core::workflow_config_hash(&workflow),
    }))
}

fn phase_payload(project_root: &str, phase_id: &str) -> Result<Value> {
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

fn list_phase_payload(project_root: &str) -> Result<Value> {
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

async fn approve_manual_phase(
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
        approved_by: "ao-cli".to_string(),
    });
    write_manual_approvals(project_root, &store)?;

    let updated = hub.workflows().complete_current_phase(workflow_id).await?;
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

pub(crate) async fn handle_workflow(
    command: WorkflowCommand,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let workflows = hub.workflows();

    match command {
        WorkflowCommand::List => print_value(workflows.list().await?, json),
        WorkflowCommand::Get(args) => print_value(workflows.get(&args.id).await?, json),
        WorkflowCommand::Decisions(args) => print_value(workflows.decisions(&args.id).await?, json),
        WorkflowCommand::Checkpoints { command } => match command {
            WorkflowCheckpointCommand::List(args) => {
                print_value(workflows.list_checkpoints(&args.id).await?, json)
            }
            WorkflowCheckpointCommand::Get(args) => print_value(
                workflows.get_checkpoint(&args.id, args.checkpoint).await?,
                json,
            ),
            WorkflowCheckpointCommand::Prune(args) => {
                let manager = orchestrator_core::WorkflowStateManager::new(project_root);
                let pruned = manager.prune_checkpoints(
                    &args.id,
                    args.keep_last_per_phase,
                    args.max_age_hours,
                    args.dry_run,
                )?;
                print_value(pruned, json)
            }
        },
        WorkflowCommand::Run(args) => {
            let input = parse_input_json_or(args.input_json, || {
                Ok(WorkflowRunInput {
                    task_id: args.task_id,
                    pipeline_id: args.pipeline_id,
                })
            })?;
            print_value(workflows.run(input).await?, json)
        }
        WorkflowCommand::Resume(args) => print_value(workflows.resume(&args.id).await?, json),
        WorkflowCommand::ResumeStatus(args) => {
            let workflow = workflows.get(&args.id).await?;
            let manager = WorkflowResumeManager::new(project_root)?;
            let resumability = manager.validate_resumability(&workflow);
            print_value(
                serde_json::json!({
                    "workflow_id": workflow.id,
                    "status": workflow.status,
                    "machine_state": workflow.machine_state,
                    "resumability": resumability_to_json(&resumability),
                }),
                json,
            )
        }
        WorkflowCommand::Pause(args) => {
            let workflow = workflows.get(&args.id).await?;
            if args.dry_run {
                let workflow_id = workflow.id.clone();
                return print_value(
                    serde_json::json!({
                        "operation": "workflow.pause",
                        "target": {
                            "workflow_id": workflow_id.clone(),
                        },
                        "action": "workflow.pause",
                        "dry_run": true,
                        "destructive": true,
                        "requires_confirmation": true,
                        "planned_effects": [
                            "pause workflow execution",
                        ],
                        "next_step": format!(
                            "rerun 'ao workflow pause --id {} --confirm {}' to apply",
                            workflow_id,
                            workflow_id
                        ),
                        "workflow_id": workflow_id,
                        "status": workflow.status,
                        "current_phase": workflow.current_phase,
                    }),
                    json,
                );
            }
            ensure_destructive_confirmation(
                args.confirm.as_deref(),
                &args.id,
                "workflow pause",
                "--id",
            )?;
            print_value(workflows.pause(&args.id).await?, json)
        }
        WorkflowCommand::Cancel(args) => {
            let workflow = workflows.get(&args.id).await?;
            if args.dry_run {
                let workflow_id = workflow.id.clone();
                return print_value(
                    serde_json::json!({
                        "operation": "workflow.cancel",
                        "target": {
                            "workflow_id": workflow_id.clone(),
                        },
                        "action": "workflow.cancel",
                        "dry_run": true,
                        "destructive": true,
                        "requires_confirmation": true,
                        "planned_effects": [
                            "cancel workflow execution",
                        ],
                        "next_step": format!(
                            "rerun 'ao workflow cancel --id {} --confirm {}' to apply",
                            workflow_id,
                            workflow_id
                        ),
                        "workflow_id": workflow_id,
                        "status": workflow.status,
                        "current_phase": workflow.current_phase,
                    }),
                    json,
                );
            }
            ensure_destructive_confirmation(
                args.confirm.as_deref(),
                &args.id,
                "workflow cancel",
                "--id",
            )?;
            print_value(workflows.cancel(&args.id).await?, json)
        }
        WorkflowCommand::Phase { command } => match command {
            WorkflowPhaseCommand::Approve(args) => print_value(
                approve_manual_phase(hub.clone(), project_root, &args.id, &args.phase, &args.note)
                    .await?,
                json,
            ),
        },
        WorkflowCommand::Phases { command } => match command {
            WorkflowPhasesCommand::List => print_value(list_phase_payload(project_root)?, json),
            WorkflowPhasesCommand::Get(args) => {
                print_value(phase_payload(project_root, &args.phase)?, json)
            }
            WorkflowPhasesCommand::Upsert(args) => {
                let definition: orchestrator_core::PhaseExecutionDefinition =
                    serde_json::from_str(&args.input_json).with_context(|| {
                        "invalid --input-json payload for workflow phases upsert; run 'ao workflow phases upsert --help' for schema"
                    })?;
                print_value(
                    upsert_phase_definition(project_root, &args.phase, definition)?,
                    json,
                )
            }
            WorkflowPhasesCommand::Remove(args) => {
                if args.dry_run {
                    return print_value(preview_phase_removal(project_root, &args.phase)?, json);
                }
                ensure_destructive_confirmation(
                    args.confirm.as_deref(),
                    &args.phase,
                    "workflow phases remove",
                    "--phase",
                )?;
                print_value(remove_phase_definition(project_root, &args.phase)?, json)
            }
        },
        WorkflowCommand::Pipelines { command } => match command {
            WorkflowPipelinesCommand::List => {
                let config = orchestrator_core::load_workflow_config(Path::new(project_root))?;
                print_value(config.pipelines, json)
            }
            WorkflowPipelinesCommand::Upsert(args) => {
                let pipeline: orchestrator_core::PipelineDefinition =
                    serde_json::from_str(&args.input_json).with_context(|| {
                        "invalid --input-json payload for workflow pipelines upsert; run 'ao workflow pipelines upsert --help' for schema"
                    })?;
                print_value(upsert_pipeline(project_root, pipeline)?, json)
            }
        },
        WorkflowCommand::Config { command } => match command {
            WorkflowConfigCommand::Get => {
                print_value(get_workflow_config_payload(project_root), json)
            }
            WorkflowConfigCommand::Validate => {
                print_value(validate_workflow_config_payload(project_root), json)
            }
            WorkflowConfigCommand::MigrateV2 => print_value(migrate_v1_to_v2(project_root)?, json),
        },
        WorkflowCommand::StateMachine { command } => match command {
            WorkflowStateMachineCommand::Get => {
                print_value(get_state_machine_payload(project_root)?, json)
            }
            WorkflowStateMachineCommand::Validate => {
                print_value(validate_state_machine_payload(project_root), json)
            }
            WorkflowStateMachineCommand::Set(args) => print_value(
                set_state_machine_payload(project_root, &args.input_json)?,
                json,
            ),
        },
        WorkflowCommand::AgentRuntime { command } => match command {
            WorkflowAgentRuntimeCommand::Get => {
                print_value(get_agent_runtime_payload(project_root), json)
            }
            WorkflowAgentRuntimeCommand::Validate => {
                print_value(validate_agent_runtime_payload(project_root), json)
            }
            WorkflowAgentRuntimeCommand::Set(args) => print_value(
                set_agent_runtime_payload(project_root, &args.input_json)?,
                json,
            ),
        },
        WorkflowCommand::UpdatePipeline(args) => {
            let pipeline = parse_input_json_or(args.input_json, || {
                Ok(orchestrator_core::PipelineDefinition {
                    id: args.id,
                    name: args.name,
                    description: args.description.unwrap_or_default(),
                    phases: args.phases,
                })
            })?;
            print_value(upsert_pipeline(project_root, pipeline)?, json)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_state_machine_payload_reports_actionable_json_error() {
        let error = set_state_machine_payload("/tmp/unused", "{invalid")
            .expect_err("invalid payload should fail");
        let message = error.to_string();
        assert!(message.contains("invalid --input-json payload"));
        assert!(message.contains("workflow state-machine set --help"));
    }

    #[test]
    fn set_agent_runtime_payload_reports_actionable_json_error() {
        let error = set_agent_runtime_payload("/tmp/unused", "{invalid")
            .expect_err("invalid payload should fail");
        let message = error.to_string();
        assert!(message.contains("invalid --input-json payload"));
        assert!(message.contains("workflow agent-runtime set --help"));
    }
}
