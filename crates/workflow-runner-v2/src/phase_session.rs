use anyhow::{bail, Context, Result};
use cli_wrapper::{
    ensure_codex_config_override, ensure_flag, ensure_flag_value, is_ai_cli_tool, parse_launch_from_runtime_contract,
    LaunchInvocation, SessionBackendResolver, SessionEvent, SessionRequest,
};
use protocol::{
    AgentRunEvent, ArtifactInfo, ArtifactType, OutputStreamType, RunId, Timestamp, TokenUsage, ToolCallInfo,
    ToolResultInfo,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{Duration, MissedTickBehavior};
use tracing::{debug, info, warn};

fn truncate_for_log(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…")
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2).find_map(|pair| (pair[0] == flag).then_some(pair[1].as_str()))
}

// ── process_builder helpers ──────────────────────────────────────────────────

pub fn resolve_idle_timeout_secs(
    tool: &str,
    hard_timeout_secs: Option<u64>,
    runtime_contract: Option<&Value>,
) -> Option<u64> {
    if !is_ai_cli_tool(tool) {
        return None;
    }

    let contract_override = runtime_contract.and_then(|contract| {
        contract
            .pointer("/policy/idle_timeout_secs")
            .or_else(|| contract.pointer("/cli/policy/idle_timeout_secs"))
            .or_else(|| contract.pointer("/runner/idle_timeout_secs"))
            .and_then(|value| value.as_u64())
    });

    let requested = contract_override.unwrap_or(600);
    if requested == 0 {
        return None;
    }

    match hard_timeout_secs.filter(|value| *value > 0) {
        Some(hard_timeout_secs) => {
            let upper_bound = hard_timeout_secs.max(1);
            let lower_bound = if upper_bound < 30 { 1 } else { 30 };
            Some(requested.clamp(lower_bound, upper_bound))
        }
        None => Some(requested.max(30)),
    }
}

pub fn merge_launch_env(env: &mut HashMap<String, String>, invocation: &LaunchInvocation) {
    for (key, value) in &invocation.env {
        if env.contains_key(key) {
            warn!(env_key = %key, "Ignoring CLI launch env override for existing environment variable");
            continue;
        }
        env.insert(key.clone(), value.clone());
    }
}

pub async fn build_cli_invocation(
    tool: &str,
    model: &str,
    prompt: &str,
    runtime_contract: Option<&Value>,
) -> Result<LaunchInvocation> {
    if let Some(invocation) = parse_contract_launch(runtime_contract)? {
        debug!(
            tool,
            model,
            command = %invocation.command,
            args = ?invocation.args,
            prompt_via_stdin = invocation.prompt_via_stdin,
            "Using runtime contract launch configuration"
        );
        return Ok(invocation);
    }

    if is_ai_cli_tool(tool) {
        warn!(tool, model, "AI CLI tool requested without runtime contract launch configuration");
        bail!(
            "Missing runtime contract launch for AI CLI '{}'. Provide context.runtime_contract.cli.launch from cli-wrapper.",
            tool
        );
    }

    let _ = model;
    let args = match tool {
        "npm" | "pnpm" | "yarn" | "cargo" | "git" | "python" | "python3" | "node" => {
            prompt.split_whitespace().map(|s| s.to_string()).collect()
        }
        "echo" => vec![prompt.to_string()],
        _ if cli_wrapper::is_binary_on_path(tool) => {
            prompt.split_whitespace().map(|s| s.to_string()).collect()
        }
        _ => {
            warn!(tool, "Unsupported tool requested");
            bail!(
                "Unsupported tool: {}. Configure a supported CLI (claude, codex, gemini, opencode, oai-runner) or provide an executable on PATH.",
                tool
            )
        }
    };

    let invocation =
        LaunchInvocation { command: tool.to_string(), args, env: Default::default(), prompt_via_stdin: false };
    debug!(
        tool,
        model,
        command = %invocation.command,
        args = ?invocation.args,
        "Built fallback CLI invocation"
    );
    Ok(invocation)
}

fn parse_contract_launch(runtime_contract: Option<&Value>) -> Result<Option<LaunchInvocation>> {
    let invocation = parse_launch_from_runtime_contract(runtime_contract)?;
    if let Some(ref inv) = invocation {
        debug!(
            command = %inv.command,
            args = ?inv.args,
            prompt_via_stdin = inv.prompt_via_stdin,
            "Parsed runtime contract launch block via cli-wrapper"
        );
    }
    Ok(invocation)
}

// ── MCP policy types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct McpStdioConfig {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AdditionalMcpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct McpToolEnforcement {
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub stdio: Option<McpStdioConfig>,
    pub agent_id: String,
    pub allowed_prefixes: Vec<String>,
    pub tool_policy_allow: Vec<String>,
    pub tool_policy_deny: Vec<String>,
    pub additional_servers: Vec<AdditionalMcpServer>,
}

#[derive(Debug, Default)]
pub struct TempPathCleanup {
    paths: Vec<PathBuf>,
}

impl TempPathCleanup {
    pub fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }
}

impl Drop for TempPathCleanup {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ── MCP policy resolution ────────────────────────────────────────────────────

pub fn resolve_mcp_tool_enforcement(runtime_contract: Option<&Value>) -> McpToolEnforcement {
    let Some(contract) = runtime_contract else {
        return McpToolEnforcement {
            enabled: false,
            endpoint: None,
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: Vec::new(),
            tool_policy_allow: Vec::new(),
            tool_policy_deny: Vec::new(),
            additional_servers: Vec::new(),
        };
    };

    let supports_mcp =
        contract.pointer("/cli/capabilities/supports_mcp").and_then(Value::as_bool).unwrap_or(false);
    let endpoint = contract
        .pointer("/mcp/endpoint")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let stdio_command = contract
        .pointer("/mcp/stdio/command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let stdio_args = contract
        .pointer("/mcp/stdio/args")
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
    let stdio = stdio_command.map(|command| McpStdioConfig { command, args: stdio_args });
    let has_endpoint = endpoint.is_some();
    let has_stdio = stdio.is_some();
    let agent_id = contract
        .pointer("/mcp/agent_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ao")
        .to_string();
    let explicit_enforce = contract.pointer("/mcp/enforce_only").and_then(Value::as_bool);
    let enabled = explicit_enforce.unwrap_or((has_endpoint || has_stdio) && supports_mcp);

    let mut allowed_prefixes = contract
        .pointer("/mcp/allowed_tool_prefixes")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if enabled && allowed_prefixes.is_empty() {
        allowed_prefixes = protocol::default_allowed_tool_prefixes(&agent_id);
    }

    let parse_string_array = |pointer: &str| -> Vec<String> {
        contract
            .pointer(pointer)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let tool_policy_allow = parse_string_array("/mcp/tool_policy/allow");
    let tool_policy_deny = parse_string_array("/mcp/tool_policy/deny");

    let additional_servers = contract
        .pointer("/mcp/additional_servers")
        .and_then(Value::as_object)
        .map(|servers| {
            servers
                .iter()
                .map(|(name, entry)| AdditionalMcpServer {
                    name: name.clone(),
                    command: entry.get("command").and_then(Value::as_str).unwrap_or_default().to_string(),
                    args: entry
                        .get("args")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(ToString::to_string).collect())
                        .unwrap_or_default(),
                    env: entry
                        .get("env")
                        .and_then(Value::as_object)
                        .map(|e| {
                            e.iter().filter_map(|(k, v)| v.as_str().map(|val| (k.clone(), val.to_string()))).collect()
                        })
                        .unwrap_or_default(),
                })
                .filter(|s| !s.command.is_empty())
                .collect()
        })
        .unwrap_or_default();

    McpToolEnforcement {
        enabled,
        endpoint,
        stdio,
        agent_id,
        allowed_prefixes,
        tool_policy_allow,
        tool_policy_deny,
        additional_servers,
    }
}

// ── MCP policy application ───────────────────────────────────────────────────

fn canonical_cli_name(command: &str) -> String {
    let trimmed = command.trim();
    std::path::Path::new(trimmed).file_name().and_then(|value| value.to_str()).unwrap_or(trimmed).to_ascii_lowercase()
}

fn toml_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn is_safe_codex_server_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn parse_codex_mcp_server_names(payload: &str) -> Vec<String> {
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .map(|entries| {
            entries
                .into_iter()
                .filter_map(|entry| {
                    entry
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|name| is_safe_codex_server_name(name))
                        .map(ToString::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn discover_codex_mcp_server_names() -> Vec<String> {
    let output = match std::process::Command::new("codex")
        .args(["mcp", "list", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            debug!(error = %error, "Failed to list configured Codex MCP servers");
            return Vec::new();
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!(
            status = %output.status,
            stderr = %truncate_for_log(&stderr, 200),
            "Codex MCP list command returned non-success status"
        );
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_codex_mcp_server_names(&stdout)
}

#[derive(Debug, Clone, Copy)]
enum McpServerTransport<'a> {
    Http(&'a str),
    Stdio { command: &'a str, args: &'a [String] },
}

fn resolve_mcp_server_transport(enforcement: &McpToolEnforcement) -> Result<McpServerTransport<'_>> {
    if let Some(stdio) = enforcement.stdio.as_ref() {
        return Ok(McpServerTransport::Stdio { command: stdio.command.trim(), args: &stdio.args });
    }
    if let Some(endpoint) = enforcement.endpoint.as_deref() {
        return Ok(McpServerTransport::Http(endpoint));
    }

    bail!("MCP-only policy is enabled, but neither mcp.endpoint nor mcp.stdio.command is configured");
}

fn summarize_mcp_transport(
    transport: McpServerTransport<'_>,
) -> (&'static str, Option<&str>, Option<&str>, Option<&[String]>) {
    match transport {
        McpServerTransport::Http(endpoint) => ("http", Some(endpoint), None, None),
        McpServerTransport::Stdio { command, args } => ("stdio", None, Some(command), Some(args)),
    }
}

fn sanitize_token_for_filename(raw: &str) -> String {
    raw.chars().map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '_' }).collect()
}

fn write_temp_json_file(run_id: &RunId, prefix: &str, value: &Value) -> Result<PathBuf> {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!(
        "ao-{prefix}-{}-{}-{now_nanos}.json",
        sanitize_token_for_filename(&run_id.0),
        std::process::id()
    ));
    let payload = serde_json::to_vec(value).context("Failed to serialize strict MCP config JSON")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("Failed to write strict MCP config file {}", path.display()))?;
    Ok(path)
}

fn apply_claude_native_mcp_lockdown(
    args: &mut Vec<String>,
    transport: McpServerTransport<'_>,
    agent_id: &str,
    additional_servers: &[AdditionalMcpServer],
) {
    let primary = match transport {
        McpServerTransport::Http(endpoint) => json!({ "type": "http", "url": endpoint }),
        McpServerTransport::Stdio { command, args } => json!({ "command": command, "args": args }),
    };
    let mut mcp_servers = serde_json::Map::new();
    mcp_servers.insert(agent_id.to_string(), primary);
    for server in additional_servers {
        let mut config = serde_json::Map::new();
        config.insert("command".to_string(), Value::String(server.command.clone()));
        config.insert("args".to_string(), serde_json::to_value(&server.args).expect("server args should serialize"));
        if !server.env.is_empty() {
            config.insert("env".to_string(), serde_json::to_value(&server.env).expect("server env should serialize"));
        }
        mcp_servers.insert(server.name.clone(), Value::Object(config));
    }
    let config = json!({ "mcpServers": mcp_servers }).to_string();
    ensure_flag(args, "--strict-mcp-config", 0);
    ensure_flag_value(args, "--mcp-config", &config, 0);
    ensure_flag_value(args, "--permission-mode", "bypassPermissions", 0);
}

fn apply_codex_native_mcp_lockdown(
    args: &mut Vec<String>,
    transport: McpServerTransport<'_>,
    agent_id: &str,
    configured_servers: &[String],
    additional_servers: &[AdditionalMcpServer],
) {
    let additional_names: Vec<&str> = additional_servers.iter().map(|s| s.name.as_str()).collect();
    for server_name in configured_servers {
        if server_name.eq_ignore_ascii_case(agent_id) {
            continue;
        }
        if additional_names.iter().any(|n| n.eq_ignore_ascii_case(server_name)) {
            continue;
        }
        ensure_codex_config_override(args, &format!("mcp_servers.{server_name}.enabled"), "false");
    }

    let base = format!("mcp_servers.{agent_id}");
    match transport {
        McpServerTransport::Http(endpoint) => {
            ensure_codex_config_override(args, &format!("{base}.url"), &toml_string(endpoint));
        }
        McpServerTransport::Stdio { command, args: stdio_args } => {
            ensure_codex_config_override(args, &format!("{base}.command"), &toml_string(command));
            let toml_args =
                format!("[{}]", stdio_args.iter().map(|arg| toml_string(arg)).collect::<Vec<_>>().join(", "));
            ensure_codex_config_override(args, &format!("{base}.args"), &toml_args);
        }
    }
    ensure_codex_config_override(args, &format!("{base}.enabled"), "true");

    for server in additional_servers {
        let sbase = format!("mcp_servers.{}", server.name);
        ensure_codex_config_override(args, &format!("{sbase}.command"), &toml_string(&server.command));
        let toml_args =
            format!("[{}]", server.args.iter().map(|arg| toml_string(arg)).collect::<Vec<_>>().join(", "));
        ensure_codex_config_override(args, &format!("{sbase}.args"), &toml_args);
        for (key, value) in &server.env {
            ensure_codex_config_override(args, &format!("{sbase}.env.{key}"), &toml_string(value));
        }
        ensure_codex_config_override(args, &format!("{sbase}.enabled"), "true");
    }
}

fn apply_gemini_native_mcp_lockdown(
    args: &mut Vec<String>,
    env: &mut HashMap<String, String>,
    transport: McpServerTransport<'_>,
    agent_id: &str,
    run_id: &RunId,
    temp_cleanup: &mut TempPathCleanup,
    additional_servers: &[AdditionalMcpServer],
) -> Result<()> {
    let mut allowed_names = vec![agent_id.to_string()];
    for server in additional_servers {
        allowed_names.push(server.name.clone());
    }
    let allowed_csv = allowed_names.join(",");
    ensure_flag_value(args, "--allowed-mcp-server-names", &allowed_csv, 0);
    let primary = match transport {
        McpServerTransport::Http(endpoint) => json!({ "type": "http", "url": endpoint }),
        McpServerTransport::Stdio { command, args } => json!({
            "type": "stdio",
            "command": command,
            "args": args,
            "env": { "AO_MCP_SCHEMA_DRAFT": "draft07" }
        }),
    };
    let mut mcp_servers = serde_json::Map::new();
    mcp_servers.insert(agent_id.to_string(), primary);
    for server in additional_servers {
        let mut config = serde_json::Map::new();
        config.insert("type".to_string(), Value::String("stdio".to_string()));
        config.insert("command".to_string(), Value::String(server.command.clone()));
        config.insert("args".to_string(), serde_json::to_value(&server.args).expect("server args should serialize"));
        if !server.env.is_empty() {
            config.insert("env".to_string(), serde_json::to_value(&server.env).expect("server env should serialize"));
        }
        mcp_servers.insert(server.name.clone(), Value::Object(config));
    }
    let settings = json!({
        "tools": { "core": [] },
        "mcp": { "allowed": allowed_names, "excluded": [] },
        "mcpServers": mcp_servers
    });
    let settings_path = write_temp_json_file(run_id, "gemini-mcp", &settings)?;
    env.insert("GEMINI_CLI_SYSTEM_SETTINGS_PATH".to_string(), settings_path.to_string_lossy().to_string());
    temp_cleanup.track(settings_path);
    Ok(())
}

fn apply_opencode_native_mcp_lockdown(
    env: &mut HashMap<String, String>,
    transport: McpServerTransport<'_>,
    agent_id: &str,
    additional_servers: &[AdditionalMcpServer],
) {
    let primary = match transport {
        McpServerTransport::Http(endpoint) => json!({ "type": "remote", "url": endpoint, "enabled": true }),
        McpServerTransport::Stdio { command, args } => {
            let mut command_with_args = Vec::with_capacity(args.len() + 1);
            command_with_args.push(command.to_string());
            command_with_args.extend(args.iter().cloned());
            json!({ "type": "local", "command": command_with_args, "enabled": true })
        }
    };
    let mut mcp_entries = serde_json::Map::new();
    mcp_entries.insert(agent_id.to_string(), primary);
    for server in additional_servers {
        let mut command_with_args = Vec::with_capacity(server.args.len() + 1);
        command_with_args.push(server.command.clone());
        command_with_args.extend(server.args.iter().cloned());
        let mut config = serde_json::Map::new();
        config.insert("type".to_string(), Value::String("local".to_string()));
        config.insert("command".to_string(), serde_json::to_value(command_with_args).expect("server command should serialize"));
        config.insert("enabled".to_string(), Value::Bool(true));
        if !server.env.is_empty() {
            config.insert("env".to_string(), serde_json::to_value(&server.env).expect("server env should serialize"));
        }
        mcp_entries.insert(server.name.clone(), Value::Object(config));
    }
    let config = json!({ "mcp": mcp_entries });
    env.insert("OPENCODE_CONFIG_CONTENT".to_string(), config.to_string());
}

fn apply_oai_runner_native_mcp_lockdown(args: &mut Vec<String>, transport: McpServerTransport<'_>) {
    let config = match transport {
        McpServerTransport::Stdio { command, args: stdio_args } => {
            json!([{ "command": command, "args": stdio_args }])
        }
        McpServerTransport::Http(_) => return,
    };
    let insert_at = args.iter().position(|entry| entry == "run").map(|index| index + 1).unwrap_or(0);
    ensure_flag_value(args, "--mcp-config", &config.to_string(), insert_at);
}

pub fn apply_native_mcp_policy(
    invocation: &mut LaunchInvocation,
    enforcement: &McpToolEnforcement,
    env: &mut HashMap<String, String>,
    run_id: &RunId,
    temp_cleanup: &mut TempPathCleanup,
) -> Result<()> {
    if !enforcement.enabled {
        debug!(command = %invocation.command, "Native MCP policy disabled for CLI invocation");
        return Ok(());
    }

    let transport = resolve_mcp_server_transport(enforcement)?;
    let agent_id = enforcement.agent_id.trim();
    let cli = canonical_cli_name(&invocation.command);
    let skipped_additional_server_names = enforcement
        .additional_servers
        .iter()
        .filter(|server| server.name.eq_ignore_ascii_case(agent_id))
        .map(|server| server.name.clone())
        .collect::<Vec<_>>();
    if !skipped_additional_server_names.is_empty() {
        warn!(
            run_id = %run_id.0.as_str(),
            agent_id,
            skipped_additional_servers = ?skipped_additional_server_names,
            "Ignoring additional MCP servers that collide with the primary agent id"
        );
    }
    let additional = enforcement
        .additional_servers
        .iter()
        .filter(|server| !server.name.eq_ignore_ascii_case(agent_id))
        .cloned()
        .collect::<Vec<_>>();
    let additional_server_names = additional.iter().map(|server| server.name.as_str()).collect::<Vec<_>>();
    let (transport_kind, transport_endpoint, transport_command, transport_args) = summarize_mcp_transport(transport);

    info!(
        run_id = %run_id.0.as_str(),
        cli,
        command = %invocation.command,
        agent_id,
        transport_kind,
        transport_endpoint = ?transport_endpoint,
        transport_command = ?transport_command,
        transport_args = ?transport_args,
        additional_servers = ?additional_server_names,
        tool_policy_allow = ?enforcement.tool_policy_allow,
        tool_policy_deny = ?enforcement.tool_policy_deny,
        "Applying native MCP policy"
    );

    match cli.as_str() {
        "claude" => {
            apply_claude_native_mcp_lockdown(&mut invocation.args, transport, agent_id, &additional);
            info!(run_id = %run_id.0.as_str(), cli = "claude", "Applied Claude native MCP policy");
        }
        "codex" => {
            let configured_servers = discover_codex_mcp_server_names();
            apply_codex_native_mcp_lockdown(
                &mut invocation.args,
                transport,
                agent_id,
                &configured_servers,
                &additional,
            );
            info!(
                run_id = %run_id.0.as_str(),
                cli = "codex",
                configured_servers = ?configured_servers,
                "Applied Codex native MCP policy"
            );
        }
        "gemini" => {
            apply_gemini_native_mcp_lockdown(
                &mut invocation.args,
                env,
                transport,
                agent_id,
                run_id,
                temp_cleanup,
                &additional,
            )?;
            info!(run_id = %run_id.0.as_str(), cli = "gemini", "Applied Gemini native MCP policy");
        }
        "opencode" => {
            apply_opencode_native_mcp_lockdown(env, transport, agent_id, &additional);
            info!(run_id = %run_id.0.as_str(), cli = "opencode", "Applied OpenCode native MCP policy");
        }
        "ao-oai-runner" => {
            apply_oai_runner_native_mcp_lockdown(&mut invocation.args, transport);
            info!(run_id = %run_id.0.as_str(), cli = "ao-oai-runner", "Applied AO OAI runner native MCP policy");
        }
        _ => {
            bail!(
                "MCP-only policy enabled, but no native enforcement adapter exists for CLI command '{}'",
                invocation.command
            );
        }
    }

    Ok(())
}

// ── Session spawning ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn spawn_session_process(
    tool: &str,
    model: &str,
    prompt: &str,
    runtime_contract: Option<&Value>,
    cwd: &str,
    env: HashMap<String, String>,
    timeout_secs: Option<u64>,
    run_id: &RunId,
    event_tx: mpsc::Sender<AgentRunEvent>,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<i32> {
    let mut invocation = build_cli_invocation(tool, model, prompt, runtime_contract).await?;
    let mut env = env;
    merge_launch_env(&mut env, &invocation);
    debug!(
        run_id = %run_id.0.as_str(),
        tool,
        model,
        command = %invocation.command,
        args = ?invocation.args,
        prompt_via_stdin = invocation.prompt_via_stdin,
        "Built native session invocation from runtime contract"
    );
    let enforcement = resolve_mcp_tool_enforcement(runtime_contract);
    let mut temp_cleanup = TempPathCleanup::default();
    apply_native_mcp_policy(&mut invocation, &enforcement, &mut env, run_id, &mut temp_cleanup)?;
    let mcp_config_preview = flag_value(&invocation.args, "--mcp-config").map(|value| truncate_for_log(value, 240));
    info!(
        run_id = %run_id.0.as_str(),
        tool,
        model,
        command = %invocation.command,
        args = ?invocation.args,
        mcp_config_preview = ?mcp_config_preview,
        "Prepared native session invocation after MCP policy"
    );
    let session_request =
        build_session_request(tool, model, prompt, runtime_contract, cwd, env, timeout_secs, invocation)?;
    let idle_timeout_secs = resolve_idle_timeout_secs(tool, timeout_secs, runtime_contract);
    let resolver = SessionBackendResolver::new();
    let backend = resolver.resolve(&session_request);
    let mut run = backend.start_session(session_request).await.context("failed to start native session backend")?;
    let run_session_id = run.session_id.clone();
    let run_started_at = Instant::now();
    let mut last_activity_at = run_started_at;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut skipped_initial_heartbeat_tick = false;

    info!(
        run_id = %run_id.0.as_str(),
        tool,
        model,
        cwd,
        selected_backend = %run.selected_backend,
        idle_timeout_secs = ?idle_timeout_secs,
        "Spawning native session backend"
    );

    loop {
        tokio::select! {
            maybe_event = run.events.recv() => {
                let Some(event) = maybe_event else {
                    bail!("native session backend closed event stream unexpectedly");
                };

                if !matches!(event, SessionEvent::Started { .. }) {
                    last_activity_at = Instant::now();
                }

                if let Some(exit_code) = forward_session_event(run_id, &event, &event_tx).await {
                    return Ok(exit_code);
                }
            }
            _ = heartbeat.tick() => {
                if !skipped_initial_heartbeat_tick {
                    skipped_initial_heartbeat_tick = true;
                    continue;
                }

                let elapsed_secs = run_started_at.elapsed().as_secs();
                let idle_secs = last_activity_at.elapsed().as_secs();
                info!(
                    run_id = %run_id.0.as_str(),
                    elapsed_secs,
                    idle_secs,
                    idle_timeout_secs = ?idle_timeout_secs,
                    "Native session run heartbeat"
                );

                if let Some(idle_limit_secs) = idle_timeout_secs {
                    if idle_secs >= idle_limit_secs {
                        if let Some(session_id) = run_session_id.as_deref() {
                            let _ = backend.terminate_session(session_id).await;
                        }
                        bail!("Process idle timeout after {}s without activity", idle_limit_secs);
                    }
                }
            }
            _ = &mut cancel_rx => {
                if let Some(session_id) = run_session_id.as_deref() {
                    let _ = backend.terminate_session(session_id).await;
                }
                bail!("Process cancelled by user");
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_session_request(
    tool: &str,
    model: &str,
    prompt: &str,
    runtime_contract: Option<&Value>,
    cwd: &str,
    env: HashMap<String, String>,
    timeout_secs: Option<u64>,
    invocation: LaunchInvocation,
) -> Result<SessionRequest> {
    let mut merged_contract = runtime_contract.cloned().unwrap_or_else(|| json!({}));
    if !merged_contract.is_object() {
        merged_contract = json!({});
    }
    let mut merged_env = env;
    merge_launch_env(&mut merged_env, &invocation);

    if merged_contract.get("cli").and_then(Value::as_object).is_none() {
        merged_contract["cli"] = json!({});
    }
    merged_contract["cli"]["name"] = Value::String(tool.to_string());
    merged_contract["cli"]["launch"] = json!({
        "command": invocation.command,
        "args": invocation.args,
        "env": merged_env,
        "prompt_via_stdin": invocation.prompt_via_stdin,
    });
    let launch_args =
        merged_contract.pointer("/cli/launch/args").and_then(Value::as_array).cloned().unwrap_or_default();
    let mcp_config_preview = launch_args.iter().zip(launch_args.iter().skip(1)).find_map(|(flag, value)| {
        (flag.as_str() == Some("--mcp-config"))
            .then(|| value.as_str().map(|inner| truncate_for_log(inner, 240)))
            .flatten()
    });
    info!(
        tool,
        model,
        cwd,
        launch_args = ?launch_args,
        mcp_config_preview = ?mcp_config_preview,
        "Built native session request runtime contract launch"
    );

    Ok(SessionRequest {
        tool: tool.to_string(),
        model: model.to_string(),
        prompt: prompt.to_string(),
        cwd: std::path::PathBuf::from(cwd),
        project_root: None,
        mcp_endpoint: merged_contract.pointer("/mcp/endpoint").and_then(Value::as_str).map(ToString::to_string),
        permission_mode: None,
        timeout_secs,
        env_vars: merged_env.into_iter().collect(),
        extras: json!({ "runtime_contract": merged_contract }),
    })
}

async fn forward_session_event(
    run_id: &RunId,
    event: &SessionEvent,
    event_tx: &mpsc::Sender<AgentRunEvent>,
) -> Option<i32> {
    match event {
        SessionEvent::Started { backend, session_id } => {
            debug!(
                run_id = %run_id.0.as_str(),
                backend,
                session_id = ?session_id,
                "Native session backend started"
            );
            None
        }
        SessionEvent::TextDelta { text } | SessionEvent::FinalText { text } => {
            let _ = event_tx
                .send(AgentRunEvent::OutputChunk {
                    run_id: run_id.clone(),
                    stream_type: OutputStreamType::Stdout,
                    text: text.clone(),
                })
                .await;
            None
        }
        SessionEvent::ToolCall { tool_name, arguments, server } => {
            let mut parameters = arguments.clone();
            if let Some(server_name) = server {
                if let Some(obj) = parameters.as_object_mut() {
                    obj.insert("server".to_string(), Value::String(server_name.clone()));
                }
            }
            let _ = event_tx
                .send(AgentRunEvent::ToolCall {
                    run_id: run_id.clone(),
                    tool_info: ToolCallInfo { tool_name: tool_name.clone(), parameters, timestamp: Timestamp::now() },
                })
                .await;
            None
        }
        SessionEvent::ToolResult { tool_name, output, success } => {
            let _ = event_tx
                .send(AgentRunEvent::ToolResult {
                    run_id: run_id.clone(),
                    result_info: ToolResultInfo {
                        tool_name: tool_name.clone(),
                        result: output.clone(),
                        duration_ms: 0,
                        success: *success,
                    },
                })
                .await;
            None
        }
        SessionEvent::Thinking { text } => {
            let _ = event_tx.send(AgentRunEvent::Thinking { run_id: run_id.clone(), content: text.clone() }).await;
            None
        }
        SessionEvent::Artifact { artifact_id, metadata } => {
            let _ = event_tx
                .send(AgentRunEvent::Artifact {
                    run_id: run_id.clone(),
                    artifact_info: ArtifactInfo {
                        artifact_id: artifact_id.clone(),
                        artifact_type: ArtifactType::Other,
                        file_path: metadata.get("file_path").and_then(Value::as_str).map(ToString::to_string),
                        size_bytes: metadata.get("size_bytes").and_then(Value::as_u64),
                        mime_type: metadata.get("mime_type").and_then(Value::as_str).map(ToString::to_string),
                    },
                })
                .await;
            None
        }
        SessionEvent::Metadata { metadata } => {
            let tokens = tokens_from_metadata(metadata);
            if tokens.is_some() {
                let _ = event_tx.send(AgentRunEvent::Metadata { run_id: run_id.clone(), cost: None, tokens }).await;
            }
            None
        }
        SessionEvent::Error { message, recoverable } => {
            if *recoverable {
                let _ = event_tx
                    .send(AgentRunEvent::OutputChunk {
                        run_id: run_id.clone(),
                        stream_type: OutputStreamType::Stderr,
                        text: message.clone(),
                    })
                    .await;
            } else {
                let _ = event_tx.send(AgentRunEvent::Error { run_id: run_id.clone(), error: message.clone() }).await;
            }
            None
        }
        SessionEvent::Finished { exit_code } => Some(exit_code.unwrap_or(0)),
    }
}

fn tokens_from_metadata(metadata: &Value) -> Option<TokenUsage> {
    match metadata.get("type").and_then(Value::as_str) {
        Some("claude_usage") => {
            let usage = metadata.get("usage")?;
            Some(TokenUsage {
                input: usage.get("input_tokens")?.as_u64()? as u32,
                output: usage.get("output_tokens")?.as_u64()? as u32,
                reasoning: None,
                cache_read: usage
                    .get("cache_read_input_tokens")
                    .or_else(|| usage.get("cached_input_tokens"))
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                cache_write: usage.get("cache_creation_input_tokens").and_then(Value::as_u64).map(|value| value as u32),
            })
        }
        Some("codex_usage") => {
            let usage = metadata.get("usage")?;
            Some(TokenUsage {
                input: usage.get("input_tokens")?.as_u64()? as u32,
                output: usage.get("output_tokens")?.as_u64()? as u32,
                reasoning: None,
                cache_read: usage.get("cached_input_tokens").and_then(Value::as_u64).map(|value| value as u32),
                cache_write: None,
            })
        }
        Some("gemini_stats") => {
            let tokens = metadata
                .pointer("/stats/models")
                .and_then(Value::as_object)
                .and_then(|models| models.values().next())
                .and_then(|model| model.pointer("/tokens"))?;
            Some(TokenUsage {
                input: tokens.get("input")?.as_u64()? as u32,
                output: tokens.get("candidates").or_else(|| tokens.get("output")).and_then(Value::as_u64)? as u32,
                reasoning: tokens.get("thoughts").and_then(Value::as_u64).map(|value| value as u32),
                cache_read: tokens.get("cached").and_then(Value::as_u64).map(|value| value as u32),
                cache_write: None,
            })
        }
        _ => None,
    }
}
