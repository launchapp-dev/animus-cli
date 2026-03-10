use crate::ipc::{
    build_runtime_contract_with_resume, collect_json_payload_lines, connect_runner,
    event_matches_run, runner_config_dir, write_json_line,
};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use orchestrator_core::ServiceHub;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use protocol::{AgentRunEvent, AgentRunRequest, ModelId, RunId, PROTOCOL_VERSION};

use super::phase_git::commit_implementation_changes;
use super::phase_output::{format_output_chunk_for_display, format_tool_call_for_display};
use super::phase_prompt::{phase_requires_commit_message_with_config, phase_result_kind_for};

pub use super::phase_output::persist_phase_output;
pub use super::phase_prompt::build_phase_prompt;
use super::runtime_contract_builder::{
    inject_agent_tool_policy, inject_default_stdio_mcp, inject_project_mcp_servers,
    inject_read_only_flag, inject_response_schema_into_launch_args, inject_workflow_mcp_servers,
    load_phase_capabilities, phase_agent_id_for, phase_decision_contract_for,
    phase_fallback_models_for, phase_model_override_for, phase_output_contract_for,
    phase_output_json_schema_for, phase_response_json_schema_for, phase_tool_override_for,
};
use crate::phase_failover::PhaseFailureClassifier;
use crate::phase_targets::PhaseTargetPlanner;
use crate::runtime_support::{
    inject_cli_launch_overrides, phase_max_continuations, phase_runner_attempts,
    phase_timeout_secs, WorkflowPhaseRuntimeSettings,
};

#[derive(Debug, Clone, Default)]
pub struct PhaseExecuteOverrides {
    pub tool: Option<String>,
    pub model: Option<String>,
    pub rework_context: Option<String>,
}

#[derive(Default)]
pub struct CliPhaseExecutor;

#[async_trait]
impl orchestrator_core::PhaseExecutor for CliPhaseExecutor {
    async fn execute_phase(
        &self,
        request: orchestrator_core::PhaseExecutionRequest,
    ) -> Result<orchestrator_core::PhaseExecutionResult> {
        let hub = orchestrator_core::FileServiceHub::new(&request.project_root)?;

        let (subject_id, subject_title, subject_description, task_complexity) =
            if let Ok(task) = hub.tasks().get(&request.task_id).await {
                (
                    task.id.clone(),
                    task.title.clone(),
                    task.description.clone(),
                    Some(task.complexity),
                )
            } else if let Ok(req) = hub.planning().get_requirement(&request.task_id).await {
                (
                    req.id.clone(),
                    req.title.clone(),
                    req.description.clone(),
                    None,
                )
            } else {
                (
                    request.task_id.clone(),
                    request.task_id.clone(),
                    String::new(),
                    None,
                )
            };

        let execution_cwd = if Path::new(&request.config_dir).is_dir() {
            request.config_dir.clone()
        } else {
            request.project_root.clone()
        };

        let timeout_override = request.timeout;

        let previous_timeout = if timeout_override.is_some() {
            std::env::var_os("AO_PHASE_TIMEOUT_SECS")
        } else {
            None
        };
        if let Some(timeout_secs) = timeout_override {
            std::env::set_var("AO_PHASE_TIMEOUT_SECS", timeout_secs.to_string());
        }

        let overrides = if request.tool_override.is_some() || request.model_override.is_some() {
            Some(PhaseExecuteOverrides {
                tool: request.tool_override,
                model: request.model_override,
                rework_context: None,
            })
        } else {
            None
        };

        let run_result = run_workflow_phase(
            &request.project_root,
            &execution_cwd,
            &request.workflow_ref,
            &request.workflow_ref,
            &subject_id,
            &subject_title,
            &subject_description,
            task_complexity,
            &request.phase_id,
            0,
            overrides.as_ref(),
            None,
        )
        .await;

        match previous_timeout {
            Some(previous_timeout) => {
                std::env::set_var("AO_PHASE_TIMEOUT_SECS", previous_timeout);
            }
            None => {
                if timeout_override.is_some() {
                    std::env::remove_var("AO_PHASE_TIMEOUT_SECS");
                }
            }
        }

        let run_result = run_result?;
        let output_log = serde_json::to_string_pretty(&run_result)?;

        let mut commit_message = None;
        let (verdict, exit_code, error) =
            phase_execution_result_values(&request.phase_id, &run_result.outcome);

        if let PhaseExecutionOutcome::Completed {
            commit_message: resolved_commit,
            phase_decision: Some(decision),
            ..
        } = &run_result.outcome
        {
            commit_message = resolved_commit
                .clone()
                .or_else(|| decision.commit_message.clone());
        }

        Ok(orchestrator_core::PhaseExecutionResult {
            exit_code,
            verdict,
            output_log,
            error,
            commit_message,
        })
    }
}

fn phase_execution_result_values(
    phase_id: &str,
    outcome: &PhaseExecutionOutcome,
) -> (orchestrator_core::PhaseVerdict, i32, Option<String>) {
    match outcome {
        PhaseExecutionOutcome::Completed { phase_decision, .. } => match phase_decision {
            Some(decision) => match decision.verdict {
                orchestrator_core::PhaseDecisionVerdict::Advance => {
                    (orchestrator_core::PhaseVerdict::Advance, 0, None)
                }
                orchestrator_core::PhaseDecisionVerdict::Rework => (
                    orchestrator_core::PhaseVerdict::Rework {
                        target_phase: decision
                            .target_phase
                            .clone()
                            .unwrap_or_else(|| phase_id.to_string()),
                    },
                    0,
                    None,
                ),
                orchestrator_core::PhaseDecisionVerdict::Skip => {
                    (orchestrator_core::PhaseVerdict::Skip, 0, None)
                }
                orchestrator_core::PhaseDecisionVerdict::Fail => {
                    let reason = if decision.reason.trim().is_empty() {
                        "phase verdict fail".to_string()
                    } else {
                        decision.reason.clone()
                    };
                    (
                        orchestrator_core::PhaseVerdict::Failed {
                            reason: reason.clone(),
                        },
                        1,
                        Some(reason),
                    )
                }
                orchestrator_core::PhaseDecisionVerdict::Unknown => {
                    let reason = "phase verdict unknown".to_string();
                    (
                        orchestrator_core::PhaseVerdict::Failed {
                            reason: reason.clone(),
                        },
                        1,
                        Some(reason),
                    )
                }
            },
            None => (orchestrator_core::PhaseVerdict::Advance, 0, None),
        },
        PhaseExecutionOutcome::NeedsResearch { reason } => (
            orchestrator_core::PhaseVerdict::Rework {
                target_phase: phase_id.to_string(),
            },
            0,
            Some(reason.clone()),
        ),
        PhaseExecutionOutcome::ManualPending { instructions, .. } => {
            let reason = format!("manual review required: {instructions}");
            (
                orchestrator_core::PhaseVerdict::Failed {
                    reason: reason.clone(),
                },
                1,
                Some(reason),
            )
        }
    }
}

pub(super) fn load_agent_runtime_config(
    project_root: &str,
) -> orchestrator_core::AgentRuntimeConfig {
    orchestrator_core::load_agent_runtime_config_or_default(Path::new(project_root))
}

fn load_agent_runtime_config_strict(
    project_root: &str,
) -> Result<orchestrator_core::LoadedAgentRuntimeConfig> {
    orchestrator_core::agent_runtime_config::load_agent_runtime_config_with_metadata(Path::new(
        project_root,
    ))
}

fn load_workflow_config_strict(
    project_root: &str,
) -> Result<orchestrator_core::LoadedWorkflowConfig> {
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseExecutionSignal {
    pub event_type: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseExecutionRunResult {
    pub outcome: PhaseExecutionOutcome,
    pub metadata: PhaseExecutionMetadata,
    #[serde(default)]
    pub signals: Vec<PhaseExecutionSignal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PhaseExecutionOutcome {
    Completed {
        commit_message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase_decision: Option<orchestrator_core::PhaseDecision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_payload: Option<Value>,
    },
    NeedsResearch {
        reason: String,
    },
    ManualPending {
        instructions: String,
        approval_note_required: bool,
    },
}

pub fn parse_research_reason_from_payload(payload: &Value) -> Option<String> {
    match payload {
        Value::Array(items) => items.iter().find_map(parse_research_reason_from_payload),
        Value::Object(object) => {
            let is_research_signal = object
                .get("kind")
                .and_then(Value::as_str)
                .map(|value| value.eq_ignore_ascii_case("research_required"))
                .unwrap_or(false)
                || object
                    .get("ao_control")
                    .and_then(Value::as_str)
                    .map(|value| value.eq_ignore_ascii_case("research_required"))
                    .unwrap_or(false)
                || object
                    .get("research_required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            if is_research_signal {
                if let Some(reason) = object
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        object
                            .get("research")
                            .and_then(|value| value.get("reason"))
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(ToOwned::to_owned)
                    })
                {
                    return Some(reason);
                }
            }

            for key in ["proposal", "data", "payload", "result", "output", "item"] {
                if let Some(value) = object.get(key) {
                    if let Some(reason) = parse_research_reason_from_payload(value) {
                        return Some(reason);
                    }
                }
            }

            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    if let Some(reason) = parse_research_reason_from_text(raw) {
                        return Some(reason);
                    }
                }
            }

            None
        }
        Value::String(text) => parse_research_reason_from_text(text),
        _ => None,
    }
}

pub fn parse_research_reason_from_text(text: &str) -> Option<String> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(reason) = parse_research_reason_from_payload(&payload) {
            return Some(reason);
        }
    }

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(reason) = trimmed.strip_prefix("AO_RESEARCH_REQUIRED:") {
            let reason = reason.trim();
            if !reason.is_empty() {
                return Some(reason.to_string());
            }
        }
    }

    None
}

fn normalize_phase_decision_payload(payload: &Value) -> Value {
    match payload {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(normalize_phase_decision_payload)
                .collect::<Vec<_>>(),
        ),
        Value::Object(object) => {
            let mut normalized = serde_json::Map::new();
            for (key, value) in object {
                normalized.insert(key.clone(), normalize_phase_decision_payload(value));
            }

            let is_phase_decision = normalized
                .get("kind")
                .and_then(Value::as_str)
                .map(|value| value.eq_ignore_ascii_case("phase_decision"))
                .unwrap_or(false);
            if is_phase_decision {
                if let Some(confidence) = normalized.get_mut("confidence") {
                    if let Some(raw) = confidence.as_str() {
                        if let Ok(parsed) = raw.trim().parse::<f64>() {
                            *confidence = serde_json::json!(parsed);
                        }
                    }
                }
            }

            Value::Object(normalized)
        }
        _ => payload.clone(),
    }
}

fn parse_phase_decision_from_payload(payload: &Value) -> Option<orchestrator_core::PhaseDecision> {
    match payload {
        Value::Array(items) => items.iter().find_map(parse_phase_decision_from_payload),
        Value::Object(object) => {
            let is_decision = object
                .get("kind")
                .and_then(Value::as_str)
                .map(|v| v.eq_ignore_ascii_case("phase_decision"))
                .unwrap_or(false);
            if is_decision {
                let normalized = normalize_phase_decision_payload(payload);
                if let Ok(decision) =
                    serde_json::from_value::<orchestrator_core::PhaseDecision>(normalized)
                {
                    return Some(decision);
                }
            }

            for key in [
                "proposal",
                "data",
                "payload",
                "result",
                "output",
                "item",
                "phase_decision",
            ] {
                if let Some(value) = object.get(key) {
                    if let Some(decision) = parse_phase_decision_from_payload(value) {
                        return Some(decision);
                    }
                }
            }

            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    if let Some(decision) = parse_phase_decision_from_text(raw) {
                        return Some(decision);
                    }
                }
            }

            None
        }
        Value::String(text) => parse_phase_decision_from_text(text),
        _ => None,
    }
}

pub fn parse_phase_decision_from_text(text: &str) -> Option<orchestrator_core::PhaseDecision> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(decision) = parse_phase_decision_from_payload(&payload) {
            return Some(decision);
        }
    }
    None
}

fn parse_phase_decision_payload_from_payload(payload: &Value) -> Option<Value> {
    match payload {
        Value::Array(items) => items
            .iter()
            .find_map(parse_phase_decision_payload_from_payload),
        Value::Object(object) => {
            let is_decision = object
                .get("kind")
                .and_then(Value::as_str)
                .map(|v| v.eq_ignore_ascii_case("phase_decision"))
                .unwrap_or(false);
            if is_decision {
                return Some(normalize_phase_decision_payload(payload));
            }

            for key in [
                "proposal",
                "data",
                "payload",
                "result",
                "output",
                "item",
                "phase_decision",
            ] {
                if let Some(value) = object.get(key) {
                    if let Some(decision) = parse_phase_decision_payload_from_payload(value) {
                        return Some(decision);
                    }
                }
            }

            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    if let Some(decision) = parse_phase_decision_payload_from_text(raw) {
                        return Some(decision);
                    }
                }
            }

            None
        }
        Value::String(text) => parse_phase_decision_payload_from_text(text),
        _ => None,
    }
}

fn parse_phase_decision_payload_from_text(text: &str) -> Option<Value> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(decision) = parse_phase_decision_payload_from_payload(&payload) {
            return Some(decision);
        }
    }

    None
}

fn parse_commit_message_from_payload_for_kind(
    payload: &Value,
    expected_kind: &str,
) -> Option<String> {
    match payload {
        Value::Array(items) => items
            .iter()
            .find_map(|item| parse_commit_message_from_payload_for_kind(item, expected_kind)),
        Value::Object(object) => {
            let kind = object
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if kind.eq_ignore_ascii_case(expected_kind) {
                if let Some(commit_message) = object
                    .get("commit_message")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                {
                    return Some(commit_message);
                }
            }

            for key in ["proposal", "data", "payload", "result", "output", "item"] {
                if let Some(value) = object.get(key) {
                    if let Some(commit_message) =
                        parse_commit_message_from_payload_for_kind(value, expected_kind)
                    {
                        return Some(commit_message);
                    }
                }
            }

            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    if let Some(commit_message) =
                        parse_commit_message_from_text_for_kind(raw, expected_kind)
                    {
                        return Some(commit_message);
                    }
                }
            }

            None
        }
        Value::String(text) => parse_commit_message_from_text_for_kind(text, expected_kind),
        _ => None,
    }
}

#[cfg(test)]
pub fn parse_commit_message_from_payload(payload: &Value) -> Option<String> {
    parse_commit_message_from_payload_for_kind(payload, "implementation_result")
}

fn parse_commit_message_from_text_for_kind(text: &str, expected_kind: &str) -> Option<String> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(commit_message) =
            parse_commit_message_from_payload_for_kind(&payload, expected_kind)
        {
            return Some(commit_message);
        }
    }

    None
}

fn parse_result_payload_from_payload_for_kind(
    payload: &Value,
    expected_kind: &str,
) -> Option<Value> {
    match payload {
        Value::Array(items) => items
            .iter()
            .find_map(|item| parse_result_payload_from_payload_for_kind(item, expected_kind)),
        Value::Object(object) => {
            let kind = object
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if kind.eq_ignore_ascii_case(expected_kind) {
                return Some(normalize_phase_decision_payload(payload));
            }

            for key in ["proposal", "data", "payload", "result", "output", "item"] {
                if let Some(value) = object.get(key) {
                    if let Some(result_payload) =
                        parse_result_payload_from_payload_for_kind(value, expected_kind)
                    {
                        return Some(result_payload);
                    }
                }
            }

            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    if let Some(result_payload) =
                        parse_result_payload_from_text_for_kind(raw, expected_kind)
                    {
                        return Some(result_payload);
                    }
                }
            }

            None
        }
        Value::String(text) => parse_result_payload_from_text_for_kind(text, expected_kind),
        _ => None,
    }
}

fn parse_result_payload_from_text_for_kind(text: &str, expected_kind: &str) -> Option<Value> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(result_payload) =
            parse_result_payload_from_payload_for_kind(&payload, expected_kind)
        {
            return Some(result_payload);
        }
    }

    None
}

pub fn parse_commit_message_from_text(text: &str) -> Option<String> {
    parse_commit_message_from_text_for_kind(text, "implementation_result")
}

pub fn fallback_implementation_commit_message(task_id: &str, task_title: &str) -> String {
    let scope = task_id.trim().to_ascii_lowercase();
    let summary = task_title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();

    match (scope.is_empty(), summary.is_empty()) {
        (false, false) => format!("feat({scope}): {summary}"),
        (false, true) => format!("feat({scope}): apply implementation changes"),
        (true, false) => format!("feat: {summary}"),
        (true, true) => "feat: apply implementation changes".to_string(),
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

pub async fn run_workflow_phase_attempt(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    request: &AgentRunRequest,
    parse_research_signal: bool,
) -> Result<PhaseExecutionOutcome> {
    let tool_id = request
        .context
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("codex");
    let parse_commit_message = phase_requires_commit_message_with_config(project_root, phase_id);
    let config_dir = runner_config_dir(Path::new(project_root));
    let stream = connect_runner(&config_dir).await.with_context(|| {
        format!(
            "failed to connect runner for workflow {} phase {}",
            workflow_id, phase_id
        )
    })?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, request).await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut pending_research_reason: Option<String> = None;
    let mut pending_commit_message: Option<String> = None;
    let mut pending_phase_decision: Option<orchestrator_core::PhaseDecision> = None;
    let mut pending_result_payload: Option<Value> = None;
    let parse_phase_decision = phase_decision_contract_for(project_root, phase_id).is_some();
    let expected_result_kind = phase_result_kind_for(project_root, phase_id);
    let mut provider_exhaustion_reason: Option<String> = None;
    let mut diagnostics = VecDeque::new();
    let stream_level = std::env::var("AO_STREAM_PHASE_OUTPUT").unwrap_or_default();
    let stream_normal = matches!(stream_level.as_str(), "1" | "normal");
    let stream_verbose = stream_level == "verbose";
    let stream_to_stderr = stream_normal || stream_verbose;
    let use_colors = stream_to_stderr && {
        use std::io::IsTerminal;
        std::io::stderr().is_terminal()
    };
    let mut last_ended_with_newline = true;
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, &request.run_id) {
            continue;
        }

        match event {
            AgentRunEvent::OutputChunk { text, .. } => {
                if provider_exhaustion_reason.is_none() {
                    provider_exhaustion_reason =
                        PhaseFailureClassifier::provider_exhaustion_reason_from_text(&text);
                }
                PhaseFailureClassifier::push_phase_diagnostic_line(&mut diagnostics, &text);
                if parse_research_signal {
                    if let Some(reason) = parse_research_reason_from_text(&text) {
                        if pending_research_reason.is_none() {
                            pending_research_reason = Some(reason);
                        }
                    }
                }
                if parse_commit_message && pending_commit_message.is_none() {
                    if let Some(commit_message) = parse_commit_message_from_text_for_kind(
                        &text,
                        expected_result_kind.as_str(),
                    ) {
                        pending_commit_message = Some(commit_message);
                    }
                }
                if parse_phase_decision && pending_phase_decision.is_none() {
                    if let Some(decision) = parse_phase_decision_from_text(&text) {
                        if pending_commit_message.is_none() {
                            if let Some(ref cm) = decision.commit_message {
                                pending_commit_message = Some(cm.clone());
                            }
                        }
                        pending_phase_decision = Some(decision);
                    }
                }
                if pending_result_payload.is_none() {
                    pending_result_payload = parse_result_payload_from_text_for_kind(
                        &text,
                        expected_result_kind.as_str(),
                    );
                }
                if pending_result_payload.is_none() && parse_phase_decision {
                    pending_result_payload = parse_phase_decision_payload_from_text(&text);
                }
                if stream_to_stderr {
                    use std::io::Write as _;
                    if let Some(formatted) =
                        format_output_chunk_for_display(&text, stream_verbose, use_colors, tool_id)
                    {
                        if !formatted.is_empty() {
                            let _ = write!(std::io::stderr(), "{}", formatted);
                            last_ended_with_newline = formatted.ends_with('\n');
                        }
                    }
                }
            }
            AgentRunEvent::Thinking { content, .. } => {
                if provider_exhaustion_reason.is_none() {
                    provider_exhaustion_reason =
                        PhaseFailureClassifier::provider_exhaustion_reason_from_text(&content);
                }
                PhaseFailureClassifier::push_phase_diagnostic_line(&mut diagnostics, &content);
                if parse_research_signal {
                    if let Some(reason) = parse_research_reason_from_text(&content) {
                        if pending_research_reason.is_none() {
                            pending_research_reason = Some(reason);
                        }
                    }
                }
                if parse_commit_message && pending_commit_message.is_none() {
                    if let Some(commit_message) = parse_commit_message_from_text_for_kind(
                        &content,
                        expected_result_kind.as_str(),
                    ) {
                        pending_commit_message = Some(commit_message);
                    }
                }
                if parse_phase_decision && pending_phase_decision.is_none() {
                    if let Some(decision) = parse_phase_decision_from_text(&content) {
                        if pending_commit_message.is_none() {
                            if let Some(ref cm) = decision.commit_message {
                                pending_commit_message = Some(cm.clone());
                            }
                        }
                        pending_phase_decision = Some(decision);
                    }
                }
                if pending_result_payload.is_none() {
                    pending_result_payload = parse_result_payload_from_text_for_kind(
                        &content,
                        expected_result_kind.as_str(),
                    );
                }
                if pending_result_payload.is_none() && parse_phase_decision {
                    pending_result_payload = parse_phase_decision_payload_from_text(&content);
                }
                if stream_verbose {
                    use std::io::Write as _;
                    let (dim, reset) = if use_colors {
                        ("\x1b[2m", "\x1b[0m")
                    } else {
                        ("", "")
                    };
                    let _ = write!(std::io::stderr(), "{dim}{content}{reset}");
                }
            }
            AgentRunEvent::Error { error, .. } => {
                PhaseFailureClassifier::push_phase_diagnostic_line(&mut diagnostics, &error);
                let exhaustion_reason = provider_exhaustion_reason.clone().or_else(|| {
                    PhaseFailureClassifier::provider_exhaustion_reason_from_text(&error)
                });
                return Err(anyhow!(
                    "workflow {} phase {} error: {}{}",
                    workflow_id,
                    phase_id,
                    error,
                    exhaustion_reason
                        .map(|reason| format!(" (provider_exhausted: {reason})"))
                        .unwrap_or_default()
                ));
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    let diagnostics_summary =
                        PhaseFailureClassifier::summarize_phase_diagnostics(&diagnostics);
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
                        exhaustion_reason
                            .map(|reason| format!(" (provider_exhausted: {reason})"))
                            .unwrap_or_default(),
                        diagnostics_summary
                            .map(|summary| format!("; diagnostics: {summary}"))
                            .unwrap_or_default(),
                    ));
                }
                if let Some(reason) = pending_research_reason {
                    return Ok(PhaseExecutionOutcome::NeedsResearch { reason });
                }
                return Ok(PhaseExecutionOutcome::Completed {
                    commit_message: pending_commit_message,
                    phase_decision: pending_phase_decision,
                    result_payload: pending_result_payload,
                });
            }
            AgentRunEvent::ToolCall { tool_info, .. } => {
                if stream_to_stderr && tool_info.tool_name != "phase_transition" {
                    use std::io::Write as _;
                    if !last_ended_with_newline {
                        let _ = writeln!(std::io::stderr());
                        last_ended_with_newline = true;
                    }
                    let formatted = format_tool_call_for_display(
                        &tool_info.tool_name,
                        &tool_info.parameters,
                        use_colors,
                    );
                    let _ = write!(std::io::stderr(), "{}", formatted);
                }
            }
            AgentRunEvent::Artifact { artifact_info, .. } => {
                if stream_verbose {
                    use std::io::Write as _;
                    let (dim, reset) = if use_colors {
                        ("\x1b[2m", "\x1b[0m")
                    } else {
                        ("", "")
                    };
                    let path = artifact_info.file_path.as_deref().unwrap_or("unknown");
                    let _ = writeln!(std::io::stderr(), "{dim}  artifact: {path}{reset}");
                }
            }
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

pub async fn run_workflow_phase_with_agent(
    project_root: &str,
    execution_cwd: &str,
    workflow_id: &str,
    subject_id: &str,
    subject_title: &str,
    subject_description: &str,
    task_complexity: Option<orchestrator_core::Complexity>,
    phase_id: &str,
    phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>,
    overrides: Option<&PhaseExecuteOverrides>,
    pipeline_vars: Option<&std::collections::HashMap<String, String>>,
) -> Result<PhaseExecutionOutcome> {
    let caps = load_phase_capabilities(project_root, phase_id);
    let routing_complexity = routing_complexity(task_complexity);
    let settings_tool = phase_runtime_settings.and_then(|s| s.tool.as_deref());
    let settings_model = phase_runtime_settings.and_then(|s| s.model.as_deref());
    let agent_model_override = phase_model_override_for(project_root, phase_id);
    let agent_tool_override = phase_tool_override_for(project_root, phase_id);
    let agent_fallback_models = phase_fallback_models_for(project_root, phase_id);
    let configured_fallback_models = if agent_fallback_models.is_empty() {
        phase_runtime_settings
            .map(|settings| settings.fallback_models.clone())
            .unwrap_or_default()
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
        &caps,
    );
    let parse_research_signal = !caps.is_research;
    let prompt = build_phase_prompt(
        project_root,
        execution_cwd,
        workflow_id,
        subject_id,
        subject_title,
        subject_description,
        phase_id,
        overrides.and_then(|o| o.rework_context.as_deref()),
        pipeline_vars,
    );
    let max_attempts = phase_runtime_settings
        .and_then(|settings| settings.max_attempts)
        .unwrap_or_else(phase_runner_attempts);
    let max_continuations = phase_runtime_settings
        .and_then(|settings| settings.max_continuations)
        .unwrap_or_else(phase_max_continuations);
    let session_id = Uuid::new_v4().to_string();
    let mut fallover_errors: Vec<String> = Vec::new();

    for (target_index, (target_tool_id, target_model_id)) in execution_targets.iter().enumerate() {
        let mut last_outcome: Option<PhaseExecutionOutcome> = None;

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

            let resume_plan = orchestrator_core::runtime_contract::CliSessionResumePlan {
                mode: orchestrator_core::runtime_contract::CliSessionResumeMode::NativeId,
                session_key: format!("wf:{workflow_id}:{phase_id}"),
                session_id: Some(session_id.clone()),
                summary_seed: None,
                reused: is_continuation,
                phase_thread_isolated: true,
            };

            let mut context = serde_json::json!({
                "tool": target_tool_id,
                "prompt": effective_prompt,
                "cwd": execution_cwd,
                "project_root": project_root,
                "workflow_id": workflow_id,
                "subject_id": subject_id,
                "phase_id": phase_id,
            });
            if let Some(agent_id) = phase_agent_id_for(project_root, phase_id) {
                context
                    .as_object_mut()
                    .expect("json object")
                    .insert("agent_id".to_string(), serde_json::json!(agent_id));
            }
            let phase_contract = phase_output_contract_for(project_root, phase_id);
            let phase_output_schema = phase_output_json_schema_for(project_root, phase_id);
            let phase_response_schema = phase_response_json_schema_for(project_root, phase_id);
            if let Some(mut runtime_contract) = build_runtime_contract_with_resume(
                context
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("codex"),
                target_model_id,
                &effective_prompt,
                Some(&resume_plan),
            ) {
                if let Some(contract) = phase_contract.as_ref() {
                    let mut policy = serde_json::json!({
                        "require_commit_message": contract.requires_field("commit_message"),
                        "required_result_kind": contract.kind.as_str(),
                        "required_result_fields": contract.required_fields.clone(),
                    });
                    if let Some(schema) = phase_response_schema
                        .clone()
                        .or(phase_output_schema.clone())
                    {
                        policy
                            .as_object_mut()
                            .expect("json object")
                            .insert("output_json_schema".to_string(), schema);
                    }
                    runtime_contract
                        .as_object_mut()
                        .expect("json object")
                        .insert("policy".to_string(), policy);
                }
                let agent_config = load_agent_runtime_config(project_root);
                if let Some(schema) = phase_response_schema.as_ref() {
                    inject_response_schema_into_launch_args(
                        &mut runtime_contract,
                        schema,
                        &agent_config,
                    );
                }
                if !caps.writes_files {
                    inject_read_only_flag(&mut runtime_contract, &agent_config);
                }
                inject_cli_launch_overrides(
                    &mut runtime_contract,
                    context
                        .get("tool")
                        .and_then(Value::as_str)
                        .unwrap_or("codex"),
                    phase_runtime_settings,
                );
                inject_default_stdio_mcp(&mut runtime_contract, project_root);
                inject_agent_tool_policy(&mut runtime_contract, project_root, phase_id);
                inject_project_mcp_servers(&mut runtime_contract, project_root, phase_id);
                inject_workflow_mcp_servers(&mut runtime_contract, project_root, phase_id);
                context
                    .as_object_mut()
                    .expect("json object")
                    .insert("runtime_contract".to_string(), runtime_contract);
            }

            let run_id = RunId(format!(
                "wf-{workflow_id}-{}-{target_index}-c{continuation}-{}",
                phase_id,
                Uuid::new_v4().simple()
            ));
            let request = AgentRunRequest {
                protocol_version: PROTOCOL_VERSION.to_string(),
                run_id,
                model: ModelId(target_model_id.clone()),
                context,
                timeout_secs: phase_runtime_settings
                    .and_then(|settings| settings.timeout_secs)
                    .or_else(phase_timeout_secs),
            };

            let mut attempt_succeeded = false;
            let mut backoff = Duration::from_millis(200);
            for attempt in 1..=max_attempts {
                match run_workflow_phase_attempt(
                    project_root,
                    workflow_id,
                    phase_id,
                    &request,
                    parse_research_signal,
                )
                .await
                {
                    Ok(mut outcome) => {
                        if phase_requires_commit_message_with_config(project_root, phase_id) {
                            if let PhaseExecutionOutcome::Completed { commit_message, .. } =
                                &mut outcome
                            {
                                let resolved_commit_message =
                                    commit_message.clone().unwrap_or_else(|| {
                                        fallback_implementation_commit_message(
                                            subject_id,
                                            subject_title,
                                        )
                                    });
                                commit_implementation_changes(
                                    execution_cwd,
                                    &resolved_commit_message,
                                )?;
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
                            sleep(backoff).await;
                            backoff =
                                std::cmp::min(backoff.saturating_mul(2), Duration::from_secs(3));
                            continue;
                        }

                        let has_fallback_target = target_index + 1 < execution_targets.len();
                        if has_fallback_target
                            && PhaseFailureClassifier::should_failover_target(&message)
                        {
                            fallover_errors.push(format!(
                                "target {}:{} failed: {}",
                                target_tool_id, target_model_id, message
                            ));
                            orchestrator_core::record_model_phase_outcome(
                                std::path::Path::new(project_root),
                                target_model_id,
                                phase_id,
                                orchestrator_core::PhaseDecisionVerdict::Fail,
                            );
                            break;
                        }
                        orchestrator_core::record_model_phase_outcome(
                            std::path::Path::new(project_root),
                            target_model_id,
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

            let outcome_is_complete = match &last_outcome {
                Some(PhaseExecutionOutcome::Completed {
                    phase_decision,
                    commit_message,
                    result_payload,
                }) => {
                    phase_decision.is_some() || commit_message.is_some() || result_payload.is_some()
                }
                Some(PhaseExecutionOutcome::NeedsResearch { .. }) => true,
                Some(PhaseExecutionOutcome::ManualPending { .. }) => true,
                None => false,
            };

            if outcome_is_complete {
                let outcome = last_outcome.take().expect("outcome verified above");
                let verdict = match &outcome {
                    PhaseExecutionOutcome::Completed { phase_decision, .. } => phase_decision
                        .as_ref()
                        .map(|d| d.verdict)
                        .unwrap_or(orchestrator_core::PhaseDecisionVerdict::Advance),
                    PhaseExecutionOutcome::NeedsResearch { .. } => {
                        orchestrator_core::PhaseDecisionVerdict::Rework
                    }
                    PhaseExecutionOutcome::ManualPending { .. } => {
                        orchestrator_core::PhaseDecisionVerdict::Advance
                    }
                };
                orchestrator_core::record_model_phase_outcome(
                    std::path::Path::new(project_root),
                    target_model_id,
                    phase_id,
                    verdict,
                );
                return Ok(outcome);
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
            let verdict = match &outcome {
                PhaseExecutionOutcome::Completed { phase_decision, .. } => phase_decision
                    .as_ref()
                    .map(|d| d.verdict)
                    .unwrap_or(orchestrator_core::PhaseDecisionVerdict::Advance),
                PhaseExecutionOutcome::NeedsResearch { .. } => {
                    orchestrator_core::PhaseDecisionVerdict::Rework
                }
                PhaseExecutionOutcome::ManualPending { .. } => {
                    orchestrator_core::PhaseDecisionVerdict::Advance
                }
            };
            orchestrator_core::record_model_phase_outcome(
                std::path::Path::new(project_root),
                target_model_id,
                phase_id,
                verdict,
            );
            return Ok(outcome);
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

fn validate_basic_json_schema(instance: &Value, schema: &Value) -> Result<()> {
    let schema_object = schema
        .as_object()
        .ok_or_else(|| anyhow!("schema must be a JSON object"))?;

    if let Some(required_fields) = schema_object.get("required").and_then(Value::as_array) {
        let instance_object = instance
            .as_object()
            .ok_or_else(|| anyhow!("instance must be a JSON object"))?;
        for required in required_fields {
            let Some(field) = required.as_str() else {
                continue;
            };
            if !instance_object.contains_key(field) {
                return Err(anyhow!(
                    "schema validation failed: missing required field '{}'",
                    field
                ));
            }
        }
    }

    if let Some(properties) = schema_object.get("properties").and_then(Value::as_object) {
        let instance_object = instance
            .as_object()
            .ok_or_else(|| anyhow!("instance must be a JSON object"))?;
        for (key, rule) in properties {
            let Some(value) = instance_object.get(key) else {
                continue;
            };

            if let Some(expected_type) = rule.get("type").and_then(Value::as_str) {
                if !validate_schema_type(expected_type, value) {
                    return Err(anyhow!(
                        "schema validation failed: field '{}' must be type '{}'",
                        key,
                        expected_type
                    ));
                }
            }

            if let Some(constant) = rule.get("const") {
                if value != constant {
                    return Err(anyhow!(
                        "schema validation failed: field '{}' must equal {}",
                        key,
                        constant
                    ));
                }
            }
        }
    }

    Ok(())
}

fn parse_command_json_output(stdout: &str) -> Result<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("command output is empty; expected JSON payload"));
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Ok(value);
    }

    let payloads = collect_json_payload_lines(stdout);
    payloads
        .last()
        .map(|(_, payload)| payload.clone())
        .ok_or_else(|| anyhow!("unable to parse JSON payload from command output"))
}

fn validate_command_contract(
    payload: &Value,
    expected_kind: Option<&str>,
    expected_schema: Option<&Value>,
) -> Result<()> {
    if let Some(kind) = expected_kind
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let payload_kind = payload
            .get("kind")
            .and_then(Value::as_str)
            .map(str::trim)
            .ok_or_else(|| anyhow!("payload is missing required field 'kind'"))?;
        if !payload_kind.eq_ignore_ascii_case(kind) {
            return Err(anyhow!(
                "payload kind mismatch: expected '{}', got '{}'",
                kind,
                payload_kind
            ));
        }
    }

    if let Some(schema) = expected_schema {
        validate_basic_json_schema(payload, schema)?;
    }

    Ok(())
}

fn is_program_allowlisted(program: &str, allowlist: &[String]) -> bool {
    let command = Path::new(program)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(program)
        .trim()
        .to_ascii_lowercase();
    if command.is_empty() {
        return false;
    }

    allowlist
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .any(|candidate| candidate.eq_ignore_ascii_case(command.as_str()))
}

#[derive(Debug, Clone, Copy)]
struct CommandExecutionContext<'a> {
    project_root: &'a str,
    execution_cwd: &'a str,
    workflow_id: &'a str,
    phase_id: &'a str,
    workflow_ref: &'a str,
    subject_id: &'a str,
    subject_title: &'a str,
    subject_description: &'a str,
    pipeline_vars: Option<&'a HashMap<String, String>>,
}

fn build_command_template_vars(context: &CommandExecutionContext<'_>) -> HashMap<String, String> {
    let mut vars = HashMap::from([
        ("project_root".to_string(), context.project_root.to_string()),
        (
            "execution_cwd".to_string(),
            context.execution_cwd.to_string(),
        ),
        ("workflow_id".to_string(), context.workflow_id.to_string()),
        ("phase_id".to_string(), context.phase_id.to_string()),
        ("workflow_ref".to_string(), context.workflow_ref.to_string()),
        ("subject_id".to_string(), context.subject_id.to_string()),
        (
            "subject_title".to_string(),
            context.subject_title.to_string(),
        ),
        (
            "subject_description".to_string(),
            context.subject_description.to_string(),
        ),
    ]);

    if let Some(pipeline_vars) = context.pipeline_vars {
        for (key, value) in pipeline_vars {
            vars.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }

    if let Some(dispatch_input) = std::env::var("AO_DISPATCH_INPUT")
        .ok()
        .filter(|value| !value.is_empty())
    {
        vars.entry("dispatch_input".to_string())
            .or_insert(dispatch_input.clone());
        if context.subject_id.starts_with("schedule:") {
            vars.entry("schedule_input".to_string())
                .or_insert(dispatch_input);
        }
    } else if let Ok(schedule_input) = std::env::var("AO_SCHEDULE_INPUT") {
        if !schedule_input.is_empty() {
            vars.entry("schedule_input".to_string())
                .or_insert(schedule_input.clone());
            vars.entry("dispatch_input".to_string())
                .or_insert(schedule_input);
        }
    }

    vars
}

fn resolve_command_cwd(
    context: &CommandExecutionContext<'_>,
    command: &orchestrator_core::PhaseCommandDefinition,
    template_vars: &HashMap<String, String>,
) -> Result<String> {
    match command.cwd_mode {
        orchestrator_core::CommandCwdMode::ProjectRoot => Ok(context.project_root.to_string()),
        orchestrator_core::CommandCwdMode::TaskRoot => Ok(context.execution_cwd.to_string()),
        orchestrator_core::CommandCwdMode::Path => {
            let expanded = command
                .cwd_path
                .as_deref()
                .map(|value| orchestrator_config::expand_variables(value, template_vars))
                .ok_or_else(|| anyhow!("command.cwd_path is required when cwd_mode='path'"))?;
            let raw = expanded.trim();
            if raw.is_empty() {
                return Err(anyhow!("command.cwd_path is required when cwd_mode='path'"));
            }
            let relative = Path::new(raw);
            if relative.is_absolute() {
                return Err(anyhow!(
                    "command.cwd_path must be relative when cwd_mode='path'"
                ));
            }
            if relative
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(anyhow!("command.cwd_path cannot contain '..' components"));
            }
            Ok(Path::new(context.project_root)
                .join(relative)
                .display()
                .to_string())
        }
    }
}

#[derive(Debug, Clone)]
struct CommandExecutionResult {
    exit_code: i32,
    program: String,
    args: Vec<String>,
    stdout: String,
    stderr: String,
    cwd: String,
    duration_ms: u64,
    parsed_payload: Option<Value>,
    phase_decision: Option<orchestrator_core::PhaseDecision>,
    failure_summary: Option<String>,
}

fn summarize_output_excerpt(text: &str, max_len: usize) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let excerpt = if trimmed.chars().count() > max_len {
        let mut shortened = trimmed.chars().take(max_len).collect::<String>();
        shortened.push_str("...");
        shortened
    } else {
        trimmed.to_string()
    };
    Some(excerpt)
}

fn command_phase_evidence_kind(
    phase_id: &str,
    program: &str,
    args: &[String],
    success: bool,
) -> orchestrator_core::PhaseEvidenceKind {
    let normalized_phase = phase_id.to_ascii_lowercase();
    let normalized_program = program.to_ascii_lowercase();
    let normalized_args = args
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let looks_like_test = normalized_phase.contains("test")
        || normalized_program.contains("cargo") && normalized_args.iter().any(|arg| arg == "test");

    if looks_like_test {
        if success {
            orchestrator_core::PhaseEvidenceKind::TestsPassed
        } else {
            orchestrator_core::PhaseEvidenceKind::TestsFailed
        }
    } else {
        orchestrator_core::PhaseEvidenceKind::Custom
    }
}

fn command_failure_category(phase_id: &str, program: &str, args: &[String]) -> String {
    let normalized_phase = phase_id.to_ascii_lowercase();
    let normalized_program = program.to_ascii_lowercase();
    let normalized_args = args
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();

    if normalized_phase.contains("test")
        || normalized_program.contains("cargo") && normalized_args.iter().any(|arg| arg == "test")
    {
        "tests_failed".to_string()
    } else if normalized_phase.contains("lint")
        || normalized_program.contains("clippy")
        || normalized_program.contains("rustfmt")
        || normalized_args
            .iter()
            .any(|arg| arg.contains("clippy") || arg.contains("fmt"))
    {
        "lint_failed".to_string()
    } else if normalized_phase.contains("build")
        || normalized_program.contains("cargo") && normalized_args.iter().any(|arg| arg == "build")
    {
        "build_failed".to_string()
    } else {
        "command_failed".to_string()
    }
}

fn extract_failing_tests(stdout: &str, stderr: &str) -> Vec<String> {
    let mut failing = Vec::new();
    for text in [stdout, stderr] {
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("test ") {
                if let Some((name, status)) = rest.split_once(" ... ") {
                    if status.trim().starts_with("FAILED") {
                        let candidate = name.trim().to_string();
                        if !candidate.is_empty() && !failing.contains(&candidate) {
                            failing.push(candidate);
                        }
                    }
                }
            }
        }
    }
    failing
}

fn build_command_phase_decision(
    phase_id: &str,
    program: &str,
    args: &[String],
    exit_code: i32,
    failure_summary: Option<&str>,
) -> orchestrator_core::PhaseDecision {
    let success = failure_summary.is_none();
    let kind = command_phase_evidence_kind(phase_id, program, args, success);
    let reason = failure_summary
        .map(str::to_string)
        .unwrap_or_else(|| format!("Command `{program}` completed successfully"));
    let verdict = if success {
        orchestrator_core::PhaseDecisionVerdict::Advance
    } else {
        orchestrator_core::PhaseDecisionVerdict::Rework
    };
    let risk = if success {
        orchestrator_core::WorkflowDecisionRisk::Low
    } else {
        orchestrator_core::WorkflowDecisionRisk::Medium
    };

    orchestrator_core::PhaseDecision {
        kind: "phase_decision".to_string(),
        phase_id: phase_id.to_string(),
        verdict,
        confidence: 1.0,
        risk,
        reason: reason.clone(),
        evidence: vec![orchestrator_core::PhaseEvidence {
            kind,
            description: format!("Command `{program}` exited with code {exit_code}"),
            file_path: None,
            value: Some(serde_json::json!({
                "program": program,
                "args": args,
                "exit_code": exit_code
            })),
        }],
        guardrail_violations: vec![],
        commit_message: None,
        target_phase: None,
    }
}

fn build_command_result_payload(
    phase_id: &str,
    contract_kind: Option<&str>,
    command_result: &CommandExecutionResult,
    phase_decision: &orchestrator_core::PhaseDecision,
) -> Value {
    let mut payload = match command_result.parsed_payload.clone() {
        Some(Value::Object(map)) => Value::Object(map),
        Some(other) => serde_json::json!({ "raw_payload": other }),
        None => serde_json::json!({}),
    };

    payload["kind"] = Value::String(
        payload
            .get("kind")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(contract_kind.unwrap_or("phase_result"))
            .to_string(),
    );
    payload["phase_id"] = Value::String(phase_id.to_string());
    payload["verdict"] =
        Value::String(format!("{:?}", phase_decision.verdict).to_ascii_lowercase());
    payload["reason"] = Value::String(phase_decision.reason.clone());
    payload["confidence"] = serde_json::json!(phase_decision.confidence);
    payload["risk"] = Value::String(format!("{:?}", phase_decision.risk).to_ascii_lowercase());
    payload["evidence"] =
        serde_json::to_value(&phase_decision.evidence).unwrap_or(Value::Array(vec![]));
    payload["exit_code"] = serde_json::json!(command_result.exit_code);
    payload["command"] = serde_json::json!({
        "program": command_result.program,
        "args": command_result.args
    });
    payload["duration_ms"] = serde_json::json!(command_result.duration_ms);

    if let Some(summary) = command_result.failure_summary.as_deref() {
        payload["failure_summary"] = Value::String(summary.to_string());
        payload["failure_category"] = Value::String(command_failure_category(
            phase_id,
            command_result.program.as_str(),
            &command_result.args,
        ));
        let failing_tests = extract_failing_tests(&command_result.stdout, &command_result.stderr);
        if !failing_tests.is_empty() {
            payload["failing_tests"] = Value::Array(
                failing_tests
                    .into_iter()
                    .map(Value::String)
                    .collect::<Vec<_>>(),
            );
        }
    }

    if let Some(stdout_excerpt) = summarize_output_excerpt(&command_result.stdout, 800) {
        payload["stdout_excerpt"] = Value::String(stdout_excerpt);
    }
    if let Some(stderr_excerpt) = summarize_output_excerpt(&command_result.stderr, 800) {
        payload["stderr_excerpt"] = Value::String(stderr_excerpt);
    }

    payload
}

#[derive(Debug)]
struct CommandStreamCapture {
    text: String,
    phase_decision: Option<orchestrator_core::PhaseDecision>,
}

async fn capture_command_stream<R>(
    reader: R,
    stream_output: bool,
    stream_verbose: bool,
    use_colors: bool,
) -> Result<CommandStreamCapture>
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut text = String::new();
    let mut phase_decision = None;

    while let Some(line) = lines.next_line().await? {
        text.push_str(&line);
        text.push('\n');

        if phase_decision.is_none() {
            phase_decision = parse_phase_decision_from_text(&line);
        }

        if stream_output {
            use std::io::Write as _;
            let display =
                format_output_chunk_for_display(&line, stream_verbose, use_colors, "command")
                    .unwrap_or_else(|| format!("{line}\n"));
            let _ = write!(std::io::stderr(), "{}", display);
        }
    }

    Ok(CommandStreamCapture {
        text,
        phase_decision,
    })
}

async fn run_workflow_phase_with_command(
    context: &CommandExecutionContext<'_>,
    runtime_config: &orchestrator_core::AgentRuntimeConfig,
    command: &orchestrator_core::PhaseCommandDefinition,
) -> Result<CommandExecutionResult> {
    if !is_program_allowlisted(&command.program, &runtime_config.tools_allowlist) {
        return Err(anyhow!(
            "phase '{}' command '{}' is not in tools_allowlist",
            context.phase_id,
            command.program
        ));
    }

    let template_vars = build_command_template_vars(context);
    let args = command
        .args
        .iter()
        .map(|arg| orchestrator_config::expand_variables(arg, &template_vars))
        .collect::<Vec<_>>();
    let env = command
        .env
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                orchestrator_config::expand_variables(value, &template_vars),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let cwd = resolve_command_cwd(context, command, &template_vars)?;
    let started = std::time::Instant::now();

    let mut process = TokioCommand::new(&command.program);
    process
        .args(&args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in &env {
        process.env(key, value);
    }

    let mut child = process.spawn()?;
    let stdout_reader = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture stdout for command phase"))?;
    let stderr_reader = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture stderr for command phase"))?;
    let stream_level = std::env::var("AO_STREAM_PHASE_OUTPUT").unwrap_or_default();
    let stream_normal = matches!(stream_level.as_str(), "1" | "normal");
    let stream_verbose = stream_level == "verbose";
    let stream_to_stderr = stream_normal || stream_verbose;
    let use_colors = stream_to_stderr && {
        use std::io::IsTerminal;
        std::io::stderr().is_terminal()
    };
    let stdout_task = tokio::spawn(capture_command_stream(
        stdout_reader,
        stream_to_stderr,
        stream_verbose,
        use_colors,
    ));
    let stderr_task = tokio::spawn(capture_command_stream(
        stderr_reader,
        stream_to_stderr,
        stream_verbose,
        use_colors,
    ));

    let status = if let Some(timeout_secs) = command.timeout_secs {
        match timeout(Duration::from_secs(timeout_secs), child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(anyhow!(
                    "phase '{}' command '{}' timed out after {} seconds",
                    context.phase_id,
                    command.program,
                    timeout_secs
                ));
            }
        }
    } else {
        child.wait().await?
    };

    let stdout_capture = stdout_task
        .await
        .map_err(|error| anyhow!("stdout capture task failed: {error}"))??;
    let stderr_capture = stderr_task
        .await
        .map_err(|error| anyhow!("stderr capture task failed: {error}"))??;

    let exit_code = status.code().unwrap_or(-1);
    let stdout = stdout_capture.text;
    let stderr = stderr_capture.text;
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let phase_decision = stdout_capture
        .phase_decision
        .or(stderr_capture.phase_decision)
        .or_else(|| parse_phase_decision_from_text(&stdout))
        .or_else(|| parse_phase_decision_from_text(&stderr));

    if !command.success_exit_codes.contains(&exit_code) {
        let mut failure_summary = format!(
            "Command `{}` exited with code {} (expected one of {:?}).",
            command.program, exit_code, command.success_exit_codes
        );
        if !stdout.trim().is_empty() {
            failure_summary.push_str("\n\nStdout:\n");
            failure_summary.push_str(stdout.trim());
        }
        if !stderr.trim().is_empty() {
            failure_summary.push_str("\n\nStderr:\n");
            failure_summary.push_str(stderr.trim());
        }
        return Ok(CommandExecutionResult {
            exit_code,
            program: command.program.clone(),
            args,
            stdout,
            stderr,
            cwd,
            duration_ms,
            parsed_payload: None,
            phase_decision,
            failure_summary: Some(failure_summary),
        });
    }

    let parsed_payload = if command.parse_json_output {
        let payload = parse_command_json_output(&stdout)?;
        validate_command_contract(
            &payload,
            command.expected_result_kind.as_deref(),
            command.expected_schema.as_ref(),
        )?;
        Some(payload)
    } else {
        None
    };

    Ok(CommandExecutionResult {
        exit_code,
        program: command.program.clone(),
        args,
        stdout,
        stderr,
        cwd,
        duration_ms,
        parsed_payload,
        phase_decision,
        failure_summary: None,
    })
}

#[cfg(test)]
mod command_phase_tests {
    use super::*;
    use std::fs;

    struct EnvVarGuard {
        key: String,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: impl Into<String>, value: Option<&str>) -> Self {
            let key = key.into();
            let original = std::env::var(&key).ok();
            match value {
                Some(value) => std::env::set_var(&key, value),
                None => std::env::remove_var(&key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(&self.key, value),
                None => std::env::remove_var(&self.key),
            }
        }
    }

    #[tokio::test]
    async fn command_phase_expands_context_vars_in_args_env_and_cwd() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let project_root = temp_dir.path().join("project");
        let execution_cwd = project_root.join("task-root");
        let command_cwd = project_root
            .join("commands")
            .join("implementation")
            .join("SUBJ-42");
        fs::create_dir_all(&execution_cwd).expect("create execution cwd");
        fs::create_dir_all(&command_cwd).expect("create command cwd");

        let mut runtime_config = orchestrator_core::AgentRuntimeConfig::default();
        runtime_config.tools_allowlist = vec!["sh".to_string()];

        let mut pipeline_vars = HashMap::new();
        pipeline_vars.insert("queue_reason".to_string(), "manual-review".to_string());
        pipeline_vars.insert("workflow_id".to_string(), "should-not-override".to_string());

        let context = CommandExecutionContext {
            project_root: project_root.to_str().expect("project root str"),
            execution_cwd: execution_cwd.to_str().expect("execution cwd str"),
            workflow_id: "wf-123",
            phase_id: "implementation",
            workflow_ref: "default",
            subject_id: "SUBJ-42",
            subject_title: "Subject Title",
            subject_description: "Refine queue state",
            pipeline_vars: Some(&pipeline_vars),
        };
        let command = orchestrator_core::PhaseCommandDefinition {
            program: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf '%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n' \"$1\" \"$2\" \"$3\" \"$SUBJECT_TITLE_ENV\" \"$PIPELINE_REASON_ENV\" \"$PWD\"".to_string(),
                "sh".to_string(),
                "{{subject_id}}".to_string(),
                "{{workflow_id}}".to_string(),
                "{{workflow_ref}}".to_string(),
            ],
            env: BTreeMap::from([
                (
                    "SUBJECT_TITLE_ENV".to_string(),
                    "{{subject_title}}".to_string(),
                ),
                (
                    "PIPELINE_REASON_ENV".to_string(),
                    "{{queue_reason}}".to_string(),
                ),
            ]),
            cwd_mode: orchestrator_core::CommandCwdMode::Path,
            cwd_path: Some("commands/{{phase_id}}/{{subject_id}}".to_string()),
            timeout_secs: Some(5),
            success_exit_codes: vec![0],
            parse_json_output: false,
            expected_result_kind: None,
            expected_schema: None,
        };

        let result = run_workflow_phase_with_command(&context, &runtime_config, &command)
            .await
            .expect("command phase should run");

        assert_eq!(result.exit_code, 0);
        assert_eq!(
            result.args,
            vec![
                "-c".to_string(),
                "printf '%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n' \"$1\" \"$2\" \"$3\" \"$SUBJECT_TITLE_ENV\" \"$PIPELINE_REASON_ENV\" \"$PWD\"".to_string(),
                "sh".to_string(),
                "SUBJ-42".to_string(),
                "wf-123".to_string(),
                "default".to_string(),
            ]
        );
        assert_eq!(
            fs::canonicalize(&result.cwd).expect("canonicalize result cwd"),
            fs::canonicalize(&command_cwd).expect("canonicalize expected cwd")
        );
        let stdout_lines = result.stdout.lines().collect::<Vec<_>>();
        assert_eq!(
            &stdout_lines[..5],
            &[
                "SUBJ-42",
                "wf-123",
                "default",
                "Subject Title",
                "manual-review"
            ]
        );
        assert_eq!(
            fs::canonicalize(stdout_lines[5]).expect("canonicalize reported cwd"),
            fs::canonicalize(&command_cwd).expect("canonicalize expected cwd")
        );
        assert!(result.phase_decision.is_none());
    }

    #[tokio::test]
    async fn command_phase_extracts_phase_decision_from_stdout() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let project_root = temp_dir.path().join("project");
        fs::create_dir_all(&project_root).expect("create project root");

        let mut runtime_config = orchestrator_core::AgentRuntimeConfig::default();
        runtime_config.tools_allowlist = vec!["sh".to_string()];

        let context = CommandExecutionContext {
            project_root: project_root.to_str().expect("project root str"),
            execution_cwd: project_root.to_str().expect("execution cwd str"),
            workflow_id: "wf-123",
            phase_id: "testing",
            workflow_ref: "default",
            subject_id: "TASK-1",
            subject_title: "Task",
            subject_description: "Task description",
            pipeline_vars: None,
        };
        let command = orchestrator_core::PhaseCommandDefinition {
            program: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf '%s\n' '{\"kind\":\"phase_decision\",\"phase_id\":\"testing\",\"verdict\":\"advance\",\"confidence\":0.91,\"risk\":\"low\",\"reason\":\"checks passed\",\"evidence\":[]}'".to_string(),
            ],
            env: BTreeMap::new(),
            cwd_mode: orchestrator_core::CommandCwdMode::ProjectRoot,
            cwd_path: None,
            timeout_secs: Some(5),
            success_exit_codes: vec![0],
            parse_json_output: false,
            expected_result_kind: None,
            expected_schema: None,
        };

        let result = run_workflow_phase_with_command(&context, &runtime_config, &command)
            .await
            .expect("command phase should run");

        let decision = result.phase_decision.expect("phase decision should parse");
        assert_eq!(decision.phase_id, "testing");
        assert_eq!(decision.reason, "checks passed");
        assert!(matches!(
            decision.verdict,
            orchestrator_core::PhaseDecisionVerdict::Advance
        ));
    }

    #[tokio::test]
    async fn command_phase_extracts_phase_decision_from_stderr_on_failure() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let project_root = temp_dir.path().join("project");
        fs::create_dir_all(&project_root).expect("create project root");

        let mut runtime_config = orchestrator_core::AgentRuntimeConfig::default();
        runtime_config.tools_allowlist = vec!["sh".to_string()];

        let context = CommandExecutionContext {
            project_root: project_root.to_str().expect("project root str"),
            execution_cwd: project_root.to_str().expect("execution cwd str"),
            workflow_id: "wf-123",
            phase_id: "lint",
            workflow_ref: "default",
            subject_id: "TASK-1",
            subject_title: "Task",
            subject_description: "Task description",
            pipeline_vars: None,
        };
        let command = orchestrator_core::PhaseCommandDefinition {
            program: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf '%s\n' '{\"kind\":\"phase_decision\",\"phase_id\":\"lint\",\"verdict\":\"rework\",\"confidence\":0.72,\"risk\":\"medium\",\"reason\":\"formatting required\",\"evidence\":[]}' >&2; exit 3".to_string(),
            ],
            env: BTreeMap::new(),
            cwd_mode: orchestrator_core::CommandCwdMode::ProjectRoot,
            cwd_path: None,
            timeout_secs: Some(5),
            success_exit_codes: vec![0],
            parse_json_output: false,
            expected_result_kind: None,
            expected_schema: None,
        };

        let result = run_workflow_phase_with_command(&context, &runtime_config, &command)
            .await
            .expect("command phase should run");

        assert!(result.failure_summary.is_some());
        let decision = result.phase_decision.expect("phase decision should parse");
        assert_eq!(decision.phase_id, "lint");
        assert_eq!(decision.reason, "formatting required");
        assert!(matches!(
            decision.verdict,
            orchestrator_core::PhaseDecisionVerdict::Rework
        ));
    }

    #[test]
    fn parse_phase_decision_coerces_numeric_confidence_strings() {
        let decision = parse_phase_decision_from_payload(&serde_json::json!({
            "kind": "phase_decision",
            "phase_id": "triage",
            "verdict": "skip",
            "confidence": "0.95",
            "risk": "low",
            "reason": "already done",
            "evidence": [],
            "skip_reason": "already_done"
        }))
        .expect("phase decision should parse");

        assert!(matches!(
            decision.verdict,
            orchestrator_core::PhaseDecisionVerdict::Skip
        ));
        assert!((decision.confidence - 0.95).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_phase_decision_payload_normalizes_nested_confidence_strings() {
        let payload = parse_result_payload_from_payload_for_kind(
            &serde_json::json!({
                "kind": "implementation_result",
                "summary": "done",
                "phase_decision": {
                    "kind": "phase_decision",
                    "phase_id": "implementation",
                    "verdict": "advance",
                    "confidence": "0.91",
                    "risk": "low",
                    "reason": "implemented",
                    "evidence": []
                }
            }),
            "implementation_result",
        )
        .expect("result payload should parse");

        assert_eq!(
            payload["phase_decision"]["confidence"],
            serde_json::json!(0.91)
        );
    }

    #[test]
    fn command_template_vars_prefer_dispatch_input_and_alias_schedule_subjects() {
        let _dispatch_input =
            EnvVarGuard::set("AO_DISPATCH_INPUT", Some("{\"source\":\"dispatch\"}"));
        let _schedule_input =
            EnvVarGuard::set("AO_SCHEDULE_INPUT", Some("{\"source\":\"schedule\"}"));

        let schedule_context = CommandExecutionContext {
            project_root: "/tmp/project",
            execution_cwd: "/tmp/project/task-root",
            workflow_id: "wf-123",
            phase_id: "implementation",
            workflow_ref: "ops",
            subject_id: "schedule:nightly",
            subject_title: "Nightly",
            subject_description: "Nightly schedule",
            pipeline_vars: None,
        };
        let schedule_vars = build_command_template_vars(&schedule_context);
        assert_eq!(
            schedule_vars.get("dispatch_input").map(String::as_str),
            Some("{\"source\":\"dispatch\"}")
        );
        assert_eq!(
            schedule_vars.get("schedule_input").map(String::as_str),
            Some("{\"source\":\"dispatch\"}")
        );

        let custom_context = CommandExecutionContext {
            subject_id: "custom:ops",
            ..schedule_context
        };
        let custom_vars = build_command_template_vars(&custom_context);
        assert_eq!(
            custom_vars.get("dispatch_input").map(String::as_str),
            Some("{\"source\":\"dispatch\"}")
        );
        assert!(!custom_vars.contains_key("schedule_input"));
    }
}

fn manual_phase_marker_path(project_root: &str) -> PathBuf {
    Path::new(project_root)
        .join(".ao")
        .join("state")
        .join("manual-phase-markers.v1.json")
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
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("manual-phase-markers"),
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

fn to_mode_string(mode: &orchestrator_core::PhaseExecutionMode) -> String {
    match mode {
        orchestrator_core::PhaseExecutionMode::Agent => "agent".to_string(),
        orchestrator_core::PhaseExecutionMode::Command => "command".to_string(),
        orchestrator_core::PhaseExecutionMode::Manual => "manual".to_string(),
    }
}

pub async fn run_workflow_phase(
    project_root: &str,
    execution_cwd: &str,
    workflow_id: &str,
    workflow_ref: &str,
    subject_id: &str,
    subject_title: &str,
    subject_description: &str,
    task_complexity: Option<orchestrator_core::Complexity>,
    phase_id: &str,
    phase_attempt: u32,
    overrides: Option<&PhaseExecuteOverrides>,
    pipeline_vars: Option<&std::collections::HashMap<String, String>>,
) -> Result<PhaseExecutionRunResult> {
    let workflow_config = load_workflow_config_strict(project_root)?;
    let runtime_loaded = load_agent_runtime_config_strict(project_root)?;
    orchestrator_core::validate_workflow_and_runtime_configs(
        &workflow_config.config,
        &runtime_loaded.config,
    )?;

    let mut merged_runtime = runtime_loaded.config.clone();
    for (id, profile) in &workflow_config.config.agent_profiles {
        merged_runtime.agents.insert(id.clone(), profile.clone());
    }
    if !workflow_config.config.tools_allowlist.is_empty() {
        let mut combined: std::collections::HashSet<String> =
            merged_runtime.tools_allowlist.iter().cloned().collect();
        combined.extend(workflow_config.config.tools_allowlist.iter().cloned());
        merged_runtime.tools_allowlist = combined.into_iter().collect();
        merged_runtime.tools_allowlist.sort();
    }

    let definition = workflow_config
        .config
        .phase_definitions
        .get(phase_id)
        .or_else(|| merged_runtime.phase_execution(phase_id))
        .ok_or_else(|| {
            anyhow!(
                "phase '{}' is missing from both workflow config and agent runtime config",
                phase_id
            )
        })?;
    let agent_id = definition
        .agent_id
        .as_deref()
        .or_else(|| merged_runtime.phase_agent_id(phase_id))
        .map(ToOwned::to_owned);
    let agent_profile_hash = agent_id
        .as_deref()
        .and_then(|id| merged_runtime.agent_profile(id))
        .map(hash_serializable);

    let mut metadata = PhaseExecutionMetadata {
        phase_id: phase_id.to_string(),
        phase_mode: to_mode_string(&definition.mode),
        phase_definition_hash: hash_serializable(definition),
        agent_runtime_config_hash: runtime_loaded.metadata.hash.clone(),
        agent_runtime_schema: runtime_loaded.metadata.schema.clone(),
        agent_runtime_version: runtime_loaded.metadata.version,
        agent_runtime_source: runtime_loaded.metadata.source.as_str().to_string(),
        agent_id: agent_id.clone(),
        agent_profile_hash,
        selected_tool: None,
        selected_model: None,
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
            let cli_tool_override = overrides.and_then(|o| o.tool.as_deref());
            let cli_model_override = overrides.and_then(|o| o.model.as_deref());

            let runtime_settings = Some(WorkflowPhaseRuntimeSettings {
                tool: cli_tool_override
                    .or_else(|| merged_runtime.phase_tool_override(phase_id))
                    .map(ToOwned::to_owned),
                model: cli_model_override
                    .or_else(|| merged_runtime.phase_model_override(phase_id))
                    .map(ToOwned::to_owned),
                fallback_models: merged_runtime.phase_fallback_models(phase_id),
                reasoning_effort: merged_runtime
                    .phase_reasoning_effort(phase_id)
                    .map(ToOwned::to_owned),
                web_search: merged_runtime.phase_web_search(phase_id),
                network_access: merged_runtime.phase_network_access(phase_id),
                timeout_secs: merged_runtime.phase_timeout_secs(phase_id),
                max_attempts: merged_runtime.phase_max_attempts(phase_id),
                extra_args: merged_runtime.phase_extra_args(phase_id),
                codex_config_overrides: merged_runtime.phase_codex_config_overrides(phase_id),
                max_continuations: merged_runtime.phase_max_continuations(phase_id),
            });

            let routing_complexity = routing_complexity(task_complexity);
            let exec_caps = merged_runtime.phase_capabilities(phase_id);
            let execution_targets = PhaseTargetPlanner::build_phase_execution_targets(
                phase_id,
                cli_model_override
                    .or_else(|| merged_runtime.phase_model_override(phase_id))
                    .or_else(|| {
                        runtime_settings
                            .as_ref()
                            .and_then(|settings| settings.model.as_deref())
                    }),
                cli_tool_override
                    .or_else(|| merged_runtime.phase_tool_override(phase_id))
                    .or_else(|| {
                        runtime_settings
                            .as_ref()
                            .and_then(|settings| settings.tool.as_deref())
                    }),
                merged_runtime.phase_fallback_models(phase_id).as_slice(),
                routing_complexity,
                Some(project_root),
                &exec_caps,
            );
            if let Some((tool, model)) = execution_targets.first() {
                metadata.selected_tool = Some(tool.clone());
                metadata.selected_model = Some(model.clone());
            }

            let outcome = run_workflow_phase_with_agent(
                project_root,
                execution_cwd,
                workflow_id,
                subject_id,
                subject_title,
                subject_description,
                task_complexity,
                phase_id,
                runtime_settings.as_ref(),
                overrides,
                pipeline_vars,
            )
            .await?;

            if definition.output_contract.is_some() || definition.output_json_schema.is_some() {
                match &outcome {
                    PhaseExecutionOutcome::Completed {
                        commit_message,
                        result_payload,
                        ..
                    } => {
                        if definition
                            .output_contract
                            .as_ref()
                            .is_some_and(|contract| contract.requires_field("commit_message"))
                            && commit_message
                                .as_deref()
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .is_none()
                        {
                            signals.push(PhaseExecutionSignal {
                                event_type: "workflow-phase-contract-violation".to_string(),
                                payload: serde_json::json!({
                                    "workflow_id": workflow_id,
                                    "phase_id": phase_id,
                                    "reason": "commit_message required but missing",
                                }),
                            });
                            return Err(anyhow!(
                                "phase '{}' contract violation: commit_message is required",
                                phase_id
                            ));
                        }

                        let phase_schema = phase_response_json_schema_for(project_root, phase_id)
                            .or_else(|| phase_output_json_schema_for(project_root, phase_id));
                        if let Some(schema) = phase_schema.as_ref() {
                            let payload = result_payload.clone().unwrap_or_else(|| {
                                serde_json::json!({
                                    "kind": definition
                                        .output_contract
                                        .as_ref()
                                        .map(|contract| contract.kind.as_str())
                                        .unwrap_or("phase_result"),
                                    "commit_message": commit_message,
                                })
                            });
                            if let Err(error) = validate_basic_json_schema(&payload, schema) {
                                signals.push(PhaseExecutionSignal {
                                    event_type: "workflow-phase-contract-violation".to_string(),
                                    payload: serde_json::json!({
                                        "workflow_id": workflow_id,
                                        "phase_id": phase_id,
                                        "reason": error.to_string(),
                                    }),
                                });
                                return Err(anyhow!(
                                    "phase '{}' contract violation: {}",
                                    phase_id,
                                    error
                                ));
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
                    PhaseExecutionOutcome::NeedsResearch { .. } => {}
                    PhaseExecutionOutcome::ManualPending { .. } => {}
                }
            }

            Ok(PhaseExecutionRunResult {
                outcome,
                metadata,
                signals,
            })
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
            };

            let command_result =
                run_workflow_phase_with_command(&command_context, &merged_runtime, command).await?;
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
                        phase_id,
                        command_result.program.as_str(),
                        &command_result.args,
                        command_result.exit_code,
                        Some(failure_summary.as_str()),
                    )
                });
                let result_payload = build_command_result_payload(
                    phase_id,
                    definition
                        .output_contract
                        .as_ref()
                        .map(|contract| contract.kind.as_str()),
                    &command_result,
                    &decision,
                );

                let outcome = PhaseExecutionOutcome::Completed {
                    commit_message: None,
                    phase_decision: Some(decision),
                    result_payload: Some(result_payload),
                };

                persist_phase_output(project_root, workflow_id, phase_id, &outcome)?;

                return Ok(PhaseExecutionRunResult {
                    outcome,
                    metadata,
                    signals,
                });
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

            let decision = command_result.phase_decision.clone().unwrap_or_else(|| {
                build_command_phase_decision(
                    phase_id,
                    command_result.program.as_str(),
                    &command_result.args,
                    command_result.exit_code,
                    None,
                )
            });
            let result_payload = build_command_result_payload(
                phase_id,
                definition
                    .output_contract
                    .as_ref()
                    .map(|contract| contract.kind.as_str()),
                &command_result,
                &decision,
            );

            Ok(PhaseExecutionRunResult {
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

            Ok(PhaseExecutionRunResult {
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
