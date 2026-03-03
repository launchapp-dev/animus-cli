use crate::shared::{
    build_runtime_contract, collect_json_payload_lines, connect_runner, event_matches_run,
    runner_config_dir, write_json_line,
};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use protocol::{AgentRunEvent, AgentRunRequest, ModelId, RunId, PROTOCOL_VERSION};

use super::phase_git::commit_implementation_changes;
use crate::services::runtime::runtime_daemon::daemon_scheduler::phase_failover::PhaseFailureClassifier;
use crate::services::runtime::runtime_daemon::daemon_scheduler::phase_targets::PhaseTargetPlanner;
use crate::services::runtime::runtime_daemon::daemon_scheduler::runtime_support::{
    inject_cli_launch_overrides, phase_runner_attempts, phase_timeout_secs,
    WorkflowPhaseRuntimeSettings,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct PhaseExecuteOverrides {
    pub(crate) tool: Option<String>,
    pub(crate) model: Option<String>,
}

const WORKFLOW_PHASE_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/runtime/workflow_phase.prompt"
));

fn load_agent_runtime_config(project_root: &str) -> orchestrator_core::AgentRuntimeConfig {
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
pub(crate) struct PhaseExecutionMetadata {
    pub(crate) phase_id: String,
    pub(crate) phase_mode: String,
    pub(crate) phase_definition_hash: String,
    pub(crate) agent_runtime_config_hash: String,
    pub(crate) agent_runtime_schema: String,
    pub(crate) agent_runtime_version: u32,
    pub(crate) agent_runtime_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_profile_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PhaseExecutionSignal {
    pub(crate) event_type: String,
    pub(crate) payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PhaseExecutionRunResult {
    pub(crate) outcome: PhaseExecutionOutcome,
    pub(crate) metadata: PhaseExecutionMetadata,
    #[serde(default)]
    pub(crate) signals: Vec<PhaseExecutionSignal>,
}

fn phase_agent_id_for(project_root: &str, phase_id: &str) -> Option<String> {
    load_agent_runtime_config(project_root)
        .phase_agent_id(phase_id)
        .map(ToOwned::to_owned)
}

fn phase_system_prompt_for(project_root: &str, phase_id: &str) -> Option<String> {
    load_agent_runtime_config(project_root)
        .phase_system_prompt(phase_id)
        .map(ToOwned::to_owned)
}

fn phase_tool_override_for(project_root: &str, phase_id: &str) -> Option<String> {
    load_agent_runtime_config(project_root)
        .phase_tool_override(phase_id)
        .map(ToOwned::to_owned)
}

fn phase_model_override_for(project_root: &str, phase_id: &str) -> Option<String> {
    load_agent_runtime_config(project_root)
        .phase_model_override(phase_id)
        .map(ToOwned::to_owned)
}

fn phase_fallback_models_for(project_root: &str, phase_id: &str) -> Vec<String> {
    load_agent_runtime_config(project_root).phase_fallback_models(phase_id)
}

fn load_phase_capabilities(project_root: &str, phase_id: &str) -> protocol::PhaseCapabilities {
    load_agent_runtime_config(project_root).phase_capabilities(phase_id)
}

fn phase_output_contract_for(
    project_root: &str,
    phase_id: &str,
) -> Option<orchestrator_core::PhaseOutputContract> {
    load_agent_runtime_config(project_root)
        .phase_output_contract(phase_id)
        .cloned()
}

fn phase_output_json_schema_for(project_root: &str, phase_id: &str) -> Option<Value> {
    load_agent_runtime_config(project_root)
        .phase_output_json_schema(phase_id)
        .cloned()
}

fn inject_read_only_flag(
    runtime_contract: &mut Value,
    config: &orchestrator_core::AgentRuntimeConfig,
) {
    let cli_name = runtime_contract
        .pointer("/cli/name")
        .and_then(Value::as_str)
        .unwrap_or("");

    if let Some(flag) = orchestrator_core::cli_tool_read_only_flag(cli_name, config) {
        if let Some(args) = runtime_contract
            .pointer_mut("/cli/launch/args")
            .and_then(Value::as_array_mut)
        {
            let prompt_idx = args.len().saturating_sub(1);
            args.insert(prompt_idx, Value::String(flag));
        }
    }
}

fn inject_response_schema_into_launch_args(
    runtime_contract: &mut Value,
    schema: &Value,
    config: &orchestrator_core::AgentRuntimeConfig,
) {
    let cli_name = runtime_contract
        .pointer("/cli/name")
        .and_then(Value::as_str)
        .unwrap_or("");

    if let Some(flag) = orchestrator_core::cli_tool_response_schema_flag(cli_name, config) {
        if let Some(args) = runtime_contract
            .pointer_mut("/cli/launch/args")
            .and_then(Value::as_array_mut)
        {
            let prompt_idx = args.len().saturating_sub(1);
            let schema_str = serde_json::to_string(schema).unwrap_or_default();
            args.insert(prompt_idx, Value::String(flag));
            args.insert(prompt_idx + 1, Value::String(schema_str));
        }
    }
}

fn inject_default_stdio_mcp(runtime_contract: &mut Value, project_root: &str) {
    if runtime_contract
        .pointer("/mcp/stdio/command")
        .and_then(Value::as_str)
        .is_some_and(|v| !v.trim().is_empty())
    {
        return;
    }

    if std::env::var("AO_MCP_TRANSPORT")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .is_some_and(|v| v == "http")
    {
        return;
    }

    let supports_mcp = runtime_contract
        .pointer("/cli/capabilities/supports_mcp")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !supports_mcp {
        return;
    }

    let command = std::env::var("AO_MCP_STDIO_COMMAND")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        });
    let Some(command) = command else {
        return;
    };

    let args = std::env::var("AO_MCP_STDIO_ARGS_JSON")
        .ok()
        .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
        .unwrap_or_else(|| {
            vec![
                "--project-root".to_string(),
                project_root.to_string(),
                "mcp".to_string(),
                "serve".to_string(),
            ]
        });

    if let Some(mcp) = runtime_contract
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
    {
        mcp.insert(
            "stdio".to_string(),
            serde_json::json!({ "command": command, "args": args }),
        );
        let has_agent_id = mcp
            .get("agent_id")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty());
        if !has_agent_id {
            mcp.insert("agent_id".to_string(), serde_json::json!("ao"));
        }
    }
}

fn inject_agent_tool_policy(runtime_contract: &mut Value, project_root: &str, phase_id: &str) {
    let config = load_agent_runtime_config(project_root);
    let Some(profile) = config.phase_agent_profile(phase_id) else {
        return;
    };
    let policy = &profile.tool_policy;
    if policy.allow.is_empty() && policy.deny.is_empty() {
        return;
    }
    if let Some(mcp) = runtime_contract
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
    {
        mcp.insert(
            "tool_policy".to_string(),
            serde_json::json!({
                "allow": policy.allow,
                "deny": policy.deny,
            }),
        );
    }
}

fn inject_project_mcp_servers(
    runtime_contract: &mut Value,
    project_root: &str,
    phase_id: &str,
) {
    let project_config = match protocol::Config::load_or_default(project_root) {
        Ok(c) => c,
        Err(_) => return,
    };
    if project_config.mcp_servers.is_empty() {
        return;
    }
    let agent_id = load_agent_runtime_config(project_root)
        .phase_agent_id(phase_id)
        .map(ToOwned::to_owned);
    let mut servers = serde_json::Map::new();
    for (name, entry) in &project_config.mcp_servers {
        let assigned = entry.assign_to.is_empty()
            || agent_id
                .as_deref()
                .is_some_and(|id| entry.assign_to.iter().any(|a| a.eq_ignore_ascii_case(id)));
        if !assigned {
            continue;
        }
        servers.insert(
            name.clone(),
            serde_json::json!({
                "command": entry.command,
                "args": entry.args,
                "env": entry.env,
            }),
        );
    }
    if servers.is_empty() {
        return;
    }
    if let Some(mcp) = runtime_contract
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
    {
        mcp.insert(
            "additional_servers".to_string(),
            Value::Object(servers),
        );
    }
}

fn phase_decision_contract_for(
    project_root: &str,
    phase_id: &str,
) -> Option<orchestrator_core::PhaseDecisionContract> {
    load_agent_runtime_config(project_root)
        .phase_decision_contract(phase_id)
        .cloned()
}

pub(crate) fn phase_directive_for(project_root: &str, phase_id: &str) -> String {
    let config = load_agent_runtime_config(project_root);
    config
        .phase_directive(phase_id)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            "Execute the current workflow phase with production-quality output.".to_string()
        })
}

pub(crate) fn build_phase_prompt(
    project_root: &str,
    workflow_id: &str,
    task_id: &str,
    task_title: &str,
    task_description: &str,
    phase_id: &str,
) -> String {
    let caps = load_phase_capabilities(project_root, phase_id);
    let phase_action_rule = if caps.writes_files {
        "Requirements:\n- Make concrete file changes in this repository."
    } else {
        "Requirements:\n- This is a READ-ONLY phase. Do NOT create, edit, or write any files. Do NOT run commands that modify the repository.\n- Read and analyze the codebase to assess the task. Your only output should be your assessment and phase decision."
    };
    let phase_contract = phase_output_contract_for(project_root, phase_id);
    let require_commit_message = phase_requires_commit_message_with_config(project_root, phase_id);
    let product_change_rule = if caps.enforce_product_changes {
        "- For this phase, changes must include product source/config/test files outside `.ao/` unless the task is explicitly documentation-only."
    } else {
        ""
    };
    let implementation_commit_rule = if require_commit_message {
        format!(
            "- Before finishing implementation, emit one JSON line exactly like: {{\"kind\":\"{}\",\"commit_message\":\"<clear commit subject>\"}}.",
            phase_contract
                .as_ref()
                .map(|contract| contract.kind.as_str())
                .unwrap_or("implementation_result")
        )
    } else {
        String::new()
    };
    let phase_directive = phase_directive_for(project_root, phase_id);
    let phase_safety_rules = phase_safety_rules(&caps);
    let phase_decision_rule = if phase_decision_contract_for(project_root, phase_id).is_some() {
        format!(
            "- Before finishing, emit one JSON line with your phase assessment:\n  {{\"kind\":\"phase_decision\",\"phase_id\":\"{phase_id}\",\"verdict\":\"advance|rework|fail\",\"confidence\":0.0-1.0,\"risk\":\"low|medium|high\",\"reason\":\"...\",\"evidence\":[{{\"kind\":\"...\",\"description\":\"...\"}}]}}\n- Set verdict to \"advance\" if work is complete and correct.\n- Set verdict to \"rework\" if issues remain that need another pass.\n- Set verdict to \"fail\" only if problems are unrecoverable.\n- Be honest about confidence. 0.5 = uncertain, 0.8+ = confident."
        )
    } else {
        String::new()
    };

    let phase_order = pipeline_phase_order_for_workflow(project_root, workflow_id);
    let prior_outputs = load_prior_phase_outputs(project_root, workflow_id, phase_id, &phase_order);
    let prior_phase_context = format_prior_phase_outputs(&prior_outputs);

    let phase_prompt = WORKFLOW_PHASE_PROMPT_TEMPLATE
        .replace("__PROJECT_ROOT__", project_root)
        .replace("__WORKFLOW_ID__", workflow_id)
        .replace("__TASK_ID__", task_id)
        .replace("__TASK_TITLE__", task_title)
        .replace("__TASK_DESCRIPTION__", task_description)
        .replace("__PHASE_ID__", phase_id)
        .replace("__PHASE_DIRECTIVE__", phase_directive.trim())
        .replace("__PHASE_ACTION_RULE__", phase_action_rule)
        .replace("__PRODUCT_CHANGE_RULE__", product_change_rule)
        .replace("__PHASE_SAFETY_RULES__", phase_safety_rules)
        .replace("__PHASE_DECISION_RULE__", &phase_decision_rule)
        .replace(
            "__IMPLEMENTATION_COMMIT_RULE__",
            implementation_commit_rule.as_str(),
        )
        .replace("__PRIOR_PHASE_OUTPUTS__", &prior_phase_context);

    if let Some(system_prompt) = phase_system_prompt_for(project_root, phase_id) {
        if !system_prompt.trim().is_empty() {
            return format!("{system_prompt}\n\n{phase_prompt}");
        }
    }

    phase_prompt
}

fn phase_safety_rules(caps: &protocol::PhaseCapabilities) -> &'static str {
    if caps.is_research {
        return "- For research phases, treat greenfield repositories as valid: missing app source files is not a blocker by itself.\n- Do targeted discovery only: inspect first-party code (`src/`, `apps/`, `db/`, `tests/`) and active `.ao` task/requirement docs; avoid broad recursive listings.\n- Do not scan dependency or checkpoint trees unless explicitly required: skip `node_modules/`, `.git/`, `.ao/workflow-state/checkpoints/`, and `.ao/runs/`.\n- If code context is limited, produce concrete assumptions, risks, and a build-ready plan in repository artifacts instead of stopping.\n- Emit `research_required` only for true external blockers that cannot be reasonably unblocked with explicit assumptions.";
    }

    ""
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum PhaseExecutionOutcome {
    Completed {
        commit_message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase_decision: Option<orchestrator_core::PhaseDecision>,
    },
    NeedsResearch {
        reason: String,
    },
    ManualPending {
        instructions: String,
        approval_note_required: bool,
    },
}

pub(crate) fn phase_requires_commit_message(phase_id: &str) -> bool {
    protocol::PhaseCapabilities::defaults_for_phase(phase_id).requires_commit
}

fn phase_requires_commit_message_with_config(project_root: &str, phase_id: &str) -> bool {
    phase_output_contract_for(project_root, phase_id)
        .map(|contract| contract.requires_field("commit_message"))
        .unwrap_or_else(|| phase_requires_commit_message(phase_id))
}

fn phase_result_kind_for(project_root: &str, phase_id: &str) -> String {
    phase_output_contract_for(project_root, phase_id)
        .map(|contract| contract.kind)
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or_else(|| "implementation_result".to_string())
}

pub(crate) fn parse_research_reason_from_payload(payload: &Value) -> Option<String> {
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

pub(crate) fn parse_research_reason_from_text(text: &str) -> Option<String> {
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
                if let Ok(decision) =
                    serde_json::from_value::<orchestrator_core::PhaseDecision>(payload.clone())
                {
                    return Some(decision);
                }
            }

            for key in ["proposal", "data", "payload", "result", "output", "item"] {
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

pub(crate) fn parse_phase_decision_from_text(
    text: &str,
) -> Option<orchestrator_core::PhaseDecision> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(decision) = parse_phase_decision_from_payload(&payload) {
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
pub(crate) fn parse_commit_message_from_payload(payload: &Value) -> Option<String> {
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

#[cfg(test)]
pub(crate) fn parse_commit_message_from_text(text: &str) -> Option<String> {
    parse_commit_message_from_text_for_kind(text, "implementation_result")
}

pub(crate) fn fallback_implementation_commit_message(task_id: &str, task_title: &str) -> String {
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

pub(crate) async fn run_workflow_phase_attempt(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    request: &AgentRunRequest,
    parse_research_signal: bool,
) -> Result<PhaseExecutionOutcome> {
    let parse_commit_message = phase_requires_commit_message_with_config(project_root, phase_id);
    let expected_result_kind = phase_result_kind_for(project_root, phase_id);
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
    let parse_phase_decision = phase_decision_contract_for(project_root, phase_id).is_some();
    let mut provider_exhaustion_reason: Option<String> = None;
    let mut diagnostics = VecDeque::new();
    let stream_level = std::env::var("AO_STREAM_PHASE_OUTPUT").unwrap_or_default();
    let stream_normal = matches!(stream_level.as_str(), "1" | "normal");
    let stream_verbose = stream_level == "verbose";
    let stream_to_stderr = stream_normal || stream_verbose;
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
                if stream_to_stderr {
                    use std::io::Write as _;
                    if stream_verbose {
                        let _ = write!(std::io::stderr(), "{}", text);
                    } else {
                        let trimmed = text.trim_start();
                        let is_json = trimmed.starts_with('{')
                            && serde_json::from_str::<serde_json::Value>(trimmed).is_ok();
                        if !is_json {
                            let _ = write!(std::io::stderr(), "{}", text);
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
                if stream_verbose {
                    use std::io::Write as _;
                    let _ = write!(std::io::stderr(), "[2m{}[0m", content);
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
                });
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

pub(crate) async fn run_workflow_phase_with_agent(
    project_root: &str,
    execution_cwd: &str,
    workflow_id: &str,
    task_id: &str,
    task_title: &str,
    task_description: &str,
    task_complexity: Option<orchestrator_core::Complexity>,
    phase_id: &str,
    phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>,
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
        settings_model
            .or(agent_model_override.as_deref()),
        settings_tool
            .or(agent_tool_override.as_deref()),
        configured_fallback_models.as_slice(),
        routing_complexity,
        Some(project_root),
        &caps,
    );
    let parse_research_signal = !caps.is_research;
    let prompt = build_phase_prompt(
        project_root,
        workflow_id,
        task_id,
        task_title,
        task_description,
        phase_id,
    );
    let max_attempts = phase_runtime_settings
        .and_then(|settings| settings.max_attempts)
        .unwrap_or_else(phase_runner_attempts);
    let mut fallover_errors: Vec<String> = Vec::new();

    for (target_index, (target_tool_id, target_model_id)) in execution_targets.iter().enumerate() {
        let mut context = serde_json::json!({
            "tool": target_tool_id,
            "prompt": prompt.clone(),
            "cwd": execution_cwd,
            "project_root": project_root,
            "workflow_id": workflow_id,
            "task_id": task_id,
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
        if let Some(mut runtime_contract) = build_runtime_contract(
            context
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("codex"),
            target_model_id,
            &prompt,
        ) {
            if let Some(contract) = phase_contract.as_ref() {
                let mut policy = serde_json::json!({
                    "require_commit_message": contract.requires_field("commit_message"),
                    "required_result_kind": contract.kind.as_str(),
                    "required_result_fields": contract.required_fields.clone(),
                });
                if let Some(schema) = phase_output_schema.clone() {
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
            if let Some(schema) = phase_output_schema.as_ref() {
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
            context
                .as_object_mut()
                .expect("json object")
                .insert("runtime_contract".to_string(), runtime_contract);
        }

        let run_id = RunId(format!(
            "wf-{workflow_id}-{}-{target_index}-{}",
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
                                    fallback_implementation_commit_message(task_id, task_title)
                                });
                            commit_implementation_changes(execution_cwd, &resolved_commit_message)?;
                            *commit_message = Some(resolved_commit_message);
                        }
                    }
                    let verdict = match &outcome {
                        PhaseExecutionOutcome::Completed { phase_decision, .. } => {
                            phase_decision
                                .as_ref()
                                .map(|d| d.verdict)
                                .unwrap_or(orchestrator_core::PhaseDecisionVerdict::Advance)
                        }
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
                Err(error) => {
                    let message = error.to_string();
                    let should_retry = attempt < max_attempts
                        && PhaseFailureClassifier::is_transient_runner_error_message(&message);
                    if should_retry {
                        sleep(backoff).await;
                        backoff = std::cmp::min(backoff.saturating_mul(2), Duration::from_secs(3));
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

fn resolve_command_cwd(
    project_root: &str,
    task_root: &str,
    command: &orchestrator_core::PhaseCommandDefinition,
) -> Result<String> {
    match command.cwd_mode {
        orchestrator_core::CommandCwdMode::ProjectRoot => Ok(project_root.to_string()),
        orchestrator_core::CommandCwdMode::TaskRoot => Ok(task_root.to_string()),
        orchestrator_core::CommandCwdMode::Path => {
            let raw = command
                .cwd_path
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("command.cwd_path is required when cwd_mode='path'"))?;
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
            Ok(Path::new(project_root).join(relative).display().to_string())
        }
    }
}

#[derive(Debug, Clone)]
struct CommandExecutionResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration_ms: u64,
    parsed_payload: Option<Value>,
}

async fn run_workflow_phase_with_command(
    project_root: &str,
    execution_cwd: &str,
    phase_id: &str,
    runtime_config: &orchestrator_core::AgentRuntimeConfig,
    command: &orchestrator_core::PhaseCommandDefinition,
) -> Result<CommandExecutionResult> {
    if !is_program_allowlisted(&command.program, &runtime_config.tools_allowlist) {
        return Err(anyhow!(
            "phase '{}' command '{}' is not in tools_allowlist",
            phase_id,
            command.program
        ));
    }

    let cwd = resolve_command_cwd(project_root, execution_cwd, command)?;
    let started = std::time::Instant::now();

    let mut process = TokioCommand::new(&command.program);
    process
        .args(&command.args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in &command.env {
        process.env(key, value);
    }

    let output = if let Some(timeout_secs) = command.timeout_secs {
        timeout(Duration::from_secs(timeout_secs), process.output())
            .await
            .with_context(|| {
                format!(
                    "phase '{}' command '{}' timed out after {} seconds",
                    phase_id, command.program, timeout_secs
                )
            })??
    } else {
        process.output().await?
    };

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

    if !command.success_exit_codes.contains(&exit_code) {
        return Err(anyhow!(
            "phase '{}' command exited with code {} (expected one of {:?}); stderr: {}",
            phase_id,
            exit_code,
            command.success_exit_codes,
            stderr.trim()
        ));
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
        stdout,
        stderr,
        duration_ms,
        parsed_payload,
    })
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

pub(crate) async fn run_workflow_phase(
    project_root: &str,
    execution_cwd: &str,
    workflow_id: &str,
    task_id: &str,
    task_title: &str,
    task_description: &str,
    task_complexity: Option<orchestrator_core::Complexity>,
    phase_id: &str,
    phase_attempt: u32,
    overrides: Option<&PhaseExecuteOverrides>,
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
            "task_id": task_id,
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
                codex_config_overrides: merged_runtime
                    .phase_codex_config_overrides(phase_id),
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
                merged_runtime
                    .phase_fallback_models(phase_id)
                    .as_slice(),
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
                task_id,
                task_title,
                task_description,
                task_complexity,
                phase_id,
                runtime_settings.as_ref(),
            )
            .await?;

            if definition.output_contract.is_some() || definition.output_json_schema.is_some() {
                match &outcome {
                    PhaseExecutionOutcome::Completed { commit_message, .. } => {
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

                        if let Some(schema) = definition.output_json_schema.as_ref() {
                            let payload = serde_json::json!({
                                "kind": definition
                                    .output_contract
                                    .as_ref()
                                    .map(|contract| contract.kind.as_str())
                                    .unwrap_or("phase_result"),
                                "commit_message": commit_message,
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

            let command_result = run_workflow_phase_with_command(
                project_root,
                execution_cwd,
                phase_id,
                &merged_runtime,
                command,
            )
            .await?;
            signals.push(PhaseExecutionSignal {
                event_type: "workflow-phase-command-executed".to_string(),
                payload: serde_json::json!({
                    "workflow_id": workflow_id,
                    "task_id": task_id,
                    "phase_id": phase_id,
                    "program": command.program,
                    "args": command.args,
                    "exit_code": command_result.exit_code,
                    "duration_ms": command_result.duration_ms,
                    "stdout": command_result.stdout,
                    "stderr": command_result.stderr,
                    "parsed_payload": command_result.parsed_payload,
                }),
            });

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

            Ok(PhaseExecutionRunResult {
                outcome: PhaseExecutionOutcome::Completed {
                    commit_message: None,
                    phase_decision: None,
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
                        "task_id": task_id,
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

const MAX_PRIOR_CONTEXT_CHARS: usize = 4000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PersistedPhaseOutput {
    pub(crate) phase_id: String,
    pub(crate) completed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) commit_message: Option<String>,
}

pub(crate) fn phase_output_dir(project_root: &str, workflow_id: &str) -> PathBuf {
    Path::new(project_root)
        .join(".ao")
        .join("state")
        .join("workflows")
        .join(workflow_id)
        .join("phase-outputs")
}

pub(crate) fn persist_phase_output(
    project_root: &str,
    workflow_id: &str,
    phase_id: &str,
    outcome: &PhaseExecutionOutcome,
) -> Result<()> {
    let dir = phase_output_dir(project_root, workflow_id);
    std::fs::create_dir_all(&dir)?;

    let (verdict, confidence, reason, commit_message) = match outcome {
        PhaseExecutionOutcome::Completed {
            commit_message,
            phase_decision,
        } => {
            let (v, c, r) = match phase_decision {
                Some(decision) => (
                    Some(format!("{:?}", decision.verdict).to_ascii_lowercase()),
                    Some(decision.confidence),
                    if decision.reason.is_empty() {
                        None
                    } else {
                        Some(decision.reason.clone())
                    },
                ),
                None => (Some("advance".to_string()), None, None),
            };
            (v, c, r, commit_message.clone())
        }
        PhaseExecutionOutcome::NeedsResearch { reason } => (
            Some("rework".to_string()),
            None,
            Some(format!("Needs research: {reason}")),
            None,
        ),
        PhaseExecutionOutcome::ManualPending { instructions, .. } => (
            Some("manual_pending".to_string()),
            None,
            Some(instructions.clone()),
            None,
        ),
    };

    let output = PersistedPhaseOutput {
        phase_id: phase_id.to_string(),
        completed_at: chrono::Utc::now().to_rfc3339(),
        verdict,
        confidence,
        reason,
        commit_message,
    };

    let payload = serde_json::to_string_pretty(&output)?;
    let file_path = dir.join(format!("{phase_id}.json"));
    let tmp_path = file_path.with_file_name(format!("{phase_id}.{}.tmp", Uuid::new_v4()));
    std::fs::write(&tmp_path, &payload)?;
    std::fs::rename(&tmp_path, &file_path)?;
    Ok(())
}

pub(crate) fn load_prior_phase_outputs(
    project_root: &str,
    workflow_id: &str,
    current_phase_id: &str,
    pipeline_phase_order: &[String],
) -> Vec<PersistedPhaseOutput> {
    let dir = phase_output_dir(project_root, workflow_id);
    if !dir.exists() {
        return Vec::new();
    }

    let mut outputs = Vec::new();
    for prior_phase_id in pipeline_phase_order {
        if prior_phase_id == current_phase_id {
            break;
        }
        let file_path = dir.join(format!("{prior_phase_id}.json"));
        if let Ok(contents) = std::fs::read_to_string(&file_path) {
            if let Ok(output) = serde_json::from_str::<PersistedPhaseOutput>(&contents) {
                outputs.push(output);
            }
        }
    }
    outputs
}

pub(crate) fn format_prior_phase_outputs(outputs: &[PersistedPhaseOutput]) -> String {
    if outputs.is_empty() {
        return String::new();
    }

    let mut sections: Vec<String> = Vec::new();
    for output in outputs {
        let mut section = format!("### {} (completed)", output.phase_id);
        if let Some(ref verdict) = output.verdict {
            section.push_str(&format!("\nVerdict: {verdict}"));
        }
        if let Some(confidence) = output.confidence {
            section.push_str(&format!("\nConfidence: {confidence:.1}"));
        }
        if let Some(ref reason) = output.reason {
            section.push_str(&format!("\nReasoning: {reason}"));
        }
        if let Some(ref cm) = output.commit_message {
            section.push_str(&format!("\nCommit: {cm}"));
        }
        sections.push(section);
    }

    let mut result = "## Prior Phase Results\n".to_string();
    result.push_str(&sections.join("\n\n"));

    if result.len() > MAX_PRIOR_CONTEXT_CHARS {
        let mut truncated = "## Prior Phase Results\n".to_string();
        let mut budget = MAX_PRIOR_CONTEXT_CHARS - truncated.len() - 30;
        for section in sections.iter().rev() {
            if section.len() <= budget {
                truncated.push_str(section);
                truncated.push_str("\n\n");
                budget = budget.saturating_sub(section.len() + 2);
            } else {
                truncated.insert_str(
                    "## Prior Phase Results\n".len(),
                    "(earlier phases truncated for brevity)\n\n",
                );
                break;
            }
        }
        return truncated.trim_end().to_string();
    }

    result
}

fn pipeline_phase_order_for_workflow(project_root: &str, workflow_id: &str) -> Vec<String> {
    let workflow_path = Path::new(project_root)
        .join(".ao")
        .join("workflow-state")
        .join(format!("{workflow_id}.json"));
    let contents = match std::fs::read_to_string(&workflow_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let workflow: orchestrator_core::OrchestratorWorkflow = match serde_json::from_str(&contents) {
        Ok(w) => w,
        Err(_) => return Vec::new(),
    };
    workflow
        .phases
        .iter()
        .map(|phase| phase.phase_id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_persist_and_load_phase_output() {
        let tmp = std::env::temp_dir().join(format!("ao-test-phase-output-{}", Uuid::new_v4()));
        let project_root = tmp.to_str().unwrap();
        let workflow_id = "wf-test-001";

        let outcome = PhaseExecutionOutcome::Completed {
            commit_message: Some("feat: add login flow".to_string()),
            phase_decision: Some(orchestrator_core::PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "research".to_string(),
                verdict: orchestrator_core::PhaseDecisionVerdict::Advance,
                confidence: 0.9,
                risk: orchestrator_core::WorkflowDecisionRisk::Low,
                reason: "Research complete, found relevant patterns".to_string(),
                evidence: vec![],
                guardrail_violations: vec![],
                commit_message: None,
                target_phase: None,
            }),
        };

        persist_phase_output(project_root, workflow_id, "research", &outcome).unwrap();

        let output_file = phase_output_dir(project_root, workflow_id).join("research.json");
        assert!(output_file.exists());

        let loaded: PersistedPhaseOutput =
            serde_json::from_str(&std::fs::read_to_string(&output_file).unwrap()).unwrap();
        assert_eq!(loaded.phase_id, "research");
        assert_eq!(loaded.verdict.as_deref(), Some("advance"));
        assert!((loaded.confidence.unwrap() - 0.9).abs() < f32::EPSILON);
        assert_eq!(
            loaded.reason.as_deref(),
            Some("Research complete, found relevant patterns")
        );
        assert_eq!(
            loaded.commit_message.as_deref(),
            Some("feat: add login flow")
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_load_prior_phase_outputs_ordering() {
        let tmp = std::env::temp_dir().join(format!(
            "ao-test-phase-output-order-{}",
            Uuid::new_v4()
        ));
        let project_root = tmp.to_str().unwrap();
        let workflow_id = "wf-test-002";

        let research_outcome = PhaseExecutionOutcome::Completed {
            commit_message: None,
            phase_decision: Some(orchestrator_core::PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "research".to_string(),
                verdict: orchestrator_core::PhaseDecisionVerdict::Advance,
                confidence: 0.8,
                risk: orchestrator_core::WorkflowDecisionRisk::Low,
                reason: "Research done".to_string(),
                evidence: vec![],
                guardrail_violations: vec![],
                commit_message: None,
                target_phase: None,
            }),
        };
        persist_phase_output(project_root, workflow_id, "research", &research_outcome).unwrap();

        let impl_outcome = PhaseExecutionOutcome::Completed {
            commit_message: Some("feat: implement feature".to_string()),
            phase_decision: Some(orchestrator_core::PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "implementation".to_string(),
                verdict: orchestrator_core::PhaseDecisionVerdict::Advance,
                confidence: 0.95,
                risk: orchestrator_core::WorkflowDecisionRisk::Low,
                reason: "Implementation complete".to_string(),
                evidence: vec![],
                guardrail_violations: vec![],
                commit_message: None,
                target_phase: None,
            }),
        };
        persist_phase_output(project_root, workflow_id, "implementation", &impl_outcome).unwrap();

        let pipeline_order = vec![
            "research".to_string(),
            "implementation".to_string(),
            "review".to_string(),
        ];

        let loaded = load_prior_phase_outputs(project_root, workflow_id, "review", &pipeline_order);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].phase_id, "research");
        assert_eq!(loaded[1].phase_id, "implementation");

        let loaded_impl =
            load_prior_phase_outputs(project_root, workflow_id, "implementation", &pipeline_order);
        assert_eq!(loaded_impl.len(), 1);
        assert_eq!(loaded_impl[0].phase_id, "research");

        let loaded_research =
            load_prior_phase_outputs(project_root, workflow_id, "research", &pipeline_order);
        assert_eq!(loaded_research.len(), 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_format_prior_phase_outputs_empty() {
        let result = format_prior_phase_outputs(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_prior_phase_outputs_renders_sections() {
        let outputs = vec![
            PersistedPhaseOutput {
                phase_id: "research".to_string(),
                completed_at: "2026-03-01T00:00:00Z".to_string(),
                verdict: Some("advance".to_string()),
                confidence: Some(0.9),
                reason: Some("Found patterns".to_string()),
                commit_message: None,
            },
            PersistedPhaseOutput {
                phase_id: "implementation".to_string(),
                completed_at: "2026-03-01T01:00:00Z".to_string(),
                verdict: Some("advance".to_string()),
                confidence: Some(0.95),
                reason: Some("Implemented".to_string()),
                commit_message: Some("feat: add feature".to_string()),
            },
        ];
        let result = format_prior_phase_outputs(&outputs);
        assert!(result.contains("## Prior Phase Results"));
        assert!(result.contains("### research (completed)"));
        assert!(result.contains("### implementation (completed)"));
        assert!(result.contains("Verdict: advance"));
        assert!(result.contains("Confidence: 0.9"));
        assert!(result.contains("Reasoning: Found patterns"));
        assert!(result.contains("Commit: feat: add feature"));
    }

    #[test]
    fn test_format_prior_phase_outputs_truncation() {
        let long_reason = "x".repeat(3000);
        let outputs = vec![
            PersistedPhaseOutput {
                phase_id: "early".to_string(),
                completed_at: "2026-03-01T00:00:00Z".to_string(),
                verdict: Some("advance".to_string()),
                confidence: None,
                reason: Some(long_reason),
                commit_message: None,
            },
            PersistedPhaseOutput {
                phase_id: "recent".to_string(),
                completed_at: "2026-03-01T01:00:00Z".to_string(),
                verdict: Some("advance".to_string()),
                confidence: Some(0.9),
                reason: Some("Recent work".to_string()),
                commit_message: None,
            },
        ];
        let result = format_prior_phase_outputs(&outputs);
        assert!(result.len() <= MAX_PRIOR_CONTEXT_CHARS);
        assert!(result.contains("### recent (completed)"));
    }

    #[test]
    fn test_persist_needs_research_outcome() {
        let tmp = std::env::temp_dir().join(format!(
            "ao-test-phase-output-research-{}",
            Uuid::new_v4()
        ));
        let project_root = tmp.to_str().unwrap();
        let workflow_id = "wf-test-003";

        let outcome = PhaseExecutionOutcome::NeedsResearch {
            reason: "Need API docs".to_string(),
        };
        persist_phase_output(project_root, workflow_id, "implementation", &outcome).unwrap();

        let output_file =
            phase_output_dir(project_root, workflow_id).join("implementation.json");
        let loaded: PersistedPhaseOutput =
            serde_json::from_str(&std::fs::read_to_string(&output_file).unwrap()).unwrap();
        assert_eq!(loaded.verdict.as_deref(), Some("rework"));
        assert!(loaded.reason.as_deref().unwrap().contains("Need API docs"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
