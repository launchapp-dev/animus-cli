#[cfg(unix)]
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use orchestrator_core::runtime_contract;
use protocol::{
    AgentRunEvent, AgentRunRequest, IpcAuthRequest, IpcAuthResult, ModelId, RunId,
    MAX_UNIX_SOCKET_PATH_LEN, PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::Duration;
use uuid::Uuid;

fn runner_scope_from_env() -> String {
    std::env::var("AO_RUNNER_SCOPE")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "project".to_string())
}

fn default_global_config_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.launchpad.agent-orchestrator")
    }

    #[cfg(target_os = "windows")]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.launchpad.agent-orchestrator")
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("agent-orchestrator")
    }
}

fn scoped_ao_root(project_root: &Path) -> Option<PathBuf> {
    protocol::scoped_state_root(project_root)
}

pub fn runner_config_dir(project_root: &Path) -> PathBuf {
    let config_dir = if let Some(override_path) = std::env::var("AO_RUNNER_CONFIG_DIR")
        .ok()
        .or_else(|| std::env::var("AO_CONFIG_DIR").ok())
        .or_else(|| std::env::var("AGENT_ORCHESTRATOR_CONFIG_DIR").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        PathBuf::from(override_path)
    } else if runner_scope_from_env() == "global" {
        default_global_config_dir()
    } else {
        scoped_ao_root(project_root)
            .unwrap_or_else(|| project_root.join(".ao"))
            .join("runner")
    };

    normalize_runner_config_dir(config_dir)
}

fn normalize_runner_config_dir(config_dir: PathBuf) -> PathBuf {
    #[cfg(unix)]
    {
        shorten_runner_config_dir_if_needed(config_dir)
    }

    #[cfg(not(unix))]
    {
        config_dir
    }
}

#[cfg(unix)]
fn shorten_runner_config_dir_if_needed(config_dir: PathBuf) -> PathBuf {
    let socket_path = config_dir.join("agent-runner.sock");
    let socket_len = socket_path.as_os_str().to_string_lossy().len();
    if socket_len <= MAX_UNIX_SOCKET_PATH_LEN {
        return config_dir;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config_dir.to_string_lossy().hash(&mut hasher);
    let digest = hasher.finish();
    let shortened = std::env::temp_dir()
        .join("ao-runner")
        .join(format!("{digest:016x}"));
    let _ = std::fs::create_dir_all(&shortened);
    let _ = std::fs::write(
        shortened.join("origin-path.txt"),
        config_dir.to_string_lossy().as_bytes(),
    );
    shortened
}

#[cfg(unix)]
pub async fn connect_runner(config_dir: &Path) -> Result<tokio::net::UnixStream> {
    let socket_path = config_dir.join("agent-runner.sock");
    let connect_timeout_secs = std::env::var("AO_RUNNER_CONNECT_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| secs.clamp(1, 30))
        .unwrap_or(5);
    let connect_future = tokio::net::UnixStream::connect(&socket_path);
    match tokio::time::timeout(Duration::from_secs(connect_timeout_secs), connect_future).await {
        Ok(Ok(mut stream)) => {
            authenticate_runner_stream(&mut stream, config_dir)
                .await
                .map_err(|error| {
                    anyhow!(
                        "failed to authenticate runner connection at {}: {error}",
                        socket_path.display()
                    )
                })?;
            Ok(stream)
        }
        Ok(Err(error)) => {
            let base_message = format!(
                "failed to connect to runner socket at {} (timeout={}s)",
                socket_path.display(),
                connect_timeout_secs
            );
            let hint = if socket_path.exists() {
                format!("{base_message}. socket file exists and may be stale")
            } else {
                base_message
            };
            Err(anyhow!("{hint}: {error}"))
        }
        Err(_) => Err(anyhow!(
            "timed out connecting to runner socket at {} after {}s; if no runner is active, remove stale socket and restart runner",
            socket_path.display(),
            connect_timeout_secs
        )),
    }
}

#[cfg(not(unix))]
pub async fn connect_runner(config_dir: &Path) -> Result<tokio::net::TcpStream> {
    let mut stream = tokio::net::TcpStream::connect("127.0.0.1:9001")
        .await
        .map_err(|error| anyhow!("failed to connect to runner at 127.0.0.1:9001: {error}"))?;
    authenticate_runner_stream(&mut stream, config_dir)
        .await
        .map_err(|error| {
            anyhow!("failed to authenticate runner connection at 127.0.0.1:9001: {error}")
        })?;
    Ok(stream)
}

pub async fn authenticate_runner_stream<S>(stream: &mut S, config_dir: &Path) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let token = protocol::Config::load_from_dir(config_dir)
        .map_err(|error| {
            anyhow!(
                "failed to load runner config for authentication from {}: {error}",
                config_dir.display()
            )
        })?
        .get_token()
        .map_err(|error| {
            format!(
                "agent runner token unavailable; set AGENT_RUNNER_TOKEN or configure agent_runner_token: {error}"
            )
        })
        .map_err(|msg| anyhow!(msg))?;

    write_json_line(stream, &IpcAuthRequest::new(token))
        .await
        .map_err(|error| anyhow!("failed to send runner auth payload: {error}"))?;

    let mut line = String::new();
    let read_len = tokio::time::timeout(Duration::from_secs(2), async {
        let mut reader = BufReader::new(stream);
        reader.read_line(&mut line).await
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for runner auth response"))?
    .map_err(|error| anyhow!("failed to read runner auth response: {error}"))?;

    if read_len == 0 {
        return Err(anyhow!("runner closed connection before auth completed",));
    }

    let response: IpcAuthResult = serde_json::from_str(line.trim())
        .map_err(|error| anyhow!("received malformed runner auth response: {error}"))?;
    if response.ok {
        return Ok(());
    }

    let failure_code = response.code.map(|code| code.as_str()).unwrap_or("unknown");
    let message = response
        .message
        .unwrap_or_else(|| "unauthorized".to_string());
    Err(anyhow!(
        "runner authentication failed ({failure_code}): {message}"
    ))
}

pub async fn write_json_line<W: AsyncWrite + Unpin, T: serde::Serialize>(
    writer: &mut W,
    payload: &T,
) -> Result<()> {
    let json = serde_json::to_string(payload)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

pub fn build_runtime_contract(tool: &str, model: &str, prompt: &str) -> Option<Value> {
    build_runtime_contract_with_resume(tool, model, prompt, None)
}

pub fn build_runtime_contract_with_resume(
    tool: &str,
    model: &str,
    prompt: &str,
    resume_plan: Option<&orchestrator_core::runtime_contract::CliSessionResumePlan>,
) -> Option<Value> {
    let mcp_config = protocol::McpRuntimeConfig::from_env();
    let mcp_endpoint = mcp_config.endpoint.clone();
    let mcp_agent_id = mcp_config.agent_id.clone();

    let mut runtime_contract = runtime_contract::build_runtime_contract(
        tool,
        model,
        prompt,
        resume_plan,
        None,
        mcp_endpoint.as_deref(),
        mcp_agent_id.as_deref(),
    )?;
    inject_cli_launch_overrides_from_env(&mut runtime_contract, tool);
    Some(runtime_contract)
}

fn codex_web_search_enabled() -> bool {
    protocol::parse_env_bool_opt("AO_CODEX_WEB_SEARCH").unwrap_or(true)
}

fn codex_network_access_enabled() -> bool {
    protocol::parse_env_bool_opt("AO_CODEX_NETWORK_ACCESS").unwrap_or(true)
}

fn claude_bypass_permissions_enabled() -> bool {
    protocol::parse_env_bool("AO_CLAUDE_BYPASS_PERMISSIONS")
}

fn env_codex_reasoning_effort_override() -> Option<String> {
    std::env::var("AO_CODEX_REASONING_EFFORT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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

fn resolved_codex_extra_overrides() -> Vec<(String, String)> {
    parse_env_string_list_json(
        "AO_CODEX_EXTRA_CONFIG_OVERRIDES_JSON",
        Some("AO_CODEX_EXTRA_CONFIG_OVERRIDES"),
        true,
    )
    .iter()
    .filter_map(|entry| parse_codex_override_entry(entry))
    .collect()
}

fn cli_tool_extra_args_env_keys(tool: &str) -> Option<(&'static str, &'static str)> {
    match tool.trim().to_ascii_lowercase().as_str() {
        "codex" => Some(("AO_CODEX_EXTRA_ARGS_JSON", "AO_CODEX_EXTRA_ARGS")),
        "claude" => Some(("AO_CLAUDE_EXTRA_ARGS_JSON", "AO_CLAUDE_EXTRA_ARGS")),
        "gemini" => Some(("AO_GEMINI_EXTRA_ARGS_JSON", "AO_GEMINI_EXTRA_ARGS")),
        "opencode" | "open-code" => Some(("AO_OPENCODE_EXTRA_ARGS_JSON", "AO_OPENCODE_EXTRA_ARGS")),
        _ => None,
    }
}

fn resolved_extra_args(tool: &str) -> Vec<String> {
    let mut args = parse_env_string_list_json(
        "AO_AI_CLI_EXTRA_ARGS_JSON",
        Some("AO_AI_CLI_EXTRA_ARGS"),
        false,
    );
    if let Some((json_key, plain_key)) = cli_tool_extra_args_env_keys(tool) {
        args.extend(parse_env_string_list_json(json_key, Some(plain_key), false));
    }
    args
}

fn inject_codex_search_launch_flag(runtime_contract: &mut Value, tool: &str) {
    if !tool.eq_ignore_ascii_case("codex") || !codex_web_search_enabled() {
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

fn inject_codex_reasoning_effort_override(runtime_contract: &mut Value, tool: &str) {
    if !tool.eq_ignore_ascii_case("codex") {
        return;
    }
    let Some(effort) = env_codex_reasoning_effort_override() else {
        return;
    };
    let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    let mut index = 0usize;
    while index + 1 < args.len() {
        let flag = args[index].as_str().unwrap_or_default();
        let value = args[index + 1].as_str().unwrap_or_default();
        if flag == "-c" && value.starts_with("model_reasoning_effort=") {
            args[index + 1] = Value::String(format!("model_reasoning_effort={effort}"));
            return;
        }
        index += 1;
    }

    let insert_at = codex_exec_insert_index(args);
    args.insert(insert_at, Value::String("-c".to_string()));
    args.insert(
        insert_at + 1,
        Value::String(format!("model_reasoning_effort={effort}")),
    );
}

fn inject_codex_network_access_override(runtime_contract: &mut Value, tool: &str) {
    if !tool.eq_ignore_ascii_case("codex") {
        return;
    }
    let value_expr = if codex_network_access_enabled() {
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

fn inject_codex_extra_config_overrides(runtime_contract: &mut Value, tool: &str) {
    if !tool.eq_ignore_ascii_case("codex") {
        return;
    }
    let overrides = resolved_codex_extra_overrides();
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

fn inject_claude_permission_mode_override(runtime_contract: &mut Value, tool: &str) {
    if !tool.eq_ignore_ascii_case("claude") || !claude_bypass_permissions_enabled() {
        return;
    }
    if let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    {
        ensure_flag_value_if_missing(args, "--permission-mode", "bypassPermissions", 0);
    }
}

fn inject_cli_extra_args_from_env(runtime_contract: &mut Value, tool: &str) {
    let extra_args = resolved_extra_args(tool);
    if extra_args.is_empty() {
        return;
    }
    if let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    {
        let mut insert_at = launch_prompt_insert_index(args);
        for extra_arg in extra_args {
            args.insert(insert_at, Value::String(extra_arg));
            insert_at += 1;
        }
    }
}

fn inject_cli_launch_overrides_from_env(runtime_contract: &mut Value, tool: &str) {
    inject_codex_search_launch_flag(runtime_contract, tool);
    inject_codex_reasoning_effort_override(runtime_contract, tool);
    inject_codex_network_access_override(runtime_contract, tool);
    inject_claude_permission_mode_override(runtime_contract, tool);
    inject_codex_extra_config_overrides(runtime_contract, tool);
    inject_cli_extra_args_from_env(runtime_contract, tool);
}

pub fn event_matches_run(event: &AgentRunEvent, run_id: &RunId) -> bool {
    match event {
        AgentRunEvent::Started {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::OutputChunk {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::Metadata {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::Error {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::Finished {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::ToolCall {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::ToolResult {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::Artifact {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::Thinking {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
    }
}

pub fn ensure_safe_run_id(run_id: &str) -> Result<()> {
    if run_id.trim().is_empty() {
        return Err(anyhow!("run_id is required"));
    }
    if run_id.contains('/') || run_id.contains('\\') || run_id.contains("..") {
        return Err(anyhow!("invalid run_id: {run_id}"));
    }
    Ok(())
}

pub fn run_dir(project_root: &str, run_id: &RunId, base_override: Option<&str>) -> PathBuf {
    let base = base_override.map(PathBuf::from).unwrap_or_else(|| {
        scoped_ao_root(Path::new(project_root))
            .unwrap_or_else(|| Path::new(project_root).join(".ao"))
            .join("runs")
    });
    base.join(&run_id.0)
}

pub fn collect_json_payload_lines(text: &str) -> Vec<(String, Value)> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let parsed = serde_json::from_str::<Value>(trimmed).ok()?;
            if parsed.is_object() || parsed.is_array() {
                Some((trimmed.to_string(), parsed))
            } else {
                None
            }
        })
        .collect()
}

pub fn append_line(path: &Path, line: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

pub async fn run_prompt_against_runner(
    project_root: &str,
    prompt: &str,
    model: &str,
    tool: &str,
    timeout_secs: u64,
) -> Result<String> {
    let run_id = RunId(format!("task-gen-{}", Uuid::new_v4()));
    let mut context = serde_json::json!({
        "tool": tool,
        "prompt": prompt,
        "cwd": project_root,
        "project_root": project_root,
        "planning_stage": "task-generation",
        "allowed_tools": ["Read", "Glob", "Grep", "WebSearch"],
        "timeout_secs": timeout_secs,
    });
    if let Some(runtime_contract) = build_runtime_contract(tool, model, prompt) {
        context["runtime_contract"] = runtime_contract;
    }

    let request = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run_id.clone(),
        model: ModelId(model.to_string()),
        context,
        timeout_secs: Some(timeout_secs),
    };

    let config_dir = runner_config_dir(Path::new(project_root));
    let stream = connect_runner(&config_dir).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut transcript = String::new();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, &run_id) {
            continue;
        }

        match event {
            AgentRunEvent::OutputChunk { text, .. } => {
                transcript.push_str(&text);
                transcript.push('\n');
            }
            AgentRunEvent::Thinking { content, .. } => {
                transcript.push_str(&content);
                transcript.push('\n');
            }
            AgentRunEvent::Error { error, .. } => {
                return Err(anyhow!("task generation run failed: {error}"));
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    return Err(anyhow!(
                        "task generation run exited with non-zero code: {:?}",
                        exit_code
                    ));
                }
                break;
            }
            _ => {}
        }
    }

    if transcript.trim().is_empty() {
        return Err(anyhow!("task generation run produced empty output"));
    }

    Ok(transcript)
}
