#[cfg(unix)]
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use orchestrator_core::runtime_contract;
use protocol::{
    AgentControlAction, AgentRunEvent, IpcAuthRequest, IpcAuthResult, ModelStatus,
    OutputStreamType, RunId, MAX_UNIX_SOCKET_PATH_LEN,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::Duration;

use crate::{unavailable_error, AgentControlActionArg, AgentRunArgs, RunnerScopeArg};

impl From<AgentControlActionArg> for AgentControlAction {
    fn from(value: AgentControlActionArg) -> Self {
        match value {
            AgentControlActionArg::Pause => AgentControlAction::Pause,
            AgentControlActionArg::Resume => AgentControlAction::Resume,
            AgentControlActionArg::Terminate => AgentControlAction::Terminate,
        }
    }
}

pub(crate) struct RunnerScopeEnvGuard {
    previous: Option<String>,
    changed: bool,
}

impl RunnerScopeEnvGuard {
    pub(crate) fn new(scope: Option<&RunnerScopeArg>) -> Self {
        let previous = std::env::var("AO_RUNNER_SCOPE").ok();
        if let Some(scope) = scope {
            std::env::set_var("AO_RUNNER_SCOPE", runner_scope_label(scope));
            Self {
                previous,
                changed: true,
            }
        } else {
            Self {
                previous,
                changed: false,
            }
        }
    }
}

impl Drop for RunnerScopeEnvGuard {
    fn drop(&mut self) {
        if !self.changed {
            return;
        }

        if let Some(value) = &self.previous {
            std::env::set_var("AO_RUNNER_SCOPE", value);
        } else {
            std::env::remove_var("AO_RUNNER_SCOPE");
        }
    }
}

fn runner_scope_label(scope: &RunnerScopeArg) -> &'static str {
    match scope {
        RunnerScopeArg::Project => "project",
        RunnerScopeArg::Global => "global",
    }
}

fn runner_scope_from_env() -> String {
    std::env::var("AO_RUNNER_SCOPE")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "project".to_string())
}

fn canonicalize_cwd_in_project(path: &str, project_root: &str) -> Result<String> {
    let root = PathBuf::from(project_root);
    let root_canonical = root
        .canonicalize()
        .with_context(|| format!("failed to resolve project root '{}'", project_root))?;
    let candidate = PathBuf::from(path);
    let resolved_candidate = if candidate.is_absolute() {
        candidate
    } else {
        root_canonical.join(candidate)
    };
    let candidate_canonical = resolved_candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve cwd '{}'", resolved_candidate.display()))?;
    let inside_project_root = candidate_canonical.starts_with(&root_canonical);
    let inside_managed_worktree =
        is_managed_worktree_for_project(&candidate_canonical, &root_canonical);
    if !inside_project_root && !inside_managed_worktree {
        return Err(anyhow!(
            "Security violation: cwd '{}' is not within project root '{}'",
            path,
            project_root
        ));
    }
    Ok(candidate_canonical.to_string_lossy().to_string())
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

pub(crate) fn runner_config_dir(project_root: &Path) -> PathBuf {
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

fn scoped_ao_root(project_root: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".ao")
            .join(protocol::repository_scope_for_path(project_root)),
    )
}

fn is_managed_worktree_for_project(candidate_cwd: &Path, project_root: &Path) -> bool {
    let mut cursor = candidate_cwd.parent();
    while let Some(path) = cursor {
        if path.file_name().and_then(|value| value.to_str()) == Some("worktrees") {
            let Some(repo_ao_root) = path.parent() else {
                return false;
            };
            let marker_path = repo_ao_root.join(".project-root");
            let Ok(marker_content) = std::fs::read_to_string(marker_path) else {
                return false;
            };
            let recorded_root = marker_content.trim();
            if recorded_root.is_empty() {
                return false;
            }
            let Ok(recorded_canonical) = Path::new(recorded_root).canonicalize() else {
                return false;
            };
            return recorded_canonical == project_root;
        }
        cursor = path.parent();
    }
    false
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
pub(crate) async fn connect_runner(config_dir: &Path) -> Result<tokio::net::UnixStream> {
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
                    unavailable_error(format!(
                        "failed to authenticate runner connection at {}: {error}",
                        socket_path.display()
                    ))
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
            Err(unavailable_error(format!("{hint}: {error}")))
        }
        Err(_) => Err(unavailable_error(format!(
            "timed out connecting to runner socket at {} after {}s; if no runner is active, remove stale socket and restart runner",
            socket_path.display(),
            connect_timeout_secs
        ))),
    }
}

#[cfg(not(unix))]
pub(crate) async fn connect_runner(config_dir: &Path) -> Result<tokio::net::TcpStream> {
    let mut stream = tokio::net::TcpStream::connect("127.0.0.1:9001")
        .await
        .map_err(|error| {
            unavailable_error(format!(
                "failed to connect to runner at 127.0.0.1:9001: {error}"
            ))
        })?;
    authenticate_runner_stream(&mut stream, config_dir)
        .await
        .map_err(|error| {
            unavailable_error(format!(
                "failed to authenticate runner connection at 127.0.0.1:9001: {error}"
            ))
        })?;
    Ok(stream)
}

async fn authenticate_runner_stream<S>(stream: &mut S, config_dir: &Path) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let token = protocol::Config::load_from_dir(config_dir)
        .map_err(|error| {
            unavailable_error(format!(
                "failed to load runner config for authentication from {}: {error}",
                config_dir.display()
            ))
        })?
        .get_token()
        .map_err(|error| {
            format!(
                "agent runner token unavailable; set AGENT_RUNNER_TOKEN or configure agent_runner_token: {error}"
            )
        })
        .map_err(unavailable_error)?;

    write_json_line(stream, &IpcAuthRequest::new(token))
        .await
        .map_err(|error| {
            unavailable_error(format!("failed to send runner auth payload: {error}"))
        })?;

    let mut line = String::new();
    let read_len = tokio::time::timeout(Duration::from_secs(2), async {
        let mut reader = BufReader::new(stream);
        reader.read_line(&mut line).await
    })
    .await
    .map_err(|_| unavailable_error("timed out waiting for runner auth response"))?
    .map_err(|error| unavailable_error(format!("failed to read runner auth response: {error}")))?;

    if read_len == 0 {
        return Err(unavailable_error(
            "runner closed connection before auth completed",
        ));
    }

    let response: IpcAuthResult = serde_json::from_str(line.trim()).map_err(|error| {
        unavailable_error(format!("received malformed runner auth response: {error}"))
    })?;
    if response.ok {
        return Ok(());
    }

    let failure_code = response.code.map(|code| code.as_str()).unwrap_or("unknown");
    let message = response
        .message
        .unwrap_or_else(|| "unauthorized".to_string());
    Err(unavailable_error(format!(
        "runner authentication failed ({failure_code}): {message}"
    )))
}

pub(crate) async fn write_json_line<W: AsyncWrite + Unpin, T: serde::Serialize>(
    writer: &mut W,
    payload: &T,
) -> Result<()> {
    let json = serde_json::to_string(payload)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

pub(crate) fn build_agent_context(args: &AgentRunArgs, project_root: &str) -> Result<Value> {
    let mut context = if let Some(context_json) = &args.context_json {
        serde_json::from_str::<Value>(context_json)?
    } else {
        serde_json::json!({})
    };

    let context_obj = context
        .as_object_mut()
        .ok_or_else(|| anyhow!("agent context must be a JSON object"))?;

    context_obj
        .entry("tool".to_string())
        .or_insert_with(|| Value::String(args.tool.clone()));

    if let Some(prompt) = &args.prompt {
        context_obj
            .entry("prompt".to_string())
            .or_insert_with(|| Value::String(prompt.clone()));
    }

    let cwd = args
        .cwd
        .clone()
        .or_else(|| {
            context_obj
                .get("cwd")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| project_root.to_string());
    let cwd = canonicalize_cwd_in_project(&cwd, project_root)?;
    context_obj.insert("cwd".to_string(), Value::String(cwd));
    context_obj.insert(
        "project_root".to_string(),
        Value::String(project_root.to_string()),
    );

    if let Some(timeout_secs) = args.timeout_secs {
        context_obj
            .entry("timeout_secs".to_string())
            .or_insert_with(|| Value::from(timeout_secs));
    }

    if let Some(runtime_contract_json) = &args.runtime_contract_json {
        context_obj.insert(
            "runtime_contract".to_string(),
            serde_json::from_str::<Value>(runtime_contract_json)?,
        );
    } else if !context_obj.contains_key("runtime_contract") {
        let prompt = context_obj
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(runtime_contract) = build_runtime_contract(&args.tool, &args.model, prompt) {
            context_obj.insert("runtime_contract".to_string(), runtime_contract);
        }
    }

    Ok(context)
}

pub(crate) fn build_runtime_contract(tool: &str, model: &str, prompt: &str) -> Option<Value> {
    let mcp_endpoint = std::env::var("AO_MCP_ENDPOINT")
        .ok()
        .or_else(|| std::env::var("MCP_ENDPOINT").ok())
        .or_else(|| std::env::var("OPENCODE_MCP_ENDPOINT").ok());
    let mcp_agent_id = std::env::var("AO_MCP_AGENT_ID").ok();

    let mut runtime_contract = runtime_contract::build_runtime_contract(
        tool,
        model,
        prompt,
        None,
        None,
        mcp_endpoint.as_deref(),
        mcp_agent_id.as_deref(),
    )?;
    inject_cli_launch_overrides_from_env(&mut runtime_contract, tool);
    Some(runtime_contract)
}

fn parse_env_flag_enabled(key: &str, default_value: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .map(|value| !matches!(value.as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(default_value)
}

fn codex_web_search_enabled() -> bool {
    parse_env_flag_enabled("AO_CODEX_WEB_SEARCH", true)
}

fn codex_network_access_enabled() -> bool {
    parse_env_flag_enabled("AO_CODEX_NETWORK_ACCESS", true)
}

fn claude_bypass_permissions_enabled() -> bool {
    parse_env_flag_enabled("AO_CLAUDE_BYPASS_PERMISSIONS", false)
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
    cli_wrapper::codex_exec_insert_index_json(args)
}

fn launch_prompt_insert_index(args: &[Value]) -> usize {
    cli_wrapper::launch_prompt_insert_index_json(args)
}

fn ensure_flag_value_if_missing(args: &mut Vec<Value>, flag: &str, value: &str, insert_at: usize) {
    cli_wrapper::ensure_flag_value_json(args, flag, value, insert_at);
}

fn ensure_codex_config_override(args: &mut Vec<Value>, key: &str, value_expr: &str) {
    cli_wrapper::ensure_codex_config_override_json(args, key, value_expr);
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

pub(crate) fn print_agent_event(event: &AgentRunEvent, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "schema": "ao.agent.event.v1",
                "ok": true,
                "data": event
            }))?
        );
        return Ok(());
    }

    match event {
        AgentRunEvent::Started { run_id, .. } => {
            println!("run {} started", run_id.0);
        }
        AgentRunEvent::OutputChunk {
            stream_type, text, ..
        } => match stream_type {
            OutputStreamType::Stderr => eprintln!("{text}"),
            OutputStreamType::Stdout | OutputStreamType::System => println!("{text}"),
        },
        AgentRunEvent::Metadata {
            run_id,
            cost,
            tokens,
        } => {
            println!("run {} metadata: cost={cost:?} tokens={tokens:?}", run_id.0);
        }
        AgentRunEvent::Error { run_id, error } => {
            eprintln!("run {} error: {error}", run_id.0);
        }
        AgentRunEvent::Finished {
            run_id,
            exit_code,
            duration_ms,
        } => {
            println!(
                "run {} finished: exit_code={exit_code:?} duration_ms={duration_ms}",
                run_id.0
            );
        }
        AgentRunEvent::ToolCall { run_id, tool_info } => {
            println!("run {} tool_call {}", run_id.0, tool_info.tool_name);
        }
        AgentRunEvent::ToolResult {
            run_id,
            result_info,
        } => {
            println!(
                "run {} tool_result {} success={}",
                run_id.0, result_info.tool_name, result_info.success
            );
        }
        AgentRunEvent::Artifact {
            run_id,
            artifact_info,
        } => {
            println!("run {} artifact {}", run_id.0, artifact_info.artifact_id);
        }
        AgentRunEvent::Thinking { run_id, content } => {
            println!(
                "run {} thinking: {} chars",
                run_id.0,
                content.chars().count()
            );
        }
    }

    Ok(())
}

pub(crate) fn print_model_status(status: ModelStatus) {
    let availability = serde_json::to_string(&status.availability)
        .unwrap_or_else(|_| "unknown".to_string())
        .trim_matches('"')
        .to_string();
    if let Some(details) = status.details {
        println!("{}: {} ({})", status.model.0, availability, details);
    } else {
        println!("{}: {}", status.model.0, availability);
    }
}

pub(crate) fn default_model_status_targets() -> Vec<String> {
    let mut tools: Vec<String> = protocol::default_model_specs()
        .into_iter()
        .map(|(_, tool)| tool)
        .collect();
    tools.sort();
    tools.dedup();
    tools
}

pub(crate) fn event_matches_run(event: &AgentRunEvent, run_id: &RunId) -> bool {
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

pub(crate) fn ensure_safe_run_id(run_id: &str) -> Result<()> {
    if run_id.trim().is_empty() {
        anyhow::bail!("run_id is required");
    }
    if run_id.contains('/') || run_id.contains('\\') || run_id.contains("..") {
        anyhow::bail!("invalid run_id");
    }
    Ok(())
}

pub(crate) fn run_dir(project_root: &str, run_id: &RunId, base_override: Option<&str>) -> PathBuf {
    let base = base_override.map(PathBuf::from).unwrap_or_else(|| {
        scoped_ao_root(Path::new(project_root))
            .unwrap_or_else(|| Path::new(project_root).join(".ao"))
            .join("runs")
    });
    base.join(&run_id.0)
}

pub(crate) fn persist_agent_event(run_dir: &Path, event: &AgentRunEvent) -> Result<()> {
    let path = run_dir.join("events.jsonl");
    let line = serde_json::to_string(event)?;
    append_line(&path, &line)
}

pub(crate) fn persist_json_output(
    run_dir: &Path,
    stream_type: OutputStreamType,
    text: &str,
) -> Result<()> {
    let path = run_dir.join("json-output.jsonl");
    for (raw, payload) in collect_json_payload_lines(text) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let entry = serde_json::json!({
            "timestamp_ms": timestamp_ms,
            "stream_type": stream_type_label(stream_type),
            "raw": raw,
            "payload": payload,
        });
        append_line(&path, &serde_json::to_string(&entry)?)?;
    }
    Ok(())
}

fn stream_type_label(stream_type: OutputStreamType) -> &'static str {
    match stream_type {
        OutputStreamType::Stdout => "stdout",
        OutputStreamType::Stderr => "stderr",
        OutputStreamType::System => "system",
    }
}

pub(crate) fn collect_json_payload_lines(text: &str) -> Vec<(String, Value)> {
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

pub(crate) fn append_line(path: &Path, line: &str) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, BufReader};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn write_config(dir: &Path, token: Option<&str>) {
        let payload = serde_json::json!({ "agent_runner_token": token });
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_string_pretty(&payload).expect("serialize config payload"),
        )
        .expect("write config file");
    }

    #[test]
    fn authenticate_runner_stream_uses_scoped_config_dir_token() {
        let _lock = env_lock().lock().expect("env lock");
        let global_dir = TempDir::new().expect("global temp dir");
        let scoped_dir = TempDir::new().expect("scoped temp dir");
        write_config(global_dir.path(), Some("global-token"));
        write_config(scoped_dir.path(), Some("scoped-token"));

        let global_override = global_dir.path().to_string_lossy().to_string();
        let _ao_config = EnvVarGuard::set("AO_CONFIG_DIR", Some(&global_override));
        let _legacy_config =
            EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", Some(&global_override));
        let _token_override = EnvVarGuard::set("AGENT_RUNNER_TOKEN", None);

        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        runtime.block_on(async {
            let (mut client, server) = tokio::io::duplex(1024);
            let server_task = tokio::spawn(async move {
                let mut reader = BufReader::new(server);
                let mut line = String::new();
                let read_len = reader
                    .read_line(&mut line)
                    .await
                    .expect("read auth request");
                assert!(read_len > 0, "expected auth request line");

                let request: IpcAuthRequest =
                    serde_json::from_str(line.trim()).expect("parse auth request");
                assert_eq!(request.token, "scoped-token");

                let mut server = reader.into_inner();
                write_json_line(&mut server, &IpcAuthResult::ok())
                    .await
                    .expect("write auth response");
            });

            authenticate_runner_stream(&mut client, scoped_dir.path())
                .await
                .expect("authenticate runner stream");

            server_task.await.expect("join server task");
        });
    }

    #[test]
    fn authenticate_runner_stream_fails_when_scoped_token_missing() {
        let _lock = env_lock().lock().expect("env lock");
        let scoped_dir = TempDir::new().expect("scoped temp dir");
        write_config(scoped_dir.path(), None);
        let _token_override = EnvVarGuard::set("AGENT_RUNNER_TOKEN", None);

        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        runtime.block_on(async {
            let (mut client, _server) = tokio::io::duplex(256);
            let error = authenticate_runner_stream(&mut client, scoped_dir.path())
                .await
                .expect_err("authentication should fail without runner token");
            assert!(
                error.to_string().contains("agent runner token unavailable"),
                "error should mention missing runner token: {error}"
            );
        });
    }

    #[test]
    fn claude_bypass_permissions_is_disabled_by_default() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", None);
        assert!(!claude_bypass_permissions_enabled());
    }

    #[test]
    fn claude_bypass_permissions_respects_enable_toggle() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some("true"));
        assert!(claude_bypass_permissions_enabled());
    }

    #[test]
    fn claude_bypass_permissions_respects_disable_toggle() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some("false"));
        assert!(!claude_bypass_permissions_enabled());
    }

    #[test]
    fn claude_bypass_permissions_treats_empty_value_as_disabled() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some(""));
        assert!(!claude_bypass_permissions_enabled());
    }

    #[test]
    fn inject_claude_permission_mode_override_is_disabled_by_default() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", None);
        let mut contract = build_runtime_contract("claude", "claude-opus-4-1", "hello")
            .expect("runtime contract should build");

        inject_claude_permission_mode_override(&mut contract, "claude");

        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_claude_permission_mode_override_respects_enable_toggle() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some("true"));
        let mut contract = build_runtime_contract("claude", "claude-opus-4-1", "hello")
            .expect("runtime contract should build");

        inject_claude_permission_mode_override(&mut contract, "claude");

        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(args.contains(&"--permission-mode"));
        assert!(args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_claude_permission_mode_override_respects_disable_toggle() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some("false"));
        let mut contract = build_runtime_contract("claude", "claude-opus-4-1", "hello")
            .expect("runtime contract should build");

        inject_claude_permission_mode_override(&mut contract, "claude");

        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }

    #[test]
    fn inject_claude_permission_mode_override_treats_empty_toggle_as_disabled() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let _bypass = EnvVarGuard::set("AO_CLAUDE_BYPASS_PERMISSIONS", Some(""));
        let mut contract = build_runtime_contract("claude", "claude-opus-4-1", "hello")
            .expect("runtime contract should build");

        inject_claude_permission_mode_override(&mut contract, "claude");

        let args = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch args should be present")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(!args.contains(&"--permission-mode"));
        assert!(!args.contains(&"bypassPermissions"));
    }
}
