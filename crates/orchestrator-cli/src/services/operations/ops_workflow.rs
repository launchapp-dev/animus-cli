use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ops_common::project_state_dir;
use anyhow::{anyhow, Result};
use chrono::Utc;
use orchestrator_core::{services::ServiceHub, WorkflowResumeManager, WorkflowRunInput};
use protocol::{
    canonical_model_id, default_fallback_models_for_phase, default_primary_model_for_phase,
    normalize_tool_id, required_api_keys_for_tool, tool_for_model_id,
    tool_supports_repository_writes, ModelRoutingComplexity,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    ensure_destructive_confirmation, parse_input_json_or, print_value, WorkflowAgentRuntimeCommand,
    WorkflowCheckpointCommand, WorkflowCommand, WorkflowConfigCommand, WorkflowPhaseCommand,
    WorkflowPhasesCommand, WorkflowPipelinesCommand, WorkflowStateMachineCommand,
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
    timeout_secs: Option<u64>,
    #[serde(default)]
    max_attempts: Option<usize>,
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
    timeout_secs: Option<u64>,
    #[serde(default)]
    max_attempts: Option<usize>,
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
    let document: orchestrator_core::StateMachinesDocument = serde_json::from_str(input_json)?;
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
    let config: orchestrator_core::AgentRuntimeConfig = serde_json::from_str(input_json)?;
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

const PHASE_RETRY_BACKOFF_BASE_MS: u64 = 200;
const PHASE_RETRY_BACKOFF_CAP_MS: u64 = 3_000;

#[derive(Debug, Clone)]
struct RuntimeDiagnosticTarget {
    index: usize,
    tool: String,
    model: String,
    source: String,
}

fn parse_phase_complexity_arg(value: Option<&str>) -> Result<Option<ModelRoutingComplexity>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    ModelRoutingComplexity::parse(value)
        .ok_or_else(|| {
            anyhow!(
                "invalid complexity '{}'; expected one of: low, medium, high",
                value
            )
        })
        .map(Some)
}

fn env_phase_key(phase_id: &str) -> String {
    phase_id
        .trim()
        .to_ascii_uppercase()
        .replace('-', "_")
        .replace(' ', "_")
}

fn parse_model_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(canonical_model_id)
        .filter(|value| !value.is_empty())
        .collect()
}

fn parse_env_bool(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn resolve_phase_model_for_diagnostics(
    runtime_config: &orchestrator_core::AgentRuntimeConfig,
    phase_id: &str,
    complexity: Option<ModelRoutingComplexity>,
) -> (String, String) {
    if let Some(model) = runtime_config
        .phase_model_override(phase_id)
        .map(canonical_model_id)
        .filter(|value| !value.is_empty())
    {
        return (model, "runtime_override".to_string());
    }

    let phase_env_key = format!("AO_PHASE_MODEL_{}", env_phase_key(phase_id));
    if let Ok(value) = std::env::var(&phase_env_key) {
        let model = canonical_model_id(&value);
        if !model.is_empty() {
            return (model, "env_phase_override".to_string());
        }
    }

    if let Ok(value) = std::env::var("AO_PHASE_MODEL") {
        let model = canonical_model_id(&value);
        if !model.is_empty() {
            return (model, "env_global_override".to_string());
        }
    }

    (
        default_primary_model_for_phase(phase_id, complexity).to_string(),
        "protocol_default".to_string(),
    )
}

fn resolve_phase_tool_for_diagnostics(
    runtime_config: &orchestrator_core::AgentRuntimeConfig,
    phase_id: &str,
    model_id: &str,
) -> (String, String) {
    if let Some(tool) = runtime_config
        .phase_tool_override(phase_id)
        .map(normalize_tool_id)
        .filter(|value| !value.is_empty())
    {
        return (tool, "runtime_override".to_string());
    }

    let phase_env_key = format!("AO_PHASE_TOOL_{}", env_phase_key(phase_id));
    if let Ok(value) = std::env::var(&phase_env_key) {
        let tool = normalize_tool_id(&value);
        if !tool.is_empty() {
            return (tool, "env_phase_override".to_string());
        }
    }

    if let Ok(value) = std::env::var("AO_PHASE_TOOL") {
        let tool = normalize_tool_id(&value);
        if !tool.is_empty() {
            return (tool, "env_global_override".to_string());
        }
    }

    (
        tool_for_model_id(model_id).to_string(),
        "model_provider_default".to_string(),
    )
}

fn phase_fallback_models_from_env_for_diagnostics(phase_id: &str) -> Vec<String> {
    let phase_key = env_phase_key(phase_id);
    let phase_specific = format!("AO_PHASE_FALLBACK_MODELS_{phase_key}");
    if let Ok(value) = std::env::var(&phase_specific) {
        let parsed = parse_model_list(&value);
        if !parsed.is_empty() {
            return parsed;
        }
    }

    std::env::var("AO_PHASE_FALLBACK_MODELS")
        .ok()
        .map(|value| parse_model_list(&value))
        .unwrap_or_default()
}

fn enforce_write_capable_target_for_diagnostics(
    tool_id: String,
    model_id: String,
) -> (String, String, bool) {
    let normalized_tool_id = normalize_tool_id(&tool_id);
    if !parse_env_bool("AO_ALLOW_NON_EDITING_PHASE_TOOL")
        && !tool_supports_repository_writes(&normalized_tool_id)
    {
        let fallback_model = std::env::var("AO_PHASE_MODEL_FILE_EDIT")
            .ok()
            .map(|value| canonical_model_id(&value))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "gpt-5.3-codex".to_string());
        let fallback_tool = std::env::var("AO_PHASE_TOOL_FILE_EDIT")
            .ok()
            .map(|value| normalize_tool_id(&value))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "codex".to_string());
        return (fallback_tool, fallback_model, true);
    }
    (normalized_tool_id, model_id, false)
}

fn resolve_phase_targets_for_diagnostics(
    runtime_config: &orchestrator_core::AgentRuntimeConfig,
    phase_id: &str,
    complexity: Option<ModelRoutingComplexity>,
) -> Vec<RuntimeDiagnosticTarget> {
    let (primary_model, model_source) =
        resolve_phase_model_for_diagnostics(runtime_config, phase_id, complexity);
    let (primary_tool, tool_source) =
        resolve_phase_tool_for_diagnostics(runtime_config, phase_id, &primary_model);
    let (primary_tool, primary_model, write_adjusted) =
        enforce_write_capable_target_for_diagnostics(primary_tool, primary_model);
    let primary_source = if write_adjusted {
        "write_capability_fallback".to_string()
    } else if model_source == tool_source {
        model_source
    } else {
        format!("{model_source}+{tool_source}")
    };

    let mut candidate_models = vec![(primary_model.clone(), primary_source)];
    candidate_models.extend(
        runtime_config
            .phase_fallback_models(phase_id)
            .into_iter()
            .filter(|value| !value.trim().is_empty())
            .map(|value| (canonical_model_id(&value), "runtime_fallback".to_string())),
    );
    candidate_models.extend(
        phase_fallback_models_from_env_for_diagnostics(phase_id)
            .into_iter()
            .map(|value| (value, "env_fallback".to_string())),
    );
    candidate_models.extend(
        default_fallback_models_for_phase(phase_id, complexity)
            .into_iter()
            .map(canonical_model_id)
            .filter(|value| !value.is_empty())
            .map(|value| (value, "protocol_default_fallback".to_string())),
    );

    let mut targets = Vec::new();
    let mut seen_models = BTreeSet::new();
    for (candidate_model, source) in candidate_models {
        if candidate_model.trim().is_empty() {
            continue;
        }

        let model_key = candidate_model.to_ascii_lowercase();
        if !seen_models.insert(model_key) {
            continue;
        }

        let (tool_id, model_id, write_adjusted) =
            if candidate_model.eq_ignore_ascii_case(&primary_model) {
                (primary_tool.clone(), primary_model.clone(), false)
            } else {
                enforce_write_capable_target_for_diagnostics(
                    tool_for_model_id(&candidate_model).to_string(),
                    candidate_model,
                )
            };

        targets.push(RuntimeDiagnosticTarget {
            index: targets.len(),
            tool: tool_id,
            model: model_id,
            source: if write_adjusted {
                "write_capability_fallback".to_string()
            } else {
                source
            },
        });
    }

    if targets.is_empty() {
        targets.push(RuntimeDiagnosticTarget {
            index: 0,
            tool: primary_tool,
            model: primary_model,
            source: "protocol_default".to_string(),
        });
    }

    targets
}

fn phase_runner_attempts_for_diagnostics() -> usize {
    std::env::var("AO_PHASE_RUN_ATTEMPTS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(3)
        .clamp(1, 10)
}

fn workflow_agent_runtime_diagnostics_payload(
    project_root: &str,
    phase_id: &str,
    pipeline_id: Option<&str>,
    requested_complexity: Option<&str>,
) -> Result<Value> {
    let normalized_phase = phase_id.trim();
    if normalized_phase.is_empty() {
        return Err(anyhow!("phase must not be empty"));
    }

    let runtime_config = orchestrator_core::load_agent_runtime_config(Path::new(project_root))?;
    let explicit_complexity = parse_phase_complexity_arg(requested_complexity)?;
    let complexity = explicit_complexity.or_else(|| {
        let phase_key = env_phase_key(normalized_phase);
        let phase_specific = format!("AO_PHASE_COMPLEXITY_{phase_key}");
        std::env::var(&phase_specific)
            .ok()
            .and_then(|value| ModelRoutingComplexity::parse(&value))
            .or_else(|| {
                std::env::var("AO_PHASE_COMPLEXITY")
                    .ok()
                    .and_then(|value| ModelRoutingComplexity::parse(&value))
            })
    });

    let execution_targets =
        resolve_phase_targets_for_diagnostics(&runtime_config, normalized_phase, complexity);

    let mut warnings = Vec::new();
    let auth_profile_chains = execution_targets
        .iter()
        .map(|target| {
            let profiles = runtime_config
                .resolve_phase_auth_profiles_for_provider(normalized_phase, &target.tool);
            if profiles.is_empty() {
                let required = required_api_keys_for_tool(&target.tool);
                let missing = required
                    .iter()
                    .filter(|env_key| {
                        std::env::var(env_key)
                            .ok()
                            .map(|value| value.trim().is_empty())
                            .unwrap_or(true)
                    })
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>();
                if !missing.is_empty() {
                    warnings.push(format!(
                        "target {} ({}/{}) missing env keys: {}",
                        target.index,
                        target.tool,
                        target.model,
                        missing.join(", ")
                    ));
                }
                return serde_json::json!({
                    "target_index": target.index,
                    "tool": target.tool,
                    "model": target.model,
                    "profiles": [{
                        "profile_id": Value::Null,
                        "provider": target.tool,
                        "readiness": {
                            "ready": missing.is_empty(),
                            "missing_env_keys": missing,
                        },
                    }],
                });
            }

            let profile_entries = profiles
                .into_iter()
                .map(|(profile_id, profile)| {
                    let mut missing_source_env = profile
                        .env_map
                        .iter()
                        .filter_map(|(_required_env, source_env)| {
                            let source = source_env.trim();
                            if source.is_empty() {
                                return None;
                            }
                            let is_missing = std::env::var(source)
                                .ok()
                                .map(|value| value.trim().is_empty())
                                .unwrap_or(true);
                            if is_missing {
                                return Some(source.to_string());
                            }
                            None
                        })
                        .collect::<Vec<_>>();
                    missing_source_env.sort();
                    missing_source_env.dedup();
                    if !missing_source_env.is_empty() {
                        warnings.push(format!(
                            "auth profile '{}' missing source env vars: {}",
                            profile_id,
                            missing_source_env.join(", ")
                        ));
                    }

                    let mut required_env_keys = profile.env_map.keys().cloned().collect::<Vec<_>>();
                    required_env_keys.sort();
                    serde_json::json!({
                        "profile_id": profile_id,
                        "provider": profile.provider,
                        "readiness": {
                            "ready": missing_source_env.is_empty(),
                            "required_env_keys": required_env_keys,
                            "missing_source_env": missing_source_env,
                        },
                    })
                })
                .collect::<Vec<_>>();

            serde_json::json!({
                "target_index": target.index,
                "tool": target.tool,
                "model": target.model,
                "profiles": profile_entries,
            })
        })
        .collect::<Vec<_>>();

    let max_attempts = runtime_config
        .phase_max_attempts(normalized_phase)
        .unwrap_or_else(phase_runner_attempts_for_diagnostics);

    Ok(serde_json::json!({
        "phase_id": normalized_phase,
        "pipeline_id": pipeline_id
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        "execution_targets": execution_targets.iter().map(|target| serde_json::json!({
            "index": target.index,
            "tool": target.tool,
            "model": target.model,
            "source": target.source,
        })).collect::<Vec<_>>(),
        "auth_profile_chains": auth_profile_chains,
        "retry_policy": {
            "max_attempts": max_attempts,
            "backoff_base_ms": PHASE_RETRY_BACKOFF_BASE_MS,
            "backoff_cap_ms": PHASE_RETRY_BACKOFF_CAP_MS,
        },
        "warnings": warnings,
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
            anyhow!("legacy workflow config not found (expected workflow-config.json)")
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
                        timeout_secs: profile.timeout_secs,
                        max_attempts: profile.max_attempts,
                        auth_profile_chain: Vec::new(),
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
                timeout_secs: settings.timeout_secs,
                max_attempts: settings.max_attempts,
                auth_profile_chain: Vec::new(),
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
            ensure_destructive_confirmation(args.confirm.as_deref(), &args.id, "workflow pause")?;
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
            ensure_destructive_confirmation(args.confirm.as_deref(), &args.id, "workflow cancel")?;
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
                    serde_json::from_str(&args.input_json)?;
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
                    serde_json::from_str(&args.input_json)?;
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
            WorkflowAgentRuntimeCommand::Diagnostics(args) => print_value(
                workflow_agent_runtime_diagnostics_payload(
                    project_root,
                    &args.phase,
                    args.pipeline_id.as_deref(),
                    args.complexity.as_deref(),
                )?,
                json,
            ),
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
    fn diagnostics_payload_includes_targets_retry_policy_and_profiles() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut config = orchestrator_core::builtin_agent_runtime_config();
        config.auth_profiles.insert(
            "codex-primary".to_string(),
            orchestrator_core::ProviderAuthProfile {
                provider: "codex".to_string(),
                env_map: BTreeMap::from([(
                    "OPENAI_API_KEY".to_string(),
                    "OPENAI_PRIMARY_KEY".to_string(),
                )]),
                priority: Some(1),
                enabled: true,
            },
        );
        config
            .agents
            .get_mut("implementation")
            .expect("implementation profile")
            .auth_profile_chain = vec!["codex-primary".to_string()];
        orchestrator_core::write_agent_runtime_config(temp.path(), &config)
            .expect("write runtime config");

        std::env::set_var("OPENAI_PRIMARY_KEY", "test-key");
        let payload = workflow_agent_runtime_diagnostics_payload(
            temp.path().to_str().expect("path"),
            "implementation",
            None,
            Some("medium"),
        )
        .expect("diagnostics payload");
        std::env::remove_var("OPENAI_PRIMARY_KEY");

        let phase_id = payload.get("phase_id").and_then(Value::as_str);
        assert_eq!(phase_id, Some("implementation"));
        let targets = payload
            .get("execution_targets")
            .and_then(Value::as_array)
            .expect("execution targets");
        assert!(!targets.is_empty());
        let retry_policy = payload
            .get("retry_policy")
            .and_then(Value::as_object)
            .expect("retry policy");
        assert_eq!(
            retry_policy.get("backoff_base_ms").and_then(Value::as_u64),
            Some(PHASE_RETRY_BACKOFF_BASE_MS)
        );
        assert_eq!(
            retry_policy.get("backoff_cap_ms").and_then(Value::as_u64),
            Some(PHASE_RETRY_BACKOFF_CAP_MS)
        );
        let auth_chains = payload
            .get("auth_profile_chains")
            .and_then(Value::as_array)
            .expect("auth profile chains");
        assert!(!auth_chains.is_empty());
    }

    #[test]
    fn diagnostics_payload_reports_missing_profile_source_env_readiness() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut config = orchestrator_core::builtin_agent_runtime_config();
        let source_env = "AO_TEST_AUTH_PROFILE_MISSING_SOURCE_023";
        std::env::remove_var(source_env);

        config.auth_profiles.insert(
            "codex-primary".to_string(),
            orchestrator_core::ProviderAuthProfile {
                provider: "codex".to_string(),
                env_map: BTreeMap::from([("OPENAI_API_KEY".to_string(), source_env.to_string())]),
                priority: Some(1),
                enabled: true,
            },
        );
        config
            .agents
            .get_mut("implementation")
            .expect("implementation profile")
            .auth_profile_chain = vec!["codex-primary".to_string()];
        orchestrator_core::write_agent_runtime_config(temp.path(), &config)
            .expect("write runtime config");

        let payload = workflow_agent_runtime_diagnostics_payload(
            temp.path().to_str().expect("path"),
            "implementation",
            None,
            Some("medium"),
        )
        .expect("diagnostics payload");

        let warnings = payload
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings array")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(warnings.iter().any(|warning| {
            warning.contains("auth profile 'codex-primary' missing source env vars")
                && warning.contains(source_env)
        }));

        let auth_chains = payload
            .get("auth_profile_chains")
            .and_then(Value::as_array)
            .expect("auth profile chains");
        let first_profile = auth_chains
            .first()
            .and_then(|entry| entry.get("profiles"))
            .and_then(Value::as_array)
            .and_then(|profiles| profiles.first())
            .expect("first auth profile");
        let readiness = first_profile
            .get("readiness")
            .and_then(Value::as_object)
            .expect("readiness object");
        assert_eq!(readiness.get("ready").and_then(Value::as_bool), Some(false));
        let missing = readiness
            .get("missing_source_env")
            .and_then(Value::as_array)
            .expect("missing source env list");
        assert!(missing
            .iter()
            .any(|value| value.as_str() == Some(source_env)));
        let required = readiness
            .get("required_env_keys")
            .and_then(Value::as_array)
            .expect("required env keys");
        assert!(required
            .iter()
            .any(|value| value.as_str() == Some("OPENAI_API_KEY")));
    }

    #[test]
    fn diagnostics_payload_reports_invalid_complexity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = orchestrator_core::builtin_agent_runtime_config();
        orchestrator_core::write_agent_runtime_config(temp.path(), &config)
            .expect("write runtime config");

        let error = workflow_agent_runtime_diagnostics_payload(
            temp.path().to_str().expect("path"),
            "implementation",
            None,
            Some("extreme"),
        )
        .expect_err("invalid complexity should fail");
        assert!(error.to_string().contains("invalid complexity"));
    }
}
