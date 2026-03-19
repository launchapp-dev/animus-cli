use crate::config_context::RuntimeConfigContext;
use crate::ipc::{
    build_runtime_contract_with_resume, collect_json_payload_lines, connect_runner, event_matches_run,
    persist_run_event, run_dir as ipc_run_dir, runner_config_dir, write_json_line,
};
use crate::payload_traversal::{parse_commit_message_from_text, parse_phase_decision_from_text};
use crate::phase_command::{
    build_command_phase_decision, build_command_result_payload, run_workflow_phase_with_command,
    CommandExecutionContext,
};
use crate::phase_failover::PhaseFailureClassifier;
use crate::phase_git::commit_implementation_changes;
use crate::phase_output::persist_phase_output;
use crate::phase_prompt::{
    phase_requires_commit_message_with_ctx, phase_result_kind_for_ctx, render_phase_prompt_with_ctx_overrides,
    PhasePromptInputs, PhaseRenderParams,
};
use crate::phase_targets::PhaseTargetPlanner;
use crate::runtime_contract::{
    apply_phase_capability_launch_flags, check_required_env_for_phase_mcp_servers, inject_agent_tool_policy,
    inject_default_stdio_mcp, inject_named_mcp_servers, inject_project_mcp_servers,
    inject_response_schema_into_launch_args, inject_workflow_mcp_servers, phase_output_json_schema_for,
    phase_response_json_schema_for, set_mcp_tool_policy,
};
use crate::runtime_support::{
    inject_cli_launch_overrides, phase_max_continuations, phase_runner_attempts, WorkflowPhaseRuntimeSettings,
};
use crate::skill_dispatch;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use orchestrator_config::{skill_resolution::ResolvedSkill, SkillApplicationResult};
use orchestrator_core::ServiceHub;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::time::sleep;
use tracing::{debug, info, warn};
use uuid::Uuid;

use protocol::{canonical_model_id, AgentRunEvent, AgentRunRequest, ModelId, RunId, PROTOCOL_VERSION};

#[derive(Debug, Clone, Default)]
pub struct PhaseExecuteOverrides {
    pub tool: Option<String>,
    pub model: Option<String>,
    pub rework_context: Option<String>,
}

#[derive(Default)]
pub struct CliPhaseExecutor;

fn debug_mcp_stdio_enabled() -> bool {
    protocol::parse_env_bool("AO_DEBUG_MCP_STDIO")
}

#[async_trait]
impl orchestrator_core::PhaseExecutor for CliPhaseExecutor {
    async fn execute_phase(
        &self,
        request: orchestrator_core::PhaseExecutionRequest,
    ) -> Result<orchestrator_core::PhaseExecutionResult> {
        let hub = orchestrator_core::FileServiceHub::new(&request.project_root)?;
        let (subject_id, subject_title, subject_description, task_complexity) =
            if let Ok(task) = hub.tasks().get(&request.task_id).await {
                (task.id.clone(), task.title.clone(), task.description.clone(), Some(task.complexity))
            } else if let Ok(req) = hub.planning().get_requirement(&request.task_id).await {
                (req.id.clone(), req.title.clone(), req.description.clone(), None)
            } else {
                (request.task_id.clone(), request.task_id.clone(), String::new(), None)
            };

        let execution_cwd = if Path::new(&request.config_dir).is_dir() {
            request.config_dir.clone()
        } else {
            request.project_root.clone()
        };

        let phase_timeout_secs = request.timeout;

        let overrides = if request.tool_override.is_some() || request.model_override.is_some() {
            Some(PhaseExecuteOverrides {
                tool: request.tool_override,
                model: request.model_override,
                rework_context: None,
            })
        } else {
            None
        };

        let routing = protocol::PhaseRoutingConfig::default();
        let run_result = run_workflow_phase(&PhaseRunParams {
            project_root: &request.project_root,
            execution_cwd: &execution_cwd,
            workflow_id: &request.workflow_ref,
            workflow_ref: &request.workflow_ref,
            subject_id: &subject_id,
            subject_title: &subject_title,
            subject_description: &subject_description,
            task_complexity,
            phase_id: &request.phase_id,
            phase_attempt: 0,
            overrides: overrides.as_ref(),
            pipeline_vars: None,
            dispatch_input: None,
            schedule_input: None,
            routing: &routing,
            phase_timeout_secs,
        })
        .await;

        let run_result = run_result?;
        let output_log = serde_json::to_string_pretty(&run_result)?;

        let mut commit_message = None;
        let (verdict, exit_code, error) = phase_execution_result_values(
            &request.project_root,
            &request.workflow_ref,
            &request.phase_id,
            &run_result.outcome,
        );

        if let PhaseExecutionOutcome::Completed {
            commit_message: resolved_commit,
            phase_decision: Some(decision),
            ..
        } = &run_result.outcome
        {
            commit_message = resolved_commit.clone().or_else(|| decision.commit_message.clone());
        }

        Ok(orchestrator_core::PhaseExecutionResult { exit_code, verdict, output_log, error, commit_message })
    }
}

fn yaml_verdict_target(
    project_root: &str,
    workflow_ref: &str,
    phase_id: &str,
    verdict: &str,
    requested_target: Option<&str>,
) -> Option<String> {
    let loaded_config = orchestrator_core::load_workflow_config_or_default(Path::new(project_root));
    let routing = orchestrator_core::resolve_workflow_verdict_routing(&loaded_config.config, Some(workflow_ref));
    let transition = routing.iter().find(|(candidate, _)| candidate.eq_ignore_ascii_case(phase_id)).and_then(
        |(_, transitions)| {
            transitions
                .iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(verdict))
                .map(|(_, transition)| transition)
        },
    )?;
    let workflow_phases =
        orchestrator_core::resolve_phase_plan_for_workflow_ref(Some(Path::new(project_root)), Some(workflow_ref))
            .ok()?;
    let requested_target = requested_target.map(str::trim).filter(|target| !target.is_empty());
    if transition.allow_agent_target {
        let requested_target = requested_target?;
        if (transition.allowed_targets.is_empty()
            || transition.allowed_targets.iter().any(|allowed| allowed.eq_ignore_ascii_case(requested_target)))
            && workflow_phases.iter().any(|phase| phase.eq_ignore_ascii_case(requested_target))
        {
            return workflow_phases.into_iter().find(|phase| phase.eq_ignore_ascii_case(requested_target));
        }
    }
    let target = transition.target.trim();
    if target.is_empty() {
        None
    } else {
        Some(target.to_string())
    }
}

fn phase_execution_result_values(
    project_root: &str,
    workflow_ref: &str,
    phase_id: &str,
    outcome: &PhaseExecutionOutcome,
) -> (orchestrator_core::PhaseVerdict, i32, Option<String>) {
    match outcome {
        PhaseExecutionOutcome::Completed { phase_decision, .. } => match phase_decision {
            Some(decision) => match decision.verdict {
                orchestrator_core::PhaseDecisionVerdict::Advance => (orchestrator_core::PhaseVerdict::Advance, 0, None),
                orchestrator_core::PhaseDecisionVerdict::Rework => (
                    orchestrator_core::PhaseVerdict::Rework {
                        target_phase: yaml_verdict_target(
                            project_root,
                            workflow_ref,
                            phase_id,
                            "rework",
                            decision.target_phase.as_deref(),
                        )
                        .unwrap_or_else(|| phase_id.to_string()),
                    },
                    0,
                    None,
                ),
                orchestrator_core::PhaseDecisionVerdict::Skip => (orchestrator_core::PhaseVerdict::Skip, 0, None),
                orchestrator_core::PhaseDecisionVerdict::Fail => {
                    let reason = if decision.reason.trim().is_empty() {
                        "phase verdict fail".to_string()
                    } else {
                        decision.reason.clone()
                    };
                    (orchestrator_core::PhaseVerdict::Failed { reason: reason.clone() }, 1, Some(reason))
                }
                orchestrator_core::PhaseDecisionVerdict::Unknown => {
                    let reason = "phase verdict unknown".to_string();
                    (orchestrator_core::PhaseVerdict::Failed { reason: reason.clone() }, 1, Some(reason))
                }
            },
            None => (orchestrator_core::PhaseVerdict::Advance, 0, None),
        },
        PhaseExecutionOutcome::ManualPending { instructions, .. } => {
            let reason = format!("manual review required: {instructions}");
            (orchestrator_core::PhaseVerdict::Failed { reason: reason.clone() }, 1, Some(reason))
        }
    }
}

fn runtime_contract_string_array(contract: &Value, pointer: &str) -> Vec<String> {
    contract
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn runtime_contract_additional_server_names(contract: &Value) -> Vec<String> {
    contract
        .pointer("/mcp/additional_servers")
        .and_then(Value::as_object)
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default()
}

pub fn load_agent_runtime_config(project_root: &str) -> orchestrator_core::AgentRuntimeConfig {
    orchestrator_core::load_agent_runtime_config_or_default(Path::new(project_root))
}

fn load_agent_runtime_config_strict(project_root: &str) -> Result<orchestrator_core::LoadedAgentRuntimeConfig> {
    orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(project_root))
}

fn load_workflow_config_strict(project_root: &str) -> Result<orchestrator_core::LoadedWorkflowConfig> {
    orchestrator_core::load_workflow_config_with_metadata(Path::new(project_root))
}

fn hash_serializable<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseExecutionMetadata {
    pub phase_id: String,
    pub phase_mode: String,
    pub phase_definition_hash: String,
    pub agent_runtime_config_hash: String,
    pub agent_runtime_schema: String,
    pub agent_runtime_version: u32,
    pub agent_runtime_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_profile_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_model: Option<String>,
    #[serde(default)]
    pub effective_capabilities: protocol::PhaseCapabilities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_skills: Vec<ResolvedSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_skills: Vec<ResolvedSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_application: Option<SkillApplicationResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseExecutionSignal {
    pub event_type: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseRunResult {
    pub outcome: PhaseExecutionOutcome,
    pub metadata: PhaseExecutionMetadata,
    #[serde(default)]
    pub signals: Vec<PhaseExecutionSignal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum PhaseExecutionOutcome {
    Completed {
        commit_message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase_decision: Option<orchestrator_core::PhaseDecision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_payload: Option<Value>,
    },
    ManualPending {
        instructions: String,
        approval_note_required: bool,
    },
}

fn outcome_verdict(outcome: &PhaseExecutionOutcome) -> orchestrator_core::PhaseDecisionVerdict {
    match outcome {
        PhaseExecutionOutcome::Completed { phase_decision, .. } => phase_decision
            .as_ref()
            .map(|decision| decision.verdict)
            .unwrap_or(orchestrator_core::PhaseDecisionVerdict::Advance),
        PhaseExecutionOutcome::ManualPending { .. } => orchestrator_core::PhaseDecisionVerdict::Advance,
    }
}

fn routing_complexity(
    task_complexity: Option<orchestrator_core::Complexity>,
) -> Option<protocol::ModelRoutingComplexity> {
    task_complexity.map(|complexity| match complexity {
        orchestrator_core::Complexity::Low => protocol::ModelRoutingComplexity::Low,
        orchestrator_core::Complexity::Medium => protocol::ModelRoutingComplexity::Medium,
        orchestrator_core::Complexity::High => protocol::ModelRoutingComplexity::High,
    })
}

pub(crate) fn validate_basic_json_schema(instance: &Value, schema: &Value) -> Result<()> {
    let schema_object = schema.as_object().ok_or_else(|| anyhow!("schema must be a JSON object"))?;

    if let Some(required_fields) = schema_object.get("required").and_then(Value::as_array) {
        let instance_object = instance.as_object().ok_or_else(|| anyhow!("instance must be a JSON object"))?;
        for required in required_fields {
            let Some(field) = required.as_str() else {
                continue;
            };
            if !instance_object.contains_key(field) {
                return Err(anyhow!("schema validation failed: missing required field '{}'", field));
            }
        }
    }

    if let Some(properties) = schema_object.get("properties").and_then(Value::as_object) {
        let instance_object = instance.as_object().ok_or_else(|| anyhow!("instance must be a JSON object"))?;
        for (key, rule) in properties {
            let Some(value) = instance_object.get(key) else {
                continue;
            };
            if let Some(expected_type) = rule.get("type").and_then(Value::as_str) {
                if !validate_schema_type(expected_type, value) {
                    return Err(anyhow!("schema validation failed: field '{}' must be type '{}'", key, expected_type));
                }
            }
            if let Some(constant) = rule.get("const") {
                if value != constant {
                    return Err(anyhow!("schema validation failed: field '{}' must equal {}", key, constant));
                }
            }
        }
    }

    Ok(())
}

fn validate_schema_type(expected_type: &str, value: &Value) -> bool {
    match expected_type {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

pub async fn run_workflow_phase_attempt(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    request: &AgentRunRequest,
) -> Result<PhaseExecutionOutcome> {
    let ctx = RuntimeConfigContext::load(project_root);
    let parse_commit_message = phase_requires_commit_message_with_ctx(&ctx, phase_id);
    let config_dir = runner_config_dir(Path::new(project_root));
    let request_mcp_stdio_command = request
        .context
        .pointer("/runtime_contract/mcp/stdio/command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let request_mcp_stdio_args = request
        .context
        .pointer("/runtime_contract/mcp/stdio/args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if debug_mcp_stdio_enabled() {
        eprintln!(
            "[ao][debug][mcp-stdio] dispatch workflow={} phase={} run_id={} command={:?} args={:?}",
            workflow_id, phase_id, request.run_id.0, request_mcp_stdio_command, request_mcp_stdio_args
        );
    }
    info!(
        workflow_id = %workflow_id,
        phase_id = %phase_id,
        run_id = %request.run_id.0,
        request_mcp_stdio_command = ?request_mcp_stdio_command,
        request_mcp_stdio_args = ?request_mcp_stdio_args,
        "Dispatching workflow phase request to agent runner"
    );
    let stream = connect_runner(&config_dir)
        .await
        .with_context(|| format!("failed to connect runner for workflow {} phase {}", workflow_id, phase_id))?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, request).await?;

    let parse_phase_decision = ctx.phase_decision_contract(phase_id).is_some();
    let expected_result_kind = phase_result_kind_for_ctx(&ctx, phase_id);
    let event_run_dir = ipc_run_dir(project_root, &request.run_id, None);

    process_phase_event_stream(
        tokio::io::BufReader::new(read_half).lines(),
        &request.run_id,
        workflow_id,
        phase_id,
        parse_commit_message,
        parse_phase_decision,
        &expected_result_kind,
        Some(&event_run_dir),
    )
    .await
}

async fn process_phase_event_stream<R: AsyncBufRead + Unpin>(
    mut lines: tokio::io::Lines<R>,
    run_id: &RunId,
    workflow_id: &str,
    phase_id: &str,
    parse_commit_message: bool,
    parse_phase_decision: bool,
    expected_result_kind: &str,
    event_run_dir: Option<&std::path::Path>,
) -> Result<PhaseExecutionOutcome> {
    let mut pending_commit_message: Option<String> = None;
    let mut pending_phase_decision: Option<orchestrator_core::PhaseDecision> = None;
    let mut pending_result_payload: Option<Value> = None;
    let mut provider_exhaustion_reason: Option<String> = None;
    let mut diagnostics = VecDeque::new();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, run_id) {
            continue;
        }

        if let Some(dir) = event_run_dir {
            let _ = persist_run_event(dir, &event);
        }

        match event {
            AgentRunEvent::OutputChunk { text, .. } => {
                if provider_exhaustion_reason.is_none() {
                    provider_exhaustion_reason = PhaseFailureClassifier::provider_exhaustion_reason_from_text(&text);
                }
                PhaseFailureClassifier::push_phase_diagnostic_line(&mut diagnostics, &text);
                if parse_commit_message && pending_commit_message.is_none() {
                    pending_commit_message = parse_commit_message_from_text(&text);
                }
                if parse_phase_decision && pending_phase_decision.is_none() {
                    if let Some(decision) = parse_phase_decision_from_text(&text, phase_id) {
                        if pending_commit_message.is_none() {
                            if let Some(ref cm) = decision.commit_message {
                                pending_commit_message = Some(cm.clone());
                            }
                        }
                        pending_phase_decision = Some(decision);
                    }
                }
                if pending_result_payload.is_none() {
                    pending_result_payload = parse_result_payload_from_text(&text, expected_result_kind);
                }
                if pending_result_payload.is_none() && parse_phase_decision {
                    pending_result_payload = parse_decision_payload_from_text(&text, phase_id);
                }
            }
            AgentRunEvent::Thinking { content, .. } => {
                if provider_exhaustion_reason.is_none() {
                    provider_exhaustion_reason = PhaseFailureClassifier::provider_exhaustion_reason_from_text(&content);
                }
                PhaseFailureClassifier::push_phase_diagnostic_line(&mut diagnostics, &content);
                if parse_commit_message && pending_commit_message.is_none() {
                    pending_commit_message = parse_commit_message_from_text(&content);
                }
                if parse_phase_decision && pending_phase_decision.is_none() {
                    if let Some(decision) = parse_phase_decision_from_text(&content, phase_id) {
                        if pending_commit_message.is_none() {
                            if let Some(ref cm) = decision.commit_message {
                                pending_commit_message = Some(cm.clone());
                            }
                        }
                        pending_phase_decision = Some(decision);
                    }
                }
                if pending_result_payload.is_none() {
                    pending_result_payload = parse_result_payload_from_text(&content, expected_result_kind);
                }
                if pending_result_payload.is_none() && parse_phase_decision {
                    pending_result_payload = parse_decision_payload_from_text(&content, phase_id);
                }
            }
            AgentRunEvent::Error { error, .. } => {
                PhaseFailureClassifier::push_phase_diagnostic_line(&mut diagnostics, &error);
                let exhaustion_reason = provider_exhaustion_reason
                    .clone()
                    .or_else(|| PhaseFailureClassifier::provider_exhaustion_reason_from_text(&error));
                return Err(anyhow!(
                    "workflow {} phase {} error: {}{}",
                    workflow_id,
                    phase_id,
                    error,
                    exhaustion_reason.map(|reason| format!(" (provider_exhausted: {reason})")).unwrap_or_default()
                ));
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    let diagnostics_summary = PhaseFailureClassifier::summarize_phase_diagnostics(&diagnostics);
                    let exhaustion_reason = provider_exhaustion_reason.clone().or_else(|| {
                        diagnostics_summary
                            .as_deref()
                            .and_then(PhaseFailureClassifier::provider_exhaustion_reason_from_text)
                    });
                    return Err(anyhow!(
                        "workflow {} phase {} exited with code {:?}{}{}",
                        workflow_id,
                        phase_id,
                        exit_code,
                        exhaustion_reason.map(|reason| format!(" (provider_exhausted: {reason})")).unwrap_or_default(),
                        diagnostics_summary.map(|summary| format!("; diagnostics: {summary}")).unwrap_or_default(),
                    ));
                }
                return Ok(PhaseExecutionOutcome::Completed {
                    commit_message: pending_commit_message,
                    phase_decision: pending_phase_decision,
                    result_payload: pending_result_payload,
                });
            }
            AgentRunEvent::ToolCall { tool_info, .. } => {
                PhaseFailureClassifier::push_phase_diagnostic_line(
                    &mut diagnostics,
                    &format!("tool_call: {}", tool_info.tool_name),
                );
            }
            AgentRunEvent::Artifact { .. } => {}
            _ => {}
        }
    }

    let diagnostics_suffix = PhaseFailureClassifier::summarize_phase_diagnostics(&diagnostics)
        .map(|summary| format!("; diagnostics: {summary}"))
        .unwrap_or_default();
    Err(anyhow!(
        "runner disconnected before workflow {} phase {} completed{}",
        workflow_id,
        phase_id,
        diagnostics_suffix
    ))
}

fn parse_result_payload_from_text(text: &str, expected_kind: &str) -> Option<Value> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(result) = parse_result_payload_from_payload(&payload, expected_kind) {
            return Some(result);
        }
    }
    None
}

fn parse_result_payload_from_payload(payload: &Value, expected_kind: &str) -> Option<Value> {
    match payload {
        Value::Array(items) => items.iter().find_map(|item| parse_result_payload_from_payload(item, expected_kind)),
        Value::Object(object) => {
            let kind = object.get("kind").and_then(Value::as_str).unwrap_or_default();
            if kind.eq_ignore_ascii_case(expected_kind) {
                return Some(payload.clone());
            }
            for key in ["proposal", "data", "payload", "result", "output", "item"] {
                if let Some(value) = object.get(key) {
                    if let Some(result) = parse_result_payload_from_payload(value, expected_kind) {
                        return Some(result);
                    }
                }
            }
            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    if let Some(result) = parse_result_payload_from_text(raw, expected_kind) {
                        return Some(result);
                    }
                }
            }
            None
        }
        Value::String(text) => parse_result_payload_from_text(text, expected_kind),
        _ => None,
    }
}

fn parse_decision_payload_from_text(text: &str, phase_id: &str) -> Option<Value> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(result) = parse_decision_payload_from_payload(&payload, phase_id) {
            return Some(result);
        }
    }
    None
}

fn parse_decision_payload_from_payload(payload: &Value, _phase_id: &str) -> Option<Value> {
    match payload {
        Value::Array(items) => items.iter().find_map(|item| parse_decision_payload_from_payload(item, _phase_id)),
        Value::Object(object) => {
            let is_decision = object
                .get("kind")
                .and_then(Value::as_str)
                .map(|v| v.eq_ignore_ascii_case("phase_decision"))
                .unwrap_or(false);
            if is_decision {
                return Some(payload.clone());
            }
            for key in ["proposal", "data", "payload", "result", "output", "item", "phase_decision"] {
                if let Some(value) = object.get(key) {
                    if let Some(result) = parse_decision_payload_from_payload(value, _phase_id) {
                        return Some(result);
                    }
                }
            }
            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    if let Some(result) = parse_decision_payload_from_text(raw, _phase_id) {
                        return Some(result);
                    }
                }
            }
            None
        }
        Value::String(text) => parse_decision_payload_from_text(text, _phase_id),
        _ => None,
    }
}

struct PhaseAgentParams<'a> {
    ctx: &'a RuntimeConfigContext,
    project_root: &'a str,
    execution_cwd: &'a str,
    workflow_id: &'a str,
    subject_id: &'a str,
    subject_title: &'a str,
    subject_description: &'a str,
    task_complexity: Option<orchestrator_core::Complexity>,
    phase_id: &'a str,
    phase_runtime_settings: Option<&'a WorkflowPhaseRuntimeSettings>,
    overrides: Option<&'a PhaseExecuteOverrides>,
    pipeline_vars: Option<&'a std::collections::HashMap<String, String>>,
    dispatch_input: Option<&'a str>,
    schedule_input: Option<&'a str>,
    routing: &'a protocol::PhaseRoutingConfig,
    resolved_phase_skills: &'a skill_dispatch::ResolvedPhaseSkillSet,
    phase_timeout_secs: Option<u64>,
}

struct AgentPhaseRunOutcome {
    outcome: PhaseExecutionOutcome,
    selected_tool: Option<String>,
    selected_model: Option<String>,
    effective_capabilities: protocol::PhaseCapabilities,
    applied_skills: skill_dispatch::AppliedPhaseSkills,
}

fn phase_session_resume_plan(
    workflow_id: &str,
    phase_id: &str,
    session_id: &str,
    continuation: usize,
    attempt: usize,
) -> orchestrator_core::runtime_contract::CliSessionResumePlan {
    let reuses_existing_session = continuation > 0 || attempt > 1;
    orchestrator_core::runtime_contract::CliSessionResumePlan {
        mode: orchestrator_core::runtime_contract::CliSessionResumeMode::NativeId,
        session_key: format!("wf:{workflow_id}:{phase_id}"),
        session_id: Some(session_id.to_string()),
        summary_seed: None,
        reused: reuses_existing_session,
        phase_thread_isolated: true,
    }
}

fn phase_requires_structured_completion(ctx: &RuntimeConfigContext, phase_id: &str) -> bool {
    ctx.phase_decision_contract(phase_id).is_some()
        || ctx.phase_output_contract(phase_id).is_some()
        || ctx.phase_output_json_schema(phase_id).is_some()
        || phase_requires_commit_message_with_ctx(ctx, phase_id)
}

fn phase_outcome_is_complete(outcome: Option<&PhaseExecutionOutcome>, requires_structured_completion: bool) -> bool {
    match outcome {
        Some(PhaseExecutionOutcome::Completed { phase_decision, commit_message, result_payload }) => {
            !requires_structured_completion
                || phase_decision.is_some()
                || commit_message.is_some()
                || result_payload.is_some()
        }
        Some(PhaseExecutionOutcome::ManualPending { .. }) => true,
        None => false,
    }
}

fn resolve_phase_skill_target(
    phase_id: &str,
    target_tool_id: &str,
    target_model_id: &str,
    explicit_tool_override: Option<&str>,
    explicit_model_override: Option<&str>,
    base_caps: &protocol::PhaseCapabilities,
    resolved_phase_skills: &skill_dispatch::ResolvedPhaseSkillSet,
    routing_complexity: Option<protocol::ModelRoutingComplexity>,
    routing: &protocol::PhaseRoutingConfig,
) -> Result<(String, String, protocol::PhaseCapabilities, skill_dispatch::AppliedPhaseSkills)> {
    const MAX_SKILL_TARGET_RESOLUTION_PASSES: usize = 3;

    let initial_tool_id = target_tool_id.to_string();
    let initial_model_id = canonical_model_id(target_model_id);
    let mut tool_id = target_tool_id.to_string();
    let mut model_id = canonical_model_id(target_model_id);
    let mut exhausted_iteration_budget = false;
    let explicit_tool_override =
        explicit_tool_override.map(protocol::normalize_tool_id).filter(|value| !value.trim().is_empty());
    let explicit_model_override =
        explicit_model_override.map(canonical_model_id).filter(|value| !value.trim().is_empty());

    for iteration in 0..MAX_SKILL_TARGET_RESOLUTION_PASSES {
        let applied_skills = skill_dispatch::apply_phase_skills(resolved_phase_skills, &tool_id, &model_id);
        let effective_caps =
            skill_dispatch::apply_skill_capability_overrides(base_caps, &applied_skills.application.capabilities);
        let requested_model = explicit_model_override
            .clone()
            .or_else(|| applied_skills.application.model.as_deref().map(canonical_model_id));
        let mut requested_tool = explicit_tool_override.clone().unwrap_or_else(|| tool_id.clone());
        let requested_model = requested_model.unwrap_or_else(|| model_id.clone());
        let model_tool = PhaseTargetPlanner::tool_for_model_id(&requested_model).to_string();

        if explicit_model_override.is_none() {
            if let Some(pinned_tool) = explicit_tool_override.as_ref() {
                if !model_tool.eq_ignore_ascii_case(pinned_tool) && applied_skills.application.model.is_some() {
                    return Err(anyhow!(
                        "phase '{}' skill selected model '{}' which requires tool '{}' but the run is pinned to tool '{}'",
                        phase_id,
                        requested_model,
                        model_tool,
                        pinned_tool
                    ));
                }
            } else if applied_skills.application.model.is_some() {
                requested_tool = model_tool;
            }
        }

        let (next_tool, next_model) = PhaseTargetPlanner::resolve_phase_execution_target(
            phase_id,
            Some(&requested_model),
            Some(&requested_tool),
            routing_complexity,
            &effective_caps,
            routing,
        );

        if next_tool.eq_ignore_ascii_case(&tool_id) && next_model.eq_ignore_ascii_case(&model_id) {
            return Ok((next_tool, next_model, effective_caps, applied_skills));
        }

        tool_id = next_tool;
        model_id = next_model;
        exhausted_iteration_budget = iteration + 1 == MAX_SKILL_TARGET_RESOLUTION_PASSES;
    }

    let applied_skills = skill_dispatch::apply_phase_skills(resolved_phase_skills, &tool_id, &model_id);
    let effective_caps =
        skill_dispatch::apply_skill_capability_overrides(base_caps, &applied_skills.application.capabilities);
    if exhausted_iteration_budget {
        warn!(
            phase_id,
            initial_tool = %initial_tool_id,
            initial_model = %initial_model_id,
            final_tool = %tool_id,
            final_model = %model_id,
            "phase skill target resolution exhausted iteration budget without convergence"
        );
    }
    Ok((tool_id, model_id, effective_caps, applied_skills))
}

async fn run_workflow_phase_with_agent(params: PhaseAgentParams<'_>) -> Result<AgentPhaseRunOutcome> {
    let ctx = params.ctx;
    let project_root = params.project_root;
    let execution_cwd = params.execution_cwd;
    let workflow_id = params.workflow_id;
    let subject_id = params.subject_id;
    let subject_title = params.subject_title;
    let subject_description = params.subject_description;
    let phase_id = params.phase_id;
    let phase_runtime_settings = params.phase_runtime_settings;
    let overrides = params.overrides;
    let pipeline_vars = params.pipeline_vars;
    let base_caps = ctx.phase_capabilities(phase_id);
    let planning_caps = skill_dispatch::preview_phase_capabilities(&base_caps, params.resolved_phase_skills);
    let routing_complexity = routing_complexity(params.task_complexity);
    let settings_tool = phase_runtime_settings.and_then(|s| s.tool.as_deref());
    let settings_model = phase_runtime_settings.and_then(|s| s.model.as_deref());
    let agent_model_override = ctx.phase_model_override(phase_id);
    let agent_tool_override = ctx.phase_tool_override(phase_id);
    let agent_fallback_models = ctx.phase_fallback_models(phase_id);
    let configured_fallback_models = if agent_fallback_models.is_empty() {
        phase_runtime_settings.map(|settings| settings.fallback_models.clone()).unwrap_or_default()
    } else {
        agent_fallback_models
    };
    let execution_targets = PhaseTargetPlanner::build_phase_execution_targets(
        phase_id,
        settings_model.or(agent_model_override.as_deref()),
        settings_tool.or(agent_tool_override.as_deref()),
        configured_fallback_models.as_slice(),
        routing_complexity,
        Some(project_root),
        &planning_caps,
        params.routing,
    );
    let prompt_inputs = PhasePromptInputs {
        rework_context: overrides.and_then(|o| o.rework_context.as_deref()).map(ToOwned::to_owned),
        pipeline_vars: pipeline_vars.cloned().unwrap_or_default(),
        dispatch_input: params.dispatch_input.map(ToOwned::to_owned),
        schedule_input: params.schedule_input.map(ToOwned::to_owned),
    };
    let phase_render_params = PhaseRenderParams {
        project_root,
        execution_cwd,
        workflow_id,
        subject_id,
        subject_title,
        subject_description,
        phase_id,
    };
    let max_attempts =
        phase_runtime_settings.and_then(|settings| settings.max_attempts).unwrap_or_else(phase_runner_attempts);
    let max_continuations =
        phase_runtime_settings.and_then(|settings| settings.max_continuations).unwrap_or_else(phase_max_continuations);
    let session_id = Uuid::new_v4().to_string();
    let mut fallover_errors: Vec<String> = Vec::new();
    let requires_structured_completion = phase_requires_structured_completion(ctx, phase_id);

    for (target_index, (target_tool_id, target_model_id)) in execution_targets.iter().enumerate() {
        let (effective_tool_id, effective_model_id, effective_caps, applied_skills) = resolve_phase_skill_target(
            phase_id,
            target_tool_id,
            target_model_id,
            overrides.and_then(|value| value.tool.as_deref()),
            overrides.and_then(|value| value.model.as_deref()),
            &planning_caps,
            params.resolved_phase_skills,
            routing_complexity,
            params.routing,
        )?;
        let prompt = render_phase_prompt_with_ctx_overrides(
            ctx,
            &phase_render_params,
            prompt_inputs.clone(),
            Some(effective_caps.clone()),
            (!applied_skills.application.is_empty()).then_some(&applied_skills.application),
        )
        .final_prompt;
        let request_timeout_secs = params
            .phase_timeout_secs
            .or(applied_skills.application.timeout_secs)
            .or(phase_runtime_settings.and_then(|settings| settings.timeout_secs));
        let mut last_outcome: Option<PhaseExecutionOutcome> = None;
        let base_context = serde_json::json!({
            "tool": effective_tool_id,
            "prompt": prompt,
            "cwd": execution_cwd,
            "project_root": project_root,
            "workflow_id": workflow_id,
            "subject_id": subject_id,
            "phase_id": phase_id,
            "phase_capabilities": serde_json::to_value(&effective_caps)?,
        });
        let phase_contract = ctx.phase_output_contract(phase_id).cloned();
        let phase_output_schema = phase_output_json_schema_for(ctx, phase_id)?;
        let phase_response_schema = phase_response_json_schema_for(ctx, phase_id)?;

        for continuation in 0..=max_continuations {
            let is_continuation = continuation > 0;
            let effective_prompt = if is_continuation {
                format!(
                    "Continue your work on the current task. Your previous session was interrupted \
                     before completion. Pick up where you left off and complete the remaining work. \
                     The original task: {}",
                    prompt
                )
            } else {
                prompt.clone()
            };

            let mut attempt_succeeded = false;
            let mut backoff = Duration::from_millis(200);
            for attempt in 1..=max_attempts {
                let resume_plan = phase_session_resume_plan(workflow_id, phase_id, &session_id, continuation, attempt);
                let mut context = base_context.clone();
                context
                    .as_object_mut()
                    .expect("json object")
                    .insert("prompt".to_string(), serde_json::json!(effective_prompt));
                if let Some(agent_id) = ctx.phase_agent_id(phase_id) {
                    context
                        .as_object_mut()
                        .expect("json object")
                        .insert("agent_id".to_string(), serde_json::json!(agent_id));
                }
                if let Some(mut runtime_contract) = build_runtime_contract_with_resume(
                    context.get("tool").and_then(Value::as_str).unwrap_or("codex"),
                    &effective_model_id,
                    &effective_prompt,
                    Some(&resume_plan),
                ) {
                    if let Some(contract) = phase_contract.as_ref() {
                        let mut policy = serde_json::json!({
                            "require_commit_message": contract.requires_field("commit_message"),
                            "required_result_kind": contract.kind.as_str(),
                            "required_result_fields": contract.required_fields.clone(),
                        });
                        if let Some(schema) = phase_response_schema.clone().or(phase_output_schema.clone()) {
                            policy
                                .as_object_mut()
                                .expect("json object")
                                .insert("output_json_schema".to_string(), schema);
                        }
                        runtime_contract.as_object_mut().expect("json object").insert("policy".to_string(), policy);
                    }
                    if let Some(schema) = phase_response_schema.as_ref() {
                        inject_response_schema_into_launch_args(
                            &mut runtime_contract,
                            schema,
                            &ctx.agent_runtime_config,
                        );
                    }
                    apply_phase_capability_launch_flags(
                        &mut runtime_contract,
                        &effective_caps,
                        &ctx.agent_runtime_config,
                    );
                    inject_cli_launch_overrides(&mut runtime_contract, &effective_tool_id, phase_runtime_settings);
                    inject_default_stdio_mcp(&mut runtime_contract, project_root);
                    inject_agent_tool_policy(&mut runtime_contract, ctx, phase_id);
                    inject_project_mcp_servers(&mut runtime_contract, project_root, ctx, phase_id);
                    inject_workflow_mcp_servers(&mut runtime_contract, ctx, phase_id);
                    if let Some(policy) = applied_skills.application.tool_policy.as_ref() {
                        set_mcp_tool_policy(&mut runtime_contract, policy);
                    }
                    inject_named_mcp_servers(
                        &mut runtime_contract,
                        project_root,
                        ctx,
                        phase_id,
                        &applied_skills.application.mcp_servers,
                    )?;
                    check_required_env_for_phase_mcp_servers(
                        project_root,
                        ctx,
                        phase_id,
                        &applied_skills.application.mcp_servers,
                    )?;
                    skill_dispatch::inject_skill_overrides(
                        &mut runtime_contract,
                        &effective_tool_id,
                        &applied_skills.application,
                    );
                    let cli_supports_mcp = runtime_contract
                        .pointer("/cli/capabilities/supports_mcp")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let mcp_enforce_only = runtime_contract.pointer("/mcp/enforce_only").and_then(Value::as_bool);
                    let mcp_endpoint = runtime_contract
                        .pointer("/mcp/endpoint")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToString::to_string);
                    let mcp_stdio_command = runtime_contract
                        .pointer("/mcp/stdio/command")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToString::to_string);
                    let mcp_stdio_args = runtime_contract_string_array(&runtime_contract, "/mcp/stdio/args");
                    let mcp_additional_servers = runtime_contract_additional_server_names(&runtime_contract);
                    let mcp_tool_policy_allow =
                        runtime_contract_string_array(&runtime_contract, "/mcp/tool_policy/allow");
                    let mcp_tool_policy_deny =
                        runtime_contract_string_array(&runtime_contract, "/mcp/tool_policy/deny");
                    info!(
                        workflow_id = %workflow_id,
                        phase_id = %phase_id,
                        subject_id = %subject_id,
                        tool = %effective_tool_id,
                        model = %effective_model_id,
                        continuation,
                        attempt,
                        execution_cwd = %execution_cwd,
                        project_root = %project_root,
                        cli_supports_mcp,
                        mcp_enforce_only = ?mcp_enforce_only,
                        mcp_endpoint = ?mcp_endpoint,
                        mcp_stdio_command = ?mcp_stdio_command,
                        mcp_stdio_args = ?mcp_stdio_args,
                        mcp_additional_servers = ?mcp_additional_servers,
                        mcp_tool_policy_allow = ?mcp_tool_policy_allow,
                        mcp_tool_policy_deny = ?mcp_tool_policy_deny,
                        "Prepared workflow phase runtime contract"
                    );
                    if debug_mcp_stdio_enabled() {
                        eprintln!(
                            "[ao][debug][mcp-stdio] prepared workflow={} phase={} command={:?} args={:?} additional={:?}",
                            workflow_id, phase_id, mcp_stdio_command, mcp_stdio_args, mcp_additional_servers
                        );
                    }
                    context
                        .as_object_mut()
                        .expect("json object")
                        .insert("runtime_contract".to_string(), runtime_contract);
                } else {
                    info!(
                        workflow_id = %workflow_id,
                        phase_id = %phase_id,
                        subject_id = %subject_id,
                        tool = %effective_tool_id,
                        model = %effective_model_id,
                        continuation,
                        attempt,
                        "Skipping runtime contract injection for workflow phase because no launch contract was built"
                    );
                }

                let run_id = RunId(format!(
                    "wf-{workflow_id}-{}-{target_index}-c{continuation}-a{attempt}-{}",
                    phase_id,
                    Uuid::new_v4().simple()
                ));
                let request = AgentRunRequest {
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    run_id,
                    model: ModelId(effective_model_id.clone()),
                    context,
                    timeout_secs: request_timeout_secs,
                };
                debug!(
                    workflow_id = %workflow_id,
                    phase_id = %phase_id,
                    subject_id = %subject_id,
                    run_id = %request.run_id.0,
                    tool = %effective_tool_id,
                    model = %effective_model_id,
                    timeout_secs = ?request.timeout_secs,
                    is_continuation,
                    continuation,
                    attempt,
                    "Dispatching workflow phase attempt to agent runner"
                );

                match run_workflow_phase_attempt(project_root, workflow_id, phase_id, &request).await {
                    Ok(mut outcome) => {
                        if phase_requires_commit_message_with_ctx(ctx, phase_id) {
                            if let PhaseExecutionOutcome::Completed { commit_message, .. } = &mut outcome {
                                let resolved_commit_message = commit_message.clone().unwrap_or_else(|| {
                                    crate::payload_traversal::fallback_implementation_commit_message(
                                        subject_id,
                                        subject_title,
                                    )
                                });
                                commit_implementation_changes(execution_cwd, &resolved_commit_message)?;
                                *commit_message = Some(resolved_commit_message);
                            }
                        }
                        last_outcome = Some(outcome);
                        attempt_succeeded = true;
                        break;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let should_retry = attempt < max_attempts
                            && PhaseFailureClassifier::is_transient_runner_error_message(&message);
                        if should_retry {
                            warn!(
                                workflow_id = %workflow_id,
                                phase_id = %phase_id,
                                attempt,
                                max_attempts,
                                error = %message,
                                "Transient runner error on phase attempt; retrying"
                            );
                            sleep(backoff).await;
                            backoff = std::cmp::min(backoff.saturating_mul(2), Duration::from_secs(3));
                            continue;
                        }

                        let has_fallback_target = target_index + 1 < execution_targets.len();
                        if has_fallback_target && PhaseFailureClassifier::should_failover_target(&message) {
                            fallover_errors.push(format!(
                                "target {}:{} failed: {}",
                                effective_tool_id, effective_model_id, message
                            ));
                            orchestrator_core::record_model_phase_outcome(
                                std::path::Path::new(project_root),
                                &effective_model_id,
                                phase_id,
                                orchestrator_core::PhaseDecisionVerdict::Fail,
                            );
                            break;
                        }
                        orchestrator_core::record_model_phase_outcome(
                            std::path::Path::new(project_root),
                            &effective_model_id,
                            phase_id,
                            orchestrator_core::PhaseDecisionVerdict::Fail,
                        );
                        return Err(error);
                    }
                }
            }

            if !attempt_succeeded {
                break;
            }

            let outcome_is_complete = phase_outcome_is_complete(last_outcome.as_ref(), requires_structured_completion);

            if outcome_is_complete {
                let outcome = last_outcome.take().expect("outcome verified above");
                orchestrator_core::record_model_phase_outcome(
                    std::path::Path::new(project_root),
                    &effective_model_id,
                    phase_id,
                    outcome_verdict(&outcome),
                );
                return Ok(AgentPhaseRunOutcome {
                    outcome,
                    selected_tool: Some(effective_tool_id.clone()),
                    selected_model: Some(effective_model_id.clone()),
                    effective_capabilities: effective_caps.clone(),
                    applied_skills: applied_skills.clone(),
                });
            }

            if continuation < max_continuations {
                eprintln!(
                    "[ao] workflow {} phase {}: agent produced no result, \
                     attempting continuation {}/{}",
                    workflow_id,
                    phase_id,
                    continuation + 1,
                    max_continuations
                );
            }
        }

        if let Some(outcome) = last_outcome {
            orchestrator_core::record_model_phase_outcome(
                std::path::Path::new(project_root),
                &effective_model_id,
                phase_id,
                outcome_verdict(&outcome),
            );
            return Ok(AgentPhaseRunOutcome {
                outcome,
                selected_tool: Some(effective_tool_id),
                selected_model: Some(effective_model_id),
                effective_capabilities: effective_caps,
                applied_skills,
            });
        }
    }

    Err(anyhow!(
        "workflow {} phase {} exhausted fallback targets: {}",
        workflow_id,
        phase_id,
        if fallover_errors.is_empty() {
            "no available execution targets".to_string()
        } else {
            fallover_errors.join(" || ")
        }
    ))
}

fn manual_phase_marker_path(project_root: &str) -> std::path::PathBuf {
    Path::new(project_root).join(".ao").join("state").join("manual-phase-markers.v1.json")
}

fn load_manual_phase_markers(path: &Path) -> BTreeMap<String, bool> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<BTreeMap<String, bool>>(&content).unwrap_or_default()
}

fn write_manual_phase_markers(path: &Path, markers: &BTreeMap<String, bool>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(markers)?;
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().and_then(|value| value.to_str()).unwrap_or("manual-phase-markers"),
        Uuid::new_v4()
    ));
    std::fs::write(&tmp_path, payload)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

fn should_emit_manual_required(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    phase_attempt: u32,
) -> Result<bool> {
    let path = manual_phase_marker_path(project_root);
    let key = format!("{workflow_id}::{phase_id}::{phase_attempt}");
    let mut markers = load_manual_phase_markers(&path);
    if markers.get(&key).copied().unwrap_or(false) {
        return Ok(false);
    }
    markers.insert(key, true);
    write_manual_phase_markers(&path, &markers)?;
    Ok(true)
}

pub struct PhaseRunParams<'a> {
    pub project_root: &'a str,
    pub execution_cwd: &'a str,
    pub workflow_id: &'a str,
    pub workflow_ref: &'a str,
    pub subject_id: &'a str,
    pub subject_title: &'a str,
    pub subject_description: &'a str,
    pub task_complexity: Option<orchestrator_core::Complexity>,
    pub phase_id: &'a str,
    pub phase_attempt: u32,
    pub overrides: Option<&'a PhaseExecuteOverrides>,
    pub pipeline_vars: Option<&'a std::collections::HashMap<String, String>>,
    pub dispatch_input: Option<&'a str>,
    pub schedule_input: Option<&'a str>,
    pub routing: &'a protocol::PhaseRoutingConfig,
    pub phase_timeout_secs: Option<u64>,
}

pub async fn run_workflow_phase(params: &PhaseRunParams<'_>) -> Result<PhaseRunResult> {
    let project_root = params.project_root;
    let execution_cwd = params.execution_cwd;
    let workflow_id = params.workflow_id;
    let workflow_ref = params.workflow_ref;
    let subject_id = params.subject_id;
    let subject_title = params.subject_title;
    let subject_description = params.subject_description;
    let task_complexity = params.task_complexity;
    let phase_id = params.phase_id;
    let phase_attempt = params.phase_attempt;
    let overrides = params.overrides;
    let pipeline_vars = params.pipeline_vars;
    let dispatch_input = params.dispatch_input;
    let schedule_input = params.schedule_input;
    let workflow_config = load_workflow_config_strict(project_root)?;
    let runtime_loaded = load_agent_runtime_config_strict(project_root)?;
    orchestrator_core::validate_workflow_and_runtime_configs(&workflow_config.config, &runtime_loaded.config)?;

    let mut merged_runtime = runtime_loaded.config.clone();
    for (id, profile) in &workflow_config.config.agent_profiles {
        merged_runtime.agents.insert(id.clone(), profile.clone());
    }
    if !workflow_config.config.tools_allowlist.is_empty() {
        let mut combined: std::collections::HashSet<String> = merged_runtime.tools_allowlist.iter().cloned().collect();
        combined.extend(workflow_config.config.tools_allowlist.iter().cloned());
        merged_runtime.tools_allowlist = combined.into_iter().collect();
        merged_runtime.tools_allowlist.sort();
    }

    let ctx =
        RuntimeConfigContext { agent_runtime_config: merged_runtime.clone(), workflow_config: workflow_config.clone() };

    let definition = ctx
        .phase_execution(phase_id)
        .ok_or_else(|| anyhow!("phase '{}' is missing from both workflow config and agent runtime config", phase_id))?;
    let agent_id = ctx.phase_agent_id(phase_id);
    let agent_profile_hash = agent_id.as_deref().and_then(|id| merged_runtime.agent_profile(id)).map(hash_serializable);

    let mut metadata = PhaseExecutionMetadata {
        phase_id: phase_id.to_string(),
        phase_mode: definition.mode.to_string(),
        phase_definition_hash: hash_serializable(definition),
        agent_runtime_config_hash: runtime_loaded.metadata.hash.clone(),
        agent_runtime_schema: runtime_loaded.metadata.schema.clone(),
        agent_runtime_version: runtime_loaded.metadata.version,
        agent_runtime_source: runtime_loaded.metadata.source.as_str().to_string(),
        agent_id: agent_id.clone(),
        agent_profile_hash,
        selected_tool: None,
        selected_model: None,
        effective_capabilities: ctx.phase_capabilities(phase_id),
        requested_skills: Vec::new(),
        resolved_skills: Vec::new(),
        applied_skills: Vec::new(),
        skill_application: None,
    };

    let mut signals = vec![PhaseExecutionSignal {
        event_type: "workflow-phase-execution-selected".to_string(),
        payload: serde_json::json!({
            "workflow_id": workflow_id,
            "subject_id": subject_id,
            "phase_id": phase_id,
            "phase_mode": metadata.phase_mode,
            "phase_definition_hash": metadata.phase_definition_hash,
            "agent_runtime_config_hash": metadata.agent_runtime_config_hash,
            "agent_runtime_schema": metadata.agent_runtime_schema,
            "agent_runtime_version": metadata.agent_runtime_version,
            "agent_runtime_source": metadata.agent_runtime_source,
            "agent_id": metadata.agent_id,
            "agent_profile_hash": metadata.agent_profile_hash,
        }),
    }];

    match definition.mode {
        orchestrator_core::PhaseExecutionMode::Agent => {
            let resolved_phase_skills = skill_dispatch::resolve_phase_skills(&ctx, Path::new(project_root), phase_id)?;
            metadata.requested_skills = resolved_phase_skills.requested_skills.clone();
            metadata.resolved_skills = resolved_phase_skills.resolved_skills.clone();
            let cli_tool_override = overrides.and_then(|o| o.tool.as_deref());
            let cli_model_override = overrides.and_then(|o| o.model.as_deref());

            let runtime_settings = Some(WorkflowPhaseRuntimeSettings {
                tool: cli_tool_override.or_else(|| merged_runtime.phase_tool_override(phase_id)).map(ToOwned::to_owned),
                model: cli_model_override
                    .or_else(|| merged_runtime.phase_model_override(phase_id))
                    .map(ToOwned::to_owned),
                fallback_models: merged_runtime.phase_fallback_models(phase_id),
                reasoning_effort: merged_runtime.phase_reasoning_effort(phase_id).map(ToOwned::to_owned),
                web_search: merged_runtime.phase_web_search(phase_id),
                network_access: merged_runtime.phase_network_access(phase_id),
                timeout_secs: merged_runtime.phase_timeout_secs(phase_id),
                max_attempts: merged_runtime.phase_max_attempts(phase_id),
                extra_args: merged_runtime.phase_extra_args(phase_id),
                codex_config_overrides: merged_runtime.phase_codex_config_overrides(phase_id),
                max_continuations: merged_runtime.phase_max_continuations(phase_id),
            });
            let agent_result = run_workflow_phase_with_agent(PhaseAgentParams {
                ctx: &ctx,
                project_root,
                execution_cwd,
                workflow_id,
                subject_id,
                subject_title,
                subject_description,
                task_complexity,
                phase_id,
                phase_runtime_settings: runtime_settings.as_ref(),
                overrides,
                pipeline_vars,
                dispatch_input,
                schedule_input,
                routing: params.routing,
                resolved_phase_skills: &resolved_phase_skills,
                phase_timeout_secs: params.phase_timeout_secs,
            })
            .await?;
            metadata.selected_tool = agent_result.selected_tool.clone();
            metadata.selected_model = agent_result.selected_model.clone();
            metadata.effective_capabilities = agent_result.effective_capabilities.clone();
            metadata.applied_skills = agent_result.applied_skills.applied_skills.clone();
            metadata.skill_application = (!agent_result.applied_skills.application.is_empty())
                .then_some(agent_result.applied_skills.application.clone());
            if !metadata.requested_skills.is_empty() {
                signals.push(PhaseExecutionSignal {
                    event_type: "workflow-phase-skills-resolved".to_string(),
                    payload: serde_json::json!({
                        "workflow_id": workflow_id,
                        "phase_id": phase_id,
                        "requested_skills": metadata.requested_skills,
                        "resolved_skills": metadata.resolved_skills,
                        "applied_skills": metadata.applied_skills,
                        "skill_application": metadata.skill_application,
                        "effective_capabilities": metadata.effective_capabilities,
                    }),
                });
            }
            let outcome = agent_result.outcome;

            if definition.output_contract.is_some() || definition.output_json_schema.is_some() {
                if let PhaseExecutionOutcome::Completed { commit_message, result_payload, .. } = &outcome {
                    if definition
                        .output_contract
                        .as_ref()
                        .is_some_and(|contract| contract.requires_field("commit_message"))
                        && commit_message.as_deref().map(str::trim).filter(|value| !value.is_empty()).is_none()
                    {
                        signals.push(PhaseExecutionSignal {
                            event_type: "workflow-phase-contract-violation".to_string(),
                            payload: serde_json::json!({
                                "workflow_id": workflow_id,
                                "phase_id": phase_id,
                                "reason": "commit_message required but missing",
                            }),
                        });
                        return Err(anyhow!("phase '{}' contract violation: commit_message is required", phase_id));
                    }

                    let phase_schema = match phase_response_json_schema_for(&ctx, phase_id)? {
                        Some(s) => Some(s),
                        None => phase_output_json_schema_for(&ctx, phase_id)?,
                    };
                    if let Some(schema) = phase_schema.as_ref() {
                        let mut payload = result_payload.clone().unwrap_or_else(|| {
                            serde_json::json!({
                                "kind": definition
                                    .output_contract
                                    .as_ref()
                                    .map(|contract| contract.kind.as_str())
                                    .unwrap_or("phase_result"),
                                "commit_message": commit_message,
                            })
                        });
                        if let (Some(obj), Some(msg)) = (payload.as_object_mut(), commit_message.as_deref()) {
                            if !obj.contains_key("commit_message") && !msg.trim().is_empty() {
                                obj.insert("commit_message".to_string(), serde_json::Value::String(msg.to_string()));
                            }
                        }
                        if let Err(error) = validate_basic_json_schema(&payload, schema) {
                            signals.push(PhaseExecutionSignal {
                                event_type: "workflow-phase-contract-violation".to_string(),
                                payload: serde_json::json!({
                                    "workflow_id": workflow_id,
                                    "phase_id": phase_id,
                                    "reason": error.to_string(),
                                }),
                            });
                            return Err(anyhow!("phase '{}' contract violation: {}", phase_id, error));
                        }
                    }

                    signals.push(PhaseExecutionSignal {
                        event_type: "workflow-phase-contract-validated".to_string(),
                        payload: serde_json::json!({
                            "workflow_id": workflow_id,
                            "phase_id": phase_id,
                            "phase_mode": "agent",
                        }),
                    });
                }
            }

            Ok(PhaseRunResult { outcome, metadata, signals })
        }
        orchestrator_core::PhaseExecutionMode::Command => {
            let command = definition
                .command
                .as_ref()
                .ok_or_else(|| anyhow!("phase '{}' is missing command definition", phase_id))?;
            let command_context = CommandExecutionContext {
                project_root,
                execution_cwd,
                workflow_id,
                phase_id,
                workflow_ref,
                subject_id,
                subject_title,
                subject_description,
                pipeline_vars,
                dispatch_input,
                schedule_input,
            };

            let command_result = run_workflow_phase_with_command(&command_context, &merged_runtime, command).await?;
            signals.push(PhaseExecutionSignal {
                event_type: "workflow-phase-command-executed".to_string(),
                payload: serde_json::json!({
                    "workflow_id": workflow_id,
                    "subject_id": subject_id,
                    "phase_id": phase_id,
                    "program": command_result.program,
                    "args": command_result.args.clone(),
                    "cwd": command_result.cwd.clone(),
                    "exit_code": command_result.exit_code,
                    "duration_ms": command_result.duration_ms,
                    "stdout": command_result.stdout.clone(),
                    "stderr": command_result.stderr.clone(),
                    "parsed_payload": command_result.parsed_payload.clone(),
                    "phase_decision": command_result.phase_decision.clone(),
                }),
            });

            if let Some(ref failure_summary) = command_result.failure_summary {
                let decision = command_result.phase_decision.clone().unwrap_or_else(|| {
                    build_command_phase_decision(
                        command,
                        phase_id,
                        command_result.exit_code,
                        Some(failure_summary.as_str()),
                    )
                });
                let result_payload = build_command_result_payload(
                    command,
                    phase_id,
                    definition.output_contract.as_ref().map(|contract| contract.kind.as_str()),
                    &command_result,
                    &decision,
                );

                let outcome = PhaseExecutionOutcome::Completed {
                    commit_message: None,
                    phase_decision: Some(decision),
                    result_payload: Some(result_payload),
                };

                persist_phase_output(project_root, workflow_id, phase_id, &outcome)?;

                return Ok(PhaseRunResult { outcome, metadata, signals });
            }

            if command.parse_json_output {
                signals.push(PhaseExecutionSignal {
                    event_type: "workflow-phase-contract-validated".to_string(),
                    payload: serde_json::json!({
                        "workflow_id": workflow_id,
                        "phase_id": phase_id,
                        "phase_mode": "command",
                    }),
                });
            }

            let decision = command_result
                .phase_decision
                .clone()
                .unwrap_or_else(|| build_command_phase_decision(command, phase_id, command_result.exit_code, None));
            let result_payload = build_command_result_payload(
                command,
                phase_id,
                definition.output_contract.as_ref().map(|contract| contract.kind.as_str()),
                &command_result,
                &decision,
            );

            Ok(PhaseRunResult {
                outcome: PhaseExecutionOutcome::Completed {
                    commit_message: None,
                    phase_decision: Some(decision),
                    result_payload: Some(result_payload),
                },
                metadata,
                signals,
            })
        }
        orchestrator_core::PhaseExecutionMode::Manual => {
            let manual = definition
                .manual
                .as_ref()
                .ok_or_else(|| anyhow!("phase '{}' is missing manual definition", phase_id))?;
            if should_emit_manual_required(project_root, workflow_id, phase_id, phase_attempt)? {
                signals.push(PhaseExecutionSignal {
                    event_type: "workflow-phase-manual-required".to_string(),
                    payload: serde_json::json!({
                        "workflow_id": workflow_id,
                        "subject_id": subject_id,
                        "phase_id": phase_id,
                        "instructions": manual.instructions,
                        "approval_note_required": manual.approval_note_required,
                    }),
                });
            }

            Ok(PhaseRunResult {
                outcome: PhaseExecutionOutcome::ManualPending {
                    instructions: manual.instructions.clone(),
                    approval_note_required: manual.approval_note_required,
                },
                metadata,
                signals,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        phase_outcome_is_complete, phase_session_resume_plan, process_phase_event_stream, PhaseExecutionOutcome,
    };

    #[test]
    fn initial_attempt_starts_a_fresh_native_session() {
        let plan = phase_session_resume_plan("wf-1", "requirements", "session-123", 0, 1);

        assert_eq!(plan.session_key, "wf:wf-1:requirements");
        assert_eq!(plan.session_id.as_deref(), Some("session-123"));
        assert!(!plan.reused);
    }

    #[test]
    fn retry_attempt_reuses_the_existing_native_session() {
        let plan = phase_session_resume_plan("wf-1", "requirements", "session-123", 0, 2);

        assert!(plan.reused);
    }

    #[test]
    fn continuation_reuses_the_existing_native_session() {
        let plan = phase_session_resume_plan("wf-1", "requirements", "session-123", 1, 1);

        assert!(plan.reused);
    }

    #[test]
    fn contractless_phases_complete_after_any_clean_exit() {
        let outcome =
            PhaseExecutionOutcome::Completed { commit_message: None, phase_decision: None, result_payload: None };

        assert!(phase_outcome_is_complete(Some(&outcome), false));
    }

    #[test]
    fn structured_phases_require_a_result_signal() {
        let empty_outcome =
            PhaseExecutionOutcome::Completed { commit_message: None, phase_decision: None, result_payload: None };
        let decided_outcome = PhaseExecutionOutcome::Completed {
            commit_message: None,
            phase_decision: Some(orchestrator_core::PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "requirements".to_string(),
                verdict: orchestrator_core::PhaseDecisionVerdict::Advance,
                confidence: 0.9,
                risk: orchestrator_core::WorkflowDecisionRisk::Low,
                reason: "Complete".to_string(),
                evidence: Vec::new(),
                guardrail_violations: Vec::new(),
                commit_message: None,
                target_phase: None,
            }),
            result_payload: None,
        };

        assert!(!phase_outcome_is_complete(Some(&empty_outcome), true));
        assert!(phase_outcome_is_complete(Some(&decided_outcome), true));
    }

    use protocol::{OutputStreamType, RunId, Timestamp};
    use tokio::io::AsyncBufReadExt;
    use uuid::Uuid;

    fn make_event_stream(events: &[protocol::AgentRunEvent]) -> Vec<u8> {
        events.iter().map(|e| serde_json::to_string(e).unwrap() + "\n").collect::<String>().into_bytes()
    }

    fn temp_run_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ao-phase-exec-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    async fn run_stream(
        events: &[protocol::AgentRunEvent],
        run_id: &RunId,
        run_dir: Option<&std::path::Path>,
    ) -> anyhow::Result<PhaseExecutionOutcome> {
        let bytes = make_event_stream(events);
        let reader = tokio::io::BufReader::new(bytes.as_slice());
        process_phase_event_stream(reader.lines(), run_id, "wf-test", "impl", false, false, "", run_dir).await
    }

    #[tokio::test]
    async fn event_stream_success_returns_completed() {
        let run_id = RunId("run-success-001".to_string());
        let events = vec![
            protocol::AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() },
            protocol::AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "work done\n".to_string(),
            },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 200 },
        ];
        let outcome = run_stream(&events, &run_id, None).await.expect("should succeed");
        assert!(matches!(outcome, PhaseExecutionOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn event_stream_nonzero_exit_returns_error() {
        let run_id = RunId("run-nonzero-001".to_string());
        let events = vec![
            protocol::AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stderr,
                text: "fatal error\n".to_string(),
            },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(1), duration_ms: 50 },
        ];
        let err = run_stream(&events, &run_id, None).await.expect_err("should be an error");
        assert!(err.to_string().contains("exited with code"), "got: {}", err);
    }

    #[tokio::test]
    async fn event_stream_error_event_returns_error() {
        let run_id = RunId("run-error-001".to_string());
        let events =
            vec![protocol::AgentRunEvent::Error { run_id: run_id.clone(), error: "unexpected failure".to_string() }];
        let err = run_stream(&events, &run_id, None).await.expect_err("should be an error");
        assert!(err.to_string().contains("unexpected failure"), "got: {}", err);
    }

    #[tokio::test]
    async fn event_stream_runner_disconnect_returns_error() {
        let run_id = RunId("run-disconnect-001".to_string());
        let events = vec![protocol::AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() }];
        let err = run_stream(&events, &run_id, None).await.expect_err("should be an error");
        assert!(err.to_string().contains("runner disconnected"), "got: {}", err);
    }

    #[tokio::test]
    async fn event_stream_provider_exhaustion_is_annotated_in_error() {
        let run_id = RunId("run-exhaust-001".to_string());
        let exhaustion_text = "Error: 429 Too Many Requests\nyour account has exceeded its usage limit\nplease upgrade";
        let events = vec![
            protocol::AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stderr,
                text: exhaustion_text.to_string(),
            },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(1), duration_ms: 10 },
        ];
        let err = run_stream(&events, &run_id, None).await.expect_err("should fail");
        assert!(err.to_string().contains("provider_exhausted"), "expected exhaustion annotation, got: {}", err);
    }

    #[tokio::test]
    async fn event_stream_tool_thinking_artifact_events_are_tolerated() {
        let run_id = RunId("run-multi-events-001".to_string());
        let events = vec![
            protocol::AgentRunEvent::Thinking { run_id: run_id.clone(), content: "reasoning...".to_string() },
            protocol::AgentRunEvent::ToolCall {
                run_id: run_id.clone(),
                tool_info: protocol::ToolCallInfo {
                    tool_name: "bash".to_string(),
                    parameters: serde_json::json!({"command": "ls"}),
                    timestamp: Timestamp::now(),
                },
            },
            protocol::AgentRunEvent::ToolResult {
                run_id: run_id.clone(),
                result_info: protocol::ToolResultInfo {
                    tool_name: "bash".to_string(),
                    result: serde_json::json!("file.txt"),
                    duration_ms: 10,
                    success: true,
                },
            },
            protocol::AgentRunEvent::Artifact {
                run_id: run_id.clone(),
                artifact_info: protocol::ArtifactInfo {
                    artifact_id: "art-001".to_string(),
                    artifact_type: protocol::ArtifactType::Other,
                    file_path: Some("out.txt".to_string()),
                    size_bytes: None,
                    mime_type: None,
                },
            },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 300 },
        ];
        let outcome = run_stream(&events, &run_id, None).await.expect("should succeed despite mixed events");
        assert!(matches!(outcome, PhaseExecutionOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn event_stream_events_for_other_run_id_are_ignored() {
        let run_id = RunId("run-filter-001".to_string());
        let other_run_id = RunId("run-other-999".to_string());
        let events = vec![
            protocol::AgentRunEvent::Finished { run_id: other_run_id.clone(), exit_code: Some(0), duration_ms: 10 },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 20 },
        ];
        let outcome = run_stream(&events, &run_id, None).await.expect("should succeed on matching run");
        assert!(matches!(outcome, PhaseExecutionOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn event_stream_persists_events_to_run_dir() {
        let run_dir = temp_run_dir();
        let run_id = RunId("run-persist-stream-001".to_string());
        let events = vec![
            protocol::AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() },
            protocol::AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "output text\n{\"kind\":\"result\"}\n".to_string(),
            },
            protocol::AgentRunEvent::Thinking { run_id: run_id.clone(), content: "thinking...".to_string() },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 100 },
        ];
        run_stream(&events, &run_id, Some(&run_dir)).await.expect("should succeed");

        let events_path = run_dir.join("events.jsonl");
        assert!(events_path.exists(), "events.jsonl should be written");
        let contents = std::fs::read_to_string(&events_path).expect("read events.jsonl");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 4, "all 4 events should be persisted");

        let json_output_path = run_dir.join("json-output.jsonl");
        assert!(json_output_path.exists(), "json-output.jsonl should be written");
        let json_contents = std::fs::read_to_string(&json_output_path).expect("read json-output.jsonl");
        assert!(json_contents.contains("\"result\""), "JSON payload from OutputChunk should be extracted");

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[tokio::test]
    async fn event_stream_persists_events_on_error_exit() {
        let run_dir = temp_run_dir();
        let run_id = RunId("run-persist-err-001".to_string());
        let events = vec![
            protocol::AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stderr,
                text: "crash\n".to_string(),
            },
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(2), duration_ms: 30 },
        ];
        let _ = run_stream(&events, &run_id, Some(&run_dir)).await;

        let events_path = run_dir.join("events.jsonl");
        assert!(events_path.exists(), "events.jsonl should be written even on failure");
        let lines: Vec<String> =
            std::fs::read_to_string(&events_path).expect("read").lines().map(str::to_string).collect();
        assert_eq!(lines.len(), 2, "both events should be persisted before the error is returned");

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[tokio::test]
    async fn event_stream_malformed_lines_are_skipped() {
        let run_id = RunId("run-malformed-001".to_string());
        let mut bytes = b"not json at all\n{\"garbage\": true}\n".to_vec();
        let finished_event =
            protocol::AgentRunEvent::Finished { run_id: run_id.clone(), exit_code: Some(0), duration_ms: 10 };
        bytes.extend(serde_json::to_string(&finished_event).unwrap().as_bytes());
        bytes.push(b'\n');

        let reader = tokio::io::BufReader::new(bytes.as_slice());
        let outcome = process_phase_event_stream(
            reader.lines(),
            &run_id,
            "wf-test",
            "impl",
            false,
            false,
            "",
            None::<&std::path::Path>,
        )
        .await
        .expect("malformed lines should be skipped, not cause failure");
        assert!(matches!(outcome, PhaseExecutionOutcome::Completed { .. }));
    }
}
