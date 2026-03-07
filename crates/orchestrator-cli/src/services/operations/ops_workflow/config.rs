use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::not_found_error;
use super::project_state_dir;

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

pub(crate) fn workflow_config_path(project_root: &str) -> PathBuf {
    orchestrator_core::workflow_config_path(Path::new(project_root))
}

pub(crate) fn agent_runtime_path(project_root: &str) -> PathBuf {
    orchestrator_core::agent_runtime_config_path(Path::new(project_root))
}

pub(super) fn manual_approvals_path(project_root: &str) -> PathBuf {
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

pub(crate) fn get_state_machine_payload(project_root: &str) -> Result<Value> {
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

pub(crate) fn validate_state_machine_payload(project_root: &str) -> Value {
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

pub(crate) fn set_state_machine_payload(project_root: &str, input_json: &str) -> Result<Value> {
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

pub(crate) fn get_agent_runtime_payload(project_root: &str) -> Value {
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

pub(crate) fn validate_agent_runtime_payload(project_root: &str) -> Value {
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

pub(crate) fn set_agent_runtime_payload(project_root: &str, input_json: &str) -> Result<Value> {
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

pub(crate) fn get_workflow_config_payload(project_root: &str) -> Value {
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

pub(crate) fn validate_workflow_config_payload(project_root: &str) -> Value {
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

pub(crate) fn compile_yaml_workflows_payload(project_root: &str) -> Result<Value> {
    match orchestrator_core::compile_and_write_yaml_workflows(Path::new(project_root))? {
        Some(result) => {
            let source_files: Vec<String> = result
                .source_files
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            Ok(serde_json::json!({
                "compiled": true,
                "source_files": source_files,
                "output_path": result.output_path.display().to_string(),
                "pipelines": result.config.pipelines.iter().map(|p| &p.id).collect::<Vec<_>>(),
                "phase_definitions": result.config.phase_definitions.len(),
                "agent_profiles": result.config.agent_profiles.len(),
                "hash": orchestrator_core::workflow_config_hash(&result.config),
            }))
        }
        None => Ok(serde_json::json!({
            "compiled": false,
            "message": "no YAML workflow files found in .ao/workflows/ or .ao/workflows.yaml",
        })),
    }
}

pub(super) fn title_case_phase_id(phase_id: &str) -> String {
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

pub(crate) fn migrate_v1_to_v2(project_root: &str) -> Result<Value> {
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
                .map(|phase| orchestrator_core::PipelinePhaseEntry::Simple(phase.to_owned()))
                .collect(),
            post_success: None,
            variables: Vec::new(),
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
                        role: None,
                        mcp_servers: Vec::new(),
                        tool_policy: Default::default(),
                        skills: Vec::new(),
                        capabilities: BTreeMap::new(),
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
                        max_continuations: None,
                        mcp_server_configs: None,
                        structured_capabilities: None,
                        project_overrides: None,
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
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            command: None,
            manual: None,
            system_prompt: None,
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
                max_continuations: None,
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
                capabilities: None,
                output_contract,
                output_json_schema,
                decision_contract: None,
                retry: None,
                command: None,
                manual: None,
                system_prompt: None,
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
