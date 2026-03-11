use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use orchestrator_core::{RequirementItem, RequirementStatus};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkflowPhaseRuntimeSettings {
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub web_search: Option<bool>,
    #[serde(default)]
    pub network_access: Option<bool>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_attempts: Option<usize>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub codex_config_overrides: Vec<String>,
    #[serde(default)]
    pub max_continuations: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkflowPipelineRuntimeRecord {
    pub id: String,
    #[serde(default)]
    pub phase_settings: std::collections::HashMap<String, WorkflowPhaseRuntimeSettings>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkflowRuntimeConfigLite {
    #[serde(default)]
    pub default_workflow_ref: String,
    #[serde(default)]
    pub workflows: Vec<WorkflowPipelineRuntimeRecord>,
}

fn workflow_runtime_config_paths(project_root: &str) -> [PathBuf; 2] {
    [
        Path::new(project_root)
            .join(".ao")
            .join("state")
            .join("workflow-config.json"),
        Path::new(project_root)
            .join(".ao")
            .join("workflow-config.json"),
    ]
}

pub fn load_workflow_runtime_config(project_root: &str) -> WorkflowRuntimeConfigLite {
    for path in workflow_runtime_config_paths(project_root) {
        if !path.exists() {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };

        if let Ok(parsed) = serde_json::from_str::<WorkflowRuntimeConfigLite>(&content) {
            return parsed;
        }
    }

    WorkflowRuntimeConfigLite::default()
}

pub fn resolve_phase_runtime_settings(
    config: &WorkflowRuntimeConfigLite,
    workflow_ref: Option<&str>,
    phase_id: &str,
) -> Option<WorkflowPhaseRuntimeSettings> {
    let requested_pipeline = workflow_ref
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let value = config.default_workflow_ref.trim();
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        })?;

    let pipeline = config
        .workflows
        .iter()
        .find(|pipeline| pipeline.id.eq_ignore_ascii_case(requested_pipeline))?;

    pipeline
        .phase_settings
        .iter()
        .find(|(phase_key, _)| phase_key.eq_ignore_ascii_case(phase_id))
        .map(|(_, settings)| settings.clone())
}

pub fn phase_timeout_secs() -> Option<u64> {
    if std::env::var("AO_PHASE_WAIT_FOR_COMPLETION")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
    {
        return None;
    }

    match std::env::var("AO_PHASE_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        Some(0) => None,
        Some(value) => Some(value),
        None => None,
    }
}

fn parse_env_usize(key: &str) -> Option<usize> {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
}

pub fn phase_runner_attempts() -> usize {
    parse_env_usize("AO_PHASE_RUN_ATTEMPTS")
        .unwrap_or(3)
        .clamp(1, 10)
}

pub fn phase_max_continuations() -> usize {
    parse_env_usize("AO_PHASE_MAX_CONTINUATIONS")
        .unwrap_or(3)
        .clamp(0, 10)
}

pub fn bootstrap_max_requirements() -> usize {
    match parse_env_usize("AO_BOOTSTRAP_MAX_REQUIREMENTS") {
        Some(0) => usize::MAX,
        Some(value) => value,
        None => usize::MAX,
    }
}

pub fn requirement_needs_refinement(requirement: &RequirementItem) -> bool {
    if requirement.status == RequirementStatus::Draft {
        return true;
    }

    if requirement.acceptance_criteria.is_empty() {
        return true;
    }

    !requirement.acceptance_criteria.iter().any(|criterion| {
        criterion
            .to_ascii_lowercase()
            .contains("automated test coverage")
    })
}

fn codex_web_search_enabled(web_search_override: Option<bool>) -> bool {
    web_search_override
        .or_else(|| protocol::parse_env_bool_opt("AO_CODEX_WEB_SEARCH"))
        .unwrap_or(true)
}

fn claude_bypass_permissions_enabled() -> bool {
    protocol::parse_env_bool("AO_CLAUDE_BYPASS_PERMISSIONS")
}

fn env_codex_reasoning_effort_override() -> Option<String> {
    std::env::var("AO_CODEX_REASONING_EFFORT")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

fn codex_reasoning_effort(reasoning_override: Option<&str>) -> Option<String> {
    reasoning_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .or_else(env_codex_reasoning_effort_override)
}

fn codex_exec_insert_index(args: &[Value]) -> usize {
    args.iter()
        .position(|item| item.as_str().is_some_and(|v| v == "exec"))
        .unwrap_or(0)
}

fn launch_prompt_insert_index(args: &[Value]) -> usize {
    args.len().saturating_sub(1)
}

fn ensure_flag_value_if_missing(args: &mut Vec<Value>, flag: &str, value: &str, insert_at: usize) {
    if args
        .iter()
        .any(|item| item.as_str().is_some_and(|v| v == flag))
    {
        return;
    }
    let insert_at = insert_at.min(args.len());
    args.insert(insert_at, Value::String(flag.to_string()));
    args.insert(
        (insert_at + 1).min(args.len()),
        Value::String(value.to_string()),
    );
}

fn ensure_codex_config_override(args: &mut Vec<Value>, key: &str, value_expr: &str) {
    let key_prefix = format!("{key}=");
    let target = format!("{key}={value_expr}");
    let mut index = 0usize;
    while index + 1 < args.len() {
        let flag = args[index].as_str().unwrap_or_default();
        let value = args
            .get(index + 1)
            .and_then(Value::as_str)
            .unwrap_or_default();
        if (flag == "-c" || flag == "--config") && value.starts_with(&key_prefix) {
            args[index + 1] = Value::String(target);
            return;
        }
        index += 1;
    }
    let insert_at = codex_exec_insert_index(args);
    args.insert(insert_at, Value::String("-c".to_string()));
    args.insert(insert_at + 1, Value::String(target));
}

fn codex_network_access_enabled(network_access_override: Option<bool>) -> bool {
    network_access_override
        .or_else(|| protocol::parse_env_bool_opt("AO_CODEX_NETWORK_ACCESS"))
        .unwrap_or(true)
}

fn parse_env_string_list_json(
    key: &str,
    fallback_key: Option<&str>,
    split_by_semicolon: bool,
) -> Vec<String> {
    let parse_json = |raw: &str| {
        serde_json::from_str::<Vec<String>>(raw)
            .ok()
            .unwrap_or_default()
    };
    let normalize = |items: Vec<String>| {
        items
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>()
    };

    if let Ok(raw) = std::env::var(key) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return normalize(parse_json(trimmed));
        }
    }

    let Some(fallback_key) = fallback_key else {
        return Vec::new();
    };

    let Ok(raw) = std::env::var(fallback_key) else {
        return Vec::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if split_by_semicolon {
        return normalize(trimmed.split(';').map(ToOwned::to_owned).collect());
    }

    normalize(trimmed.split_whitespace().map(ToOwned::to_owned).collect())
}

fn cli_tool_extra_args_env_keys(tool_id: &str) -> Option<(&'static str, &'static str)> {
    match tool_id.trim().to_ascii_lowercase().as_str() {
        "codex" => Some(("AO_CODEX_EXTRA_ARGS_JSON", "AO_CODEX_EXTRA_ARGS")),
        "claude" => Some(("AO_CLAUDE_EXTRA_ARGS_JSON", "AO_CLAUDE_EXTRA_ARGS")),
        "gemini" => Some(("AO_GEMINI_EXTRA_ARGS_JSON", "AO_GEMINI_EXTRA_ARGS")),
        "opencode" | "open-code" => Some(("AO_OPENCODE_EXTRA_ARGS_JSON", "AO_OPENCODE_EXTRA_ARGS")),
        _ => None,
    }
}

fn resolved_phase_extra_args(
    tool_id: &str,
    phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>,
) -> Vec<String> {
    if let Some(settings) = phase_runtime_settings {
        let explicit = settings
            .extra_args
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !explicit.is_empty() {
            return explicit;
        }
    }

    let mut resolved = parse_env_string_list_json(
        "AO_AI_CLI_EXTRA_ARGS_JSON",
        Some("AO_AI_CLI_EXTRA_ARGS"),
        false,
    );
    if let Some((json_key, plain_key)) = cli_tool_extra_args_env_keys(tool_id) {
        resolved.extend(parse_env_string_list_json(json_key, Some(plain_key), false));
    }

    resolved
}

fn parse_codex_override_entry(entry: &str) -> Option<(String, String)> {
    let trimmed = entry.trim();
    let (key, value_expr) = trimmed.split_once('=')?;
    let key = key.trim();
    let value_expr = value_expr.trim();
    if key.is_empty() || value_expr.is_empty() {
        return None;
    }
    Some((key.to_string(), value_expr.to_string()))
}

fn resolved_codex_config_overrides(
    phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>,
) -> Vec<(String, String)> {
    let from_settings = phase_runtime_settings
        .map(|settings| settings.codex_config_overrides.as_slice())
        .unwrap_or_default();
    let overrides: Vec<(String, String)> = from_settings
        .iter()
        .filter_map(|entry| parse_codex_override_entry(entry))
        .collect();

    if !overrides.is_empty() {
        return overrides;
    }

    parse_env_string_list_json(
        "AO_CODEX_EXTRA_CONFIG_OVERRIDES_JSON",
        Some("AO_CODEX_EXTRA_CONFIG_OVERRIDES"),
        true,
    )
    .iter()
    .filter_map(|entry| parse_codex_override_entry(entry))
    .collect()
}

fn inject_cli_extra_args(
    runtime_contract: &mut Value,
    tool_id: &str,
    phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>,
) {
    let extra_args = resolved_phase_extra_args(tool_id, phase_runtime_settings);
    if extra_args.is_empty() {
        return;
    }

    let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    let mut insert_at = launch_prompt_insert_index(args);
    for extra_arg in extra_args {
        args.insert(insert_at, Value::String(extra_arg));
        insert_at += 1;
    }
}

fn inject_codex_extra_config_overrides(
    runtime_contract: &mut Value,
    tool_id: &str,
    phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>,
) {
    if !tool_id.eq_ignore_ascii_case("codex") {
        return;
    }

    let overrides = resolved_codex_config_overrides(phase_runtime_settings);
    if overrides.is_empty() {
        return;
    }

    if let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    {
        for (key, value_expr) in overrides {
            ensure_codex_config_override(args, &key, &value_expr);
        }
    }
}

pub fn inject_codex_search_launch_flag(
    runtime_contract: &mut Value,
    tool_id: &str,
    web_search_override: Option<bool>,
) {
    if !tool_id.eq_ignore_ascii_case("codex") || !codex_web_search_enabled(web_search_override) {
        return;
    }

    if let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    {
        let has_search_flag = args
            .iter()
            .any(|item| item.as_str().is_some_and(|value| value == "--search"));
        if !has_search_flag {
            let insert_at = codex_exec_insert_index(args);
            args.insert(insert_at, Value::String("--search".to_string()));
        }
    }

    if let Some(capabilities) = runtime_contract
        .pointer_mut("/cli/capabilities")
        .and_then(Value::as_object_mut)
    {
        capabilities.insert("supports_web_search".to_string(), Value::Bool(true));
    }
}

pub fn inject_codex_reasoning_effort(
    runtime_contract: &mut Value,
    tool_id: &str,
    reasoning_override: Option<&str>,
) {
    if !tool_id.eq_ignore_ascii_case("codex") {
        return;
    }
    let Some(effort) = codex_reasoning_effort(reasoning_override) else {
        return;
    };

    if let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    {
        let mut has_override = false;
        for window in args.windows(2) {
            let Some(flag) = window[0].as_str() else {
                continue;
            };
            let Some(value) = window[1].as_str() else {
                continue;
            };
            if flag == "-c" && value.starts_with("model_reasoning_effort=") {
                has_override = true;
                break;
            }
        }
        if !has_override {
            let insert_at = codex_exec_insert_index(args);
            args.insert(insert_at, Value::String("-c".to_string()));
            args.insert(
                insert_at + 1,
                Value::String(format!("model_reasoning_effort={effort}")),
            );
        }
    }
}

pub fn inject_codex_network_access(
    runtime_contract: &mut Value,
    tool_id: &str,
    network_access_override: Option<bool>,
) {
    if !tool_id.eq_ignore_ascii_case("codex") {
        return;
    }

    let value_expr = if codex_network_access_enabled(network_access_override) {
        "true"
    } else {
        "false"
    };

    if let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    {
        ensure_codex_config_override(args, "sandbox_workspace_write.network_access", value_expr);
    }
}

pub fn inject_claude_permission_mode(runtime_contract: &mut Value, tool_id: &str) {
    if !tool_id.eq_ignore_ascii_case("claude") || !claude_bypass_permissions_enabled() {
        return;
    }

    if let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    {
        ensure_flag_value_if_missing(args, "--permission-mode", "bypassPermissions", 0);
    }
}

pub fn inject_cli_launch_overrides(
    runtime_contract: &mut Value,
    tool_id: &str,
    phase_runtime_settings: Option<&WorkflowPhaseRuntimeSettings>,
) {
    inject_codex_search_launch_flag(
        runtime_contract,
        tool_id,
        phase_runtime_settings.and_then(|settings| settings.web_search),
    );
    inject_codex_reasoning_effort(
        runtime_contract,
        tool_id,
        phase_runtime_settings.and_then(|settings| settings.reasoning_effort.as_deref()),
    );
    inject_codex_network_access(
        runtime_contract,
        tool_id,
        phase_runtime_settings.and_then(|settings| settings.network_access),
    );
    inject_claude_permission_mode(runtime_contract, tool_id);
    inject_codex_extra_config_overrides(runtime_contract, tool_id, phase_runtime_settings);
    inject_cli_extra_args(runtime_contract, tool_id, phase_runtime_settings);
}
