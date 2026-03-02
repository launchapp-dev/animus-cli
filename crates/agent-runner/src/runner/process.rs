use anyhow::{bail, Context, Result};
use cli_wrapper::{ensure_codex_config_override, ensure_flag, ensure_flag_value, LaunchInvocation};
use protocol::{AgentRunEvent, OutputStreamType, RunId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration, MissedTickBehavior};
use tracing::{debug, info, warn};

use super::lifecycle::spawn_wait_task;
use super::process_builder::{build_cli_invocation, resolve_idle_timeout_secs};
use super::stream_bridge::spawn_stream_forwarders;
use crate::cleanup::{track_process, untrack_process};

fn truncate_for_log(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{}…", truncated)
}

#[derive(Debug, Clone)]
struct McpStdioConfig {
    command: String,
    args: Vec<String>,
}

#[derive(Debug, Clone)]
struct AdditionalMcpServer {
    name: String,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct McpToolEnforcement {
    enabled: bool,
    endpoint: Option<String>,
    stdio: Option<McpStdioConfig>,
    agent_id: String,
    allowed_prefixes: Vec<String>,
    tool_policy_allow: Vec<String>,
    tool_policy_deny: Vec<String>,
    additional_servers: Vec<AdditionalMcpServer>,
}

#[derive(Debug, Default)]
struct TempPathCleanup {
    paths: Vec<PathBuf>,
}

impl TempPathCleanup {
    fn track(&mut self, path: PathBuf) {
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

fn resolve_mcp_tool_enforcement(
    runtime_contract: Option<&serde_json::Value>,
) -> McpToolEnforcement {
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

    let supports_mcp = contract
        .pointer("/cli/capabilities/supports_mcp")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let endpoint = contract
        .pointer("/mcp/endpoint")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let stdio_command = contract
        .pointer("/mcp/stdio/command")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let stdio_args = contract
        .pointer("/mcp/stdio/args")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let stdio = stdio_command.map(|command| McpStdioConfig {
        command,
        args: stdio_args,
    });
    let has_endpoint = endpoint.is_some();
    let has_stdio = stdio.is_some();
    let agent_id = contract
        .pointer("/mcp/agent_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ao")
        .to_string();
    let explicit_enforce = contract
        .pointer("/mcp/enforce_only")
        .and_then(serde_json::Value::as_bool);
    let enabled = explicit_enforce.unwrap_or((has_endpoint || has_stdio) && supports_mcp);

    let mut allowed_prefixes = contract
        .pointer("/mcp/allowed_tool_prefixes")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
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
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
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
        .and_then(serde_json::Value::as_object)
        .map(|servers| {
            servers
                .iter()
                .map(|(name, entry)| AdditionalMcpServer {
                    name: name.clone(),
                    command: entry
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    args: entry
                        .get("args")
                        .and_then(serde_json::Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(ToString::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    env: entry
                        .get("env")
                        .and_then(serde_json::Value::as_object)
                        .map(|e| {
                            e.iter()
                                .filter_map(|(k, v)| {
                                    v.as_str().map(|val| (k.clone(), val.to_string()))
                                })
                                .collect()
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

fn canonical_cli_name(command: &str) -> String {
    let trimmed = command.trim();
    std::path::Path::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

fn toml_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn is_safe_codex_server_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn parse_codex_mcp_server_names(payload: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .map(|entries| {
            entries
                .into_iter()
                .filter_map(|entry| {
                    entry
                        .get("name")
                        .and_then(serde_json::Value::as_str)
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
    Stdio {
        command: &'a str,
        args: &'a [String],
    },
}

fn resolve_mcp_server_transport<'a>(
    enforcement: &'a McpToolEnforcement,
) -> Result<McpServerTransport<'a>> {
    if let Some(stdio) = enforcement.stdio.as_ref() {
        return Ok(McpServerTransport::Stdio {
            command: stdio.command.trim(),
            args: &stdio.args,
        });
    }
    if let Some(endpoint) = enforcement.endpoint.as_deref() {
        return Ok(McpServerTransport::Http(endpoint));
    }

    bail!(
        "MCP-only policy is enabled, but neither mcp.endpoint nor mcp.stdio.command is configured"
    );
}

fn sanitize_token_for_filename(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn write_temp_json_file(
    run_id: &RunId,
    prefix: &str,
    value: &serde_json::Value,
) -> Result<PathBuf> {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!(
        "ao-{prefix}-{}-{}-{now_nanos}.json",
        sanitize_token_for_filename(&run_id.0),
        std::process::id()
    ));
    let payload =
        serde_json::to_vec(value).context("Failed to serialize strict MCP config JSON")?;
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
        McpServerTransport::Http(endpoint) => serde_json::json!({
            "type": "http",
            "url": endpoint
        }),
        McpServerTransport::Stdio { command, args } => serde_json::json!({
            "command": command,
            "args": args
        }),
    };
    let mut mcp_servers = serde_json::Map::new();
    mcp_servers.insert(agent_id.to_string(), primary);
    for server in additional_servers {
        mcp_servers.insert(
            server.name.clone(),
            serde_json::json!({
                "command": server.command,
                "args": server.args
            }),
        );
    }
    let config = serde_json::json!({ "mcpServers": mcp_servers }).to_string();
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
        if additional_names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(server_name))
        {
            continue;
        }
        ensure_codex_config_override(args, &format!("mcp_servers.{server_name}.enabled"), "false");
    }

    let base = format!("mcp_servers.{agent_id}");
    match transport {
        McpServerTransport::Http(endpoint) => {
            ensure_codex_config_override(args, &format!("{base}.url"), &toml_string(endpoint));
        }
        McpServerTransport::Stdio {
            command,
            args: stdio_args,
        } => {
            ensure_codex_config_override(args, &format!("{base}.command"), &toml_string(command));
            let toml_args = format!(
                "[{}]",
                stdio_args
                    .iter()
                    .map(|arg| toml_string(arg))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            ensure_codex_config_override(args, &format!("{base}.args"), &toml_args);
        }
    }
    ensure_codex_config_override(args, &format!("{base}.enabled"), "true");

    for server in additional_servers {
        let sbase = format!("mcp_servers.{}", server.name);
        ensure_codex_config_override(args, &format!("{sbase}.command"), &toml_string(&server.command));
        let toml_args = format!(
            "[{}]",
            server
                .args
                .iter()
                .map(|arg| toml_string(arg))
                .collect::<Vec<_>>()
                .join(", ")
        );
        ensure_codex_config_override(args, &format!("{sbase}.args"), &toml_args);
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
        McpServerTransport::Http(endpoint) => serde_json::json!({
            "type": "http",
            "url": endpoint
        }),
        McpServerTransport::Stdio { command, args } => serde_json::json!({
            "type": "stdio",
            "command": command,
            "args": args,
            "env": {
                "AO_MCP_SCHEMA_DRAFT": "draft07"
            }
        }),
    };
    let mut mcp_servers = serde_json::Map::new();
    mcp_servers.insert(agent_id.to_string(), primary);
    for server in additional_servers {
        mcp_servers.insert(
            server.name.clone(),
            serde_json::json!({
                "type": "stdio",
                "command": server.command,
                "args": server.args,
            }),
        );
    }
    let settings = serde_json::json!({
        "tools": {
            "core": []
        },
        "mcp": {
            "allowed": allowed_names,
            "excluded": []
        },
        "mcpServers": mcp_servers
    });
    let settings_path = write_temp_json_file(run_id, "gemini-mcp", &settings)?;
    env.insert(
        "GEMINI_CLI_SYSTEM_SETTINGS_PATH".to_string(),
        settings_path.to_string_lossy().to_string(),
    );
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
        McpServerTransport::Http(endpoint) => serde_json::json!({
            "type": "remote",
            "url": endpoint,
            "enabled": true
        }),
        McpServerTransport::Stdio { command, args } => {
            let mut command_with_args = Vec::with_capacity(args.len() + 1);
            command_with_args.push(command.to_string());
            command_with_args.extend(args.iter().cloned());
            serde_json::json!({
                "type": "local",
                "command": command_with_args,
                "enabled": true
            })
        }
    };
    let mut mcp_entries = serde_json::Map::new();
    mcp_entries.insert(agent_id.to_string(), primary);
    for server in additional_servers {
        let mut command_with_args = Vec::with_capacity(server.args.len() + 1);
        command_with_args.push(server.command.clone());
        command_with_args.extend(server.args.iter().cloned());
        mcp_entries.insert(
            server.name.clone(),
            serde_json::json!({
                "type": "local",
                "command": command_with_args,
                "enabled": true
            }),
        );
    }
    let config = serde_json::json!({ "mcp": mcp_entries });
    env.insert("OPENCODE_CONFIG_CONTENT".to_string(), config.to_string());
}

fn apply_oai_runner_native_mcp_lockdown(
    args: &mut Vec<String>,
    transport: McpServerTransport<'_>,
) {
    let config = match transport {
        McpServerTransport::Stdio { command, args: stdio_args } => {
            serde_json::json!([{ "command": command, "args": stdio_args }])
        }
        McpServerTransport::Http(_) => return,
    };
    ensure_flag_value(args, "--mcp-config", &config.to_string(), 0);
}

fn apply_native_mcp_policy(
    invocation: &mut LaunchInvocation,
    enforcement: &McpToolEnforcement,
    env: &mut HashMap<String, String>,
    run_id: &RunId,
    temp_cleanup: &mut TempPathCleanup,
) -> Result<()> {
    if !enforcement.enabled {
        return Ok(());
    }

    let transport = resolve_mcp_server_transport(enforcement)?;
    let agent_id = enforcement.agent_id.trim();
    let cli = canonical_cli_name(&invocation.command);

    let additional = &enforcement.additional_servers;

    match cli.as_str() {
        "claude" => apply_claude_native_mcp_lockdown(
            &mut invocation.args,
            transport,
            agent_id,
            additional,
        ),
        "codex" => {
            let configured_servers = discover_codex_mcp_server_names();
            apply_codex_native_mcp_lockdown(
                &mut invocation.args,
                transport,
                agent_id,
                &configured_servers,
                additional,
            );
        }
        "gemini" => apply_gemini_native_mcp_lockdown(
            &mut invocation.args,
            env,
            transport,
            agent_id,
            run_id,
            temp_cleanup,
            additional,
        )?,
        "opencode" => apply_opencode_native_mcp_lockdown(env, transport, agent_id, additional),
        "ao-oai-runner" => {
            apply_oai_runner_native_mcp_lockdown(&mut invocation.args, transport);
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

fn tool_policy_glob_match(pattern: &str, value: &str) -> bool {
    fn inner(pat: &[u8], val: &[u8]) -> bool {
        match (pat.first(), val.first()) {
            (None, None) => true,
            (Some(b'*'), _) => {
                inner(&pat[1..], val) || (!val.is_empty() && inner(pat, &val[1..]))
            }
            (Some(&p), Some(&v)) if p == v => inner(&pat[1..], &val[1..]),
            _ => false,
        }
    }
    inner(pattern.as_bytes(), value.as_bytes())
}

fn is_tool_policy_permitted(tool_name: &str, enforcement: &McpToolEnforcement) -> bool {
    if enforcement.tool_policy_allow.is_empty() && enforcement.tool_policy_deny.is_empty() {
        return true;
    }
    let allowed = if enforcement.tool_policy_allow.is_empty() {
        true
    } else {
        enforcement
            .tool_policy_allow
            .iter()
            .any(|p| tool_policy_glob_match(p, tool_name))
    };
    if !allowed {
        return false;
    }
    !enforcement
        .tool_policy_deny
        .iter()
        .any(|p| tool_policy_glob_match(p, tool_name))
}

fn is_tool_call_allowed(
    tool_name: &str,
    parameters: &serde_json::Value,
    enforcement: &McpToolEnforcement,
) -> bool {
    if !enforcement.enabled {
        return true;
    }
    let normalized = tool_name.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    if matches!(normalized.as_str(), "phase_transition" | "phase-transition") {
        return true;
    }

    let is_mcp_discovery_helper = matches!(
        normalized.as_str(),
        "list_mcp_resources" | "list_mcp_resource_templates" | "read_mcp_resource"
    );

    // Codex stream-json MCP events include a plain tool name and a "server" field
    // inside arguments. Allow calls explicitly targeting the locked agent_id.
    let target_server = parameters
        .get("server")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    if let Some(server) = target_server {
        if server == enforcement.agent_id.to_ascii_lowercase() {
            return true;
        }
        // Codex emits MCP discovery helper calls on its own host server.
        if server == "codex" && is_mcp_discovery_helper {
            return true;
        }
        return false;
    }

    // These are MCP discovery helpers emitted by Codex itself; they do not map to a
    // concrete downstream AO tool name.
    if is_mcp_discovery_helper {
        return true;
    }

    let prefix_allowed = enforcement
        .allowed_prefixes
        .iter()
        .any(|prefix| normalized.starts_with(prefix));
    if !prefix_allowed {
        return false;
    }
    is_tool_policy_permitted(&normalized, enforcement)
}

// Keeping this explicit signature preserves current call sites across the
// runner orchestration path during the staged refactor. (2026-02-11)
#[allow(clippy::too_many_arguments)]
pub async fn spawn_cli_process(
    tool: &str,
    model: &str,
    prompt: &str,
    runtime_contract: Option<&serde_json::Value>,
    cwd: &str,
    env: HashMap<String, String>,
    timeout_secs: Option<u64>,
    run_id: &RunId,
    event_tx: mpsc::Sender<AgentRunEvent>,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<i32> {
    let mut invocation = build_cli_invocation(tool, model, prompt, runtime_contract).await?;
    let mut env = env;
    let hard_timeout_secs = timeout_secs.filter(|value| *value > 0);
    let idle_timeout_secs = resolve_idle_timeout_secs(tool, hard_timeout_secs, runtime_contract);
    let mcp_tool_enforcement = resolve_mcp_tool_enforcement(runtime_contract);
    let mut temp_cleanup = TempPathCleanup::default();
    apply_native_mcp_policy(
        &mut invocation,
        &mcp_tool_enforcement,
        &mut env,
        run_id,
        &mut temp_cleanup,
    )?;
    let prompt_len = prompt.chars().count();
    let prompt_preview = truncate_for_log(prompt, 160);

    info!(
        run_id = %run_id.0.as_str(),
        tool,
        model,
        cwd,
        command = %invocation.command,
        args = ?invocation.args,
        prompt_chars = prompt_len,
        prompt_via_stdin = invocation.prompt_via_stdin,
        has_runtime_contract = runtime_contract.is_some(),
        hard_timeout_secs = ?hard_timeout_secs,
        idle_timeout_secs = ?idle_timeout_secs,
        env_vars = env.len(),
        mcp_only = mcp_tool_enforcement.enabled,
        mcp_endpoint = ?mcp_tool_enforcement.endpoint,
        mcp_stdio_command = ?mcp_tool_enforcement
            .stdio
            .as_ref()
            .map(|config| config.command.as_str()),
        mcp_stdio_args = ?mcp_tool_enforcement
            .stdio
            .as_ref()
            .map(|config| config.args.as_slice()),
        mcp_agent_id = %mcp_tool_enforcement.agent_id,
        mcp_allowed_prefixes = ?mcp_tool_enforcement.allowed_prefixes,
        "Spawning CLI process"
    );
    debug!(
        run_id = %run_id.0.as_str(),
        prompt_preview = %prompt_preview,
        "CLI prompt preview (truncated)"
    );

    let mut command = Command::new(&invocation.command);
    command
        .args(&invocation.args)
        .current_dir(cwd)
        .envs(env)
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    command.process_group(0);

    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to spawn CLI process '{}'", invocation.command))?;

    // Always close stdin; only some CLIs should receive the prompt via stdin.
    if let Some(mut stdin) = child.stdin.take() {
        if invocation.prompt_via_stdin && !prompt.is_empty() {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = stdin.write_all(prompt.as_bytes()).await {
                warn!(
                    run_id = %run_id.0.as_str(),
                    command = %invocation.command,
                    error = %e,
                    "Failed to write prompt to stdin"
                );
            } else {
                debug!(
                    run_id = %run_id.0.as_str(),
                    command = %invocation.command,
                    bytes = prompt.len(),
                    "Wrote prompt payload to stdin"
                );
            }
        }
        drop(stdin);
    }

    let pid = child.id().context("Failed to get PID")?;
    info!(
        run_id = %run_id.0.as_str(),
        pid,
        command = %invocation.command,
        "CLI process spawned"
    );
    if let Err(e) = track_process(&run_id.0, pid) {
        warn!(
            run_id = %run_id.0.as_str(),
            pid,
            error = %e,
            "Failed to record process in orphan tracker"
        );
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::JobObjects::*;
        use windows::Win32::System::Threading::OpenProcess;

        unsafe {
            if let Ok(job) = CreateJobObjectW(None, None) {
                if let Ok(process_handle) = OpenProcess(
                    windows::Win32::System::Threading::PROCESS_SET_QUOTA
                        | windows::Win32::System::Threading::PROCESS_TERMINATE,
                    false,
                    pid,
                ) {
                    if AssignProcessToJobObject(job, process_handle).is_ok() {
                        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

                        if SetInformationJobObject(
                            job,
                            JobObjectExtendedLimitInformation,
                            &info as *const _ as *const _,
                            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                        )
                        .is_ok()
                        {
                            crate::cleanup::track_job(pid, job);
                        } else {
                            let _ = CloseHandle(job);
                        }
                    } else {
                        let _ = CloseHandle(job);
                    }
                    let _ = CloseHandle(process_handle);
                } else {
                    let _ = CloseHandle(job);
                }
            }
        }
    }

    let stdout = child.stdout.take().context("Failed to capture stdout")?;
    let stderr = child.stderr.take().context("Failed to capture stderr")?;

    let (output_tx, mut output_rx) = mpsc::channel::<AgentRunEvent>(100);
    let (wait_tx, mut wait_rx) = tokio::sync::oneshot::channel();

    spawn_stream_forwarders(stdout, stderr, run_id.clone(), output_tx.clone());

    drop(output_tx);

    spawn_wait_task(child, run_id.clone(), wait_tx);

    let run_id_for_select = run_id.clone();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let run_started_at = Instant::now();
    let mut last_activity_at = run_started_at;
    let mut output_chunks_total: u64 = 0;
    let mut output_chunks_stdout: u64 = 0;
    let mut output_chunks_stderr: u64 = 0;
    let mut skipped_initial_heartbeat_tick = false;
    let mcp_tool_enforcement_for_select = mcp_tool_enforcement.clone();

    let run_loop = async move {
        loop {
            tokio::select! {
                Some(evt) = output_rx.recv() => {
                    if let AgentRunEvent::ToolCall { tool_info, .. } = &evt {
                        if !is_tool_call_allowed(
                            &tool_info.tool_name,
                            &tool_info.parameters,
                            &mcp_tool_enforcement_for_select,
                        ) {
                            let server_context = tool_info
                                .parameters
                                .get("server")
                                .and_then(serde_json::Value::as_str)
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(ToString::to_string);
                            let policy = mcp_tool_enforcement_for_select.allowed_prefixes.join(", ");
                            let error = if let Some(server_name) = &server_context {
                                format!(
                                    "MCP-only policy violation: tool '{}' on server '{}' is not allowed (allowed prefixes: [{}], allowed server: '{}')",
                                    tool_info.tool_name,
                                    server_name,
                                    policy,
                                    mcp_tool_enforcement_for_select.agent_id
                                )
                            } else {
                                format!(
                                    "MCP-only policy violation: tool '{}' is not allowed (allowed prefixes: [{}])",
                                    tool_info.tool_name,
                                    policy
                                )
                            };
                            warn!(
                                run_id = %run_id_for_select.0.as_str(),
                                pid,
                                tool_name = %tool_info.tool_name,
                                tool_server = ?server_context,
                                allowed_prefixes = ?mcp_tool_enforcement_for_select.allowed_prefixes,
                                "Run emitted disallowed tool call under MCP-only policy"
                            );
                            let _ = event_tx.send(evt.clone()).await;
                            let _ = event_tx.send(AgentRunEvent::Error {
                                run_id: run_id_for_select.clone(),
                                error: error.clone(),
                            }).await;
                            let killed = crate::cleanup::kill_process(pid as i32);
                            if !killed {
                                warn!(
                                    run_id = %run_id_for_select.0.as_str(),
                                    pid,
                                    "Failed to terminate process after MCP-only violation"
                                );
                            }
                            if let Err(track_error) = untrack_process(&run_id_for_select.0) {
                                warn!(
                                    run_id = %run_id_for_select.0.as_str(),
                                    pid,
                                    error = %track_error,
                                    "Failed to remove process from orphan tracker after MCP-only violation"
                                );
                            }
                            #[cfg(windows)]
                            crate::cleanup::untrack_job(pid);
                            bail!("{error}");
                        }
                    }
                    if let AgentRunEvent::OutputChunk { stream_type, text, .. } = &evt {
                        output_chunks_total += 1;
                        match stream_type {
                            OutputStreamType::Stdout => output_chunks_stdout += 1,
                            OutputStreamType::Stderr => output_chunks_stderr += 1,
                            OutputStreamType::System => {}
                        }
                        if output_chunks_total == 1 {
                            info!(
                                run_id = %run_id_for_select.0.as_str(),
                                pid,
                                stream = ?stream_type,
                                preview = %truncate_for_log(text, 200),
                                "Received first CLI output chunk"
                            );
                        }
                    }
                    last_activity_at = Instant::now();
                    let _ = event_tx.send(evt).await;
                }
                _ = heartbeat.tick() => {
                    if !skipped_initial_heartbeat_tick {
                        skipped_initial_heartbeat_tick = true;
                        continue;
                    }

                    let elapsed_secs = run_started_at.elapsed().as_secs();
                    let idle_secs = last_activity_at.elapsed().as_secs();
                    info!(
                        run_id = %run_id_for_select.0.as_str(),
                        pid,
                        elapsed_secs,
                        idle_secs,
                        output_chunks_total,
                        output_chunks_stdout,
                        output_chunks_stderr,
                        idle_timeout_secs = ?idle_timeout_secs,
                        "CLI run heartbeat"
                    );

                    if let Some(idle_limit_secs) = idle_timeout_secs {
                        if idle_secs >= idle_limit_secs {
                            warn!(
                                run_id = %run_id_for_select.0.as_str(),
                                pid,
                                idle_secs,
                                idle_limit_secs,
                                output_chunks_total,
                                "CLI run exceeded idle timeout; terminating process group"
                            );
                            let killed = crate::cleanup::kill_process(pid as i32);
                            if !killed {
                                warn!(run_id = %run_id_for_select.0.as_str(), pid, "Failed to terminate idle-timed-out process");
                            }
                            if let Err(e) = untrack_process(&run_id_for_select.0) {
                                warn!(
                                    run_id = %run_id_for_select.0.as_str(),
                                    pid,
                                    error = %e,
                                    "Failed to remove process from orphan tracker after idle timeout"
                                );
                            }
                            #[cfg(windows)]
                            crate::cleanup::untrack_job(pid);
                            bail!("Process idle timeout after {}s without activity", idle_limit_secs);
                        }
                    }
                }
                _ = &mut cancel_rx => {
                    warn!(
                        run_id = %run_id_for_select.0.as_str(),
                        pid,
                        "Process cancelled by caller; terminating process group"
                    );
                    let killed = crate::cleanup::kill_process(pid as i32);
                    if !killed {
                        warn!(run_id = %run_id_for_select.0.as_str(), pid, "Failed to terminate cancelled process");
                    }
                    if let Err(e) = untrack_process(&run_id_for_select.0) {
                        warn!(run_id = %run_id_for_select.0.as_str(), pid, error = %e, "Failed to remove process from orphan tracker");
                    }
                    #[cfg(windows)]
                    crate::cleanup::untrack_job(pid);
                    bail!("Process cancelled by user");
                }
                result = &mut wait_rx => {
                    while let Some(evt) = output_rx.recv().await {
                        let _ = event_tx.send(evt).await;
                    }
                    return match result {
                        Ok(wait_result) => wait_result.map_err(anyhow::Error::from),
                        Err(_) => Err(anyhow::anyhow!("Wait task failed")),
                    };
                }
            }
        }
    };

    let status: std::process::ExitStatus = match hard_timeout_secs {
        Some(timeout_secs) => {
            let timeout_duration = Duration::from_secs(timeout_secs);
            match timeout(timeout_duration, run_loop).await {
                Ok(Ok(status)) => status,
                Ok(Err(e)) => {
                    warn!(
                        run_id = %run_id.0.as_str(),
                        pid,
                        error = %e,
                        "CLI process execution returned an error"
                    );
                    if let Err(untrack_err) = untrack_process(&run_id.0) {
                        warn!(
                            run_id = %run_id.0.as_str(),
                            pid,
                            error = %untrack_err,
                            "Failed to remove process from orphan tracker after execution error"
                        );
                    }
                    return Err(e);
                }
                Err(_) => {
                    warn!(
                        run_id = %run_id.0.as_str(),
                        pid,
                        timeout_secs,
                        "CLI process timed out; terminating process group"
                    );
                    let killed = crate::cleanup::kill_process(pid as i32);
                    if !killed {
                        warn!(run_id = %run_id.0.as_str(), pid, "Failed to terminate timed-out process");
                    }
                    if let Err(e) = untrack_process(&run_id.0) {
                        warn!(
                            run_id = %run_id.0.as_str(),
                            pid,
                            error = %e,
                            "Failed to remove process from orphan tracker after timeout"
                        );
                    }
                    #[cfg(windows)]
                    crate::cleanup::untrack_job(pid);
                    bail!("Process timed out");
                }
            }
        }
        None => match run_loop.await {
            Ok(status) => status,
            Err(e) => {
                warn!(
                    run_id = %run_id.0.as_str(),
                    pid,
                    error = %e,
                    "CLI process execution returned an error"
                );
                if let Err(untrack_err) = untrack_process(&run_id.0) {
                    warn!(
                        run_id = %run_id.0.as_str(),
                        pid,
                        error = %untrack_err,
                        "Failed to remove process from orphan tracker after execution error"
                    );
                }
                return Err(e);
            }
        },
    };

    if let Err(e) = untrack_process(&run_id.0) {
        warn!(
            run_id = %run_id.0.as_str(),
            pid,
            error = %e,
            "Failed to remove process from orphan tracker after completion"
        );
    }

    #[cfg(windows)]
    crate::cleanup::untrack_job(pid);

    let exit_code = status.code().unwrap_or(-1);
    info!(run_id = %run_id.0.as_str(), pid, exit_code, "CLI process completed");
    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mcp_tool_enforcement_defaults_to_ao_prefix_when_endpoint_is_set() {
        let contract = json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": { "endpoint": "http://127.0.0.1:3101/mcp/ao" }
        });
        let enforcement = resolve_mcp_tool_enforcement(Some(&contract));
        assert!(enforcement.enabled);
        assert_eq!(
            enforcement.endpoint.as_deref(),
            Some("http://127.0.0.1:3101/mcp/ao")
        );
        assert_eq!(enforcement.agent_id, "ao");
        assert!(enforcement
            .allowed_prefixes
            .iter()
            .any(|prefix| prefix == "ao."));
        assert!(enforcement
            .allowed_prefixes
            .iter()
            .any(|prefix| prefix == "mcp__ao__"));
    }

    #[test]
    fn mcp_tool_enforcement_rejects_non_matching_tool_calls() {
        let contract = json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": {
                "endpoint": "http://127.0.0.1:3101/mcp/ao",
                "enforce_only": true,
                "allowed_tool_prefixes": ["ao."]
            }
        });
        let enforcement = resolve_mcp_tool_enforcement(Some(&contract));
        assert!(is_tool_call_allowed(
            "ao.task.list",
            &json!({}),
            &enforcement
        ));
        assert!(is_tool_call_allowed(
            "phase_transition",
            &json!({}),
            &enforcement
        ));
        assert!(!is_tool_call_allowed("Bash", &json!({}), &enforcement));
        assert!(!is_tool_call_allowed(
            "stories-search",
            &json!({ "server": "shortcut" }),
            &enforcement
        ));
        assert!(is_tool_call_allowed(
            "requirements-get",
            &json!({ "server": "ao" }),
            &enforcement
        ));
        assert!(is_tool_call_allowed(
            "list_mcp_resources",
            &json!({}),
            &enforcement
        ));
        assert!(is_tool_call_allowed(
            "list_mcp_resources",
            &json!({ "server": "codex" }),
            &enforcement
        ));
    }

    #[test]
    fn native_mcp_policy_rejects_unknown_cli_when_enforced() {
        let mut invocation = LaunchInvocation {
            command: "unknown-cli".to_string(),
            args: vec!["hello".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: None,
            stdio: Some(McpStdioConfig {
                command: "/Users/samishukri/ao-cli/target/debug/ao".to_string(),
                args: vec![
                    "--project-root".to_string(),
                    "/Users/samishukri/ao-cli".to_string(),
                    "mcp".to_string(),
                    "serve".to_string(),
                ],
            }),
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
            tool_policy_allow: Vec::new(),
            tool_policy_deny: Vec::new(),
            additional_servers: Vec::new(),
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-1".to_string());

        let err = apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect_err("unknown provider should fail closed");

        assert!(err.to_string().contains("no native enforcement adapter"));
    }

    #[test]
    fn native_mcp_policy_requires_transport_when_enforced() {
        let mut invocation = LaunchInvocation {
            command: "claude".to_string(),
            args: vec!["--print".to_string(), "hello".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: None,
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
            tool_policy_allow: Vec::new(),
            tool_policy_deny: Vec::new(),
            additional_servers: Vec::new(),
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-1b".to_string());

        let err = apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect_err("missing transport should fail closed");

        assert!(err
            .to_string()
            .contains("neither mcp.endpoint nor mcp.stdio.command"));
    }

    #[test]
    fn native_mcp_policy_adds_codex_mcp_server_override() {
        let mut invocation = LaunchInvocation {
            command: "codex".to_string(),
            args: vec![
                "exec".to_string(),
                "--json".to_string(),
                "hello".to_string(),
            ],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: Some("http://127.0.0.1:3101/mcp/ao".to_string()),
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
            tool_policy_allow: Vec::new(),
            tool_policy_deny: Vec::new(),
            additional_servers: Vec::new(),
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-2".to_string());

        apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect("codex policy should apply");

        let joined = invocation.args.join(" ");
        assert!(joined.contains("mcp_servers.ao.url=\"http://127.0.0.1:3101/mcp/ao\""));
    }

    #[test]
    fn native_mcp_policy_configures_claude_permission_mode() {
        let mut invocation = LaunchInvocation {
            command: "claude".to_string(),
            args: vec!["--print".to_string(), "hello".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: Some("http://127.0.0.1:3101/mcp/ao".to_string()),
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
            tool_policy_allow: Vec::new(),
            tool_policy_deny: Vec::new(),
            additional_servers: Vec::new(),
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-claude".to_string());

        apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect("claude policy should apply");

        assert!(invocation
            .args
            .windows(2)
            .any(|pair| { pair[0] == "--permission-mode" && pair[1] == "bypassPermissions" }));
        assert!(invocation
            .args
            .iter()
            .any(|arg| arg == "--strict-mcp-config"));
        assert!(!invocation.args.iter().any(|arg| arg == "--tools"));
    }

    #[test]
    fn parse_codex_mcp_server_names_extracts_safe_names() {
        let payload = r#"
            [
              {"name":"ao"},
              {"name":"shortcut"},
              {"name":"bad.name"},
              {"name":"with space"}
            ]
        "#;
        assert_eq!(
            parse_codex_mcp_server_names(payload),
            vec!["ao".to_string(), "shortcut".to_string()]
        );
    }

    #[test]
    fn codex_native_lockdown_disables_non_target_servers() {
        let mut args = vec![
            "exec".to_string(),
            "--json".to_string(),
            "hello".to_string(),
        ];
        let configured_servers = vec!["shortcut".to_string(), "ao".to_string()];

        apply_codex_native_mcp_lockdown(
            &mut args,
            McpServerTransport::Http("http://127.0.0.1:3101/mcp/ao"),
            "ao",
            &configured_servers,
            &[],
        );

        let joined = args.join(" ");
        assert!(joined.contains("mcp_servers.shortcut.enabled=false"));
        assert!(joined.contains("mcp_servers.ao.url=\"http://127.0.0.1:3101/mcp/ao\""));
        assert!(!joined.contains("mcp_servers.ao.enabled=false"));
    }

    #[test]
    fn codex_native_lockdown_sets_stdio_transport_when_configured() {
        let mut args = vec![
            "exec".to_string(),
            "--json".to_string(),
            "hello".to_string(),
        ];

        apply_codex_native_mcp_lockdown(
            &mut args,
            McpServerTransport::Stdio {
                command: "/Users/samishukri/ao-cli/target/debug/ao",
                args: &[
                    "--project-root".to_string(),
                    "/Users/samishukri/ao-cli".to_string(),
                    "mcp".to_string(),
                    "serve".to_string(),
                ],
            },
            "ao",
            &[],
            &[],
        );

        let joined = args.join(" ");
        assert!(
            joined.contains("mcp_servers.ao.command=\"/Users/samishukri/ao-cli/target/debug/ao\"")
        );
        assert!(joined.contains(
            "mcp_servers.ao.args=[\"--project-root\", \"/Users/samishukri/ao-cli\", \"mcp\", \"serve\"]"
        ));
        assert!(joined.contains("mcp_servers.ao.enabled=true"));
    }

    #[test]
    fn native_mcp_policy_sets_gemini_system_settings_path_for_stdio_transport() {
        let mut invocation = LaunchInvocation {
            command: "gemini".to_string(),
            args: vec!["--output-format".to_string(), "json".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: None,
            stdio: Some(McpStdioConfig {
                command: "/Users/samishukri/ao-cli/target/debug/ao".to_string(),
                args: vec![
                    "--project-root".to_string(),
                    "/Users/samishukri/ao-cli".to_string(),
                    "mcp".to_string(),
                    "serve".to_string(),
                ],
            }),
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
            tool_policy_allow: Vec::new(),
            tool_policy_deny: Vec::new(),
            additional_servers: Vec::new(),
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-3".to_string());

        apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect("gemini policy should apply");

        let settings_path = env
            .get("GEMINI_CLI_SYSTEM_SETTINGS_PATH")
            .expect("gemini settings path should be set")
            .to_string();
        assert!(invocation
            .args
            .windows(2)
            .any(|pair| pair[0] == "--allowed-mcp-server-names" && pair[1] == "ao"));
        let settings = std::fs::read_to_string(&settings_path).expect("read gemini settings");
        assert!(
            settings.contains("\"AO_MCP_SCHEMA_DRAFT\":\"draft07\""),
            "expected draft07 env in gemini settings, got: {settings}"
        );
        assert!(
            settings.contains("\"type\":\"stdio\""),
            "expected stdio transport in gemini settings, got: {settings}"
        );
    }

    #[test]
    fn native_mcp_policy_sets_gemini_http_settings_without_schema_override() {
        let mut invocation = LaunchInvocation {
            command: "gemini".to_string(),
            args: vec!["--output-format".to_string(), "json".to_string()],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: Some("http://127.0.0.1:3101/mcp/ao".to_string()),
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
            tool_policy_allow: Vec::new(),
            tool_policy_deny: Vec::new(),
            additional_servers: Vec::new(),
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-3-http".to_string());

        apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect("gemini policy should apply");

        let settings_path = env
            .get("GEMINI_CLI_SYSTEM_SETTINGS_PATH")
            .expect("gemini settings path should be set")
            .to_string();
        let settings = std::fs::read_to_string(&settings_path).expect("read gemini settings");
        assert!(
            settings.contains("\"type\":\"http\""),
            "expected http transport in gemini settings, got: {settings}"
        );
        assert!(
            settings.contains("\"url\":\"http://127.0.0.1:3101/mcp/ao\""),
            "expected ao endpoint in gemini settings, got: {settings}"
        );
        assert!(
            !settings.contains("\"AO_MCP_SCHEMA_DRAFT\""),
            "did not expect schema override env for gemini http transport, got: {settings}"
        );
    }

    #[test]
    fn native_mcp_policy_sets_opencode_local_mcp_command_array() {
        let mut invocation = LaunchInvocation {
            command: "opencode".to_string(),
            args: vec![
                "run".to_string(),
                "--format".to_string(),
                "json".to_string(),
            ],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: None,
            stdio: Some(McpStdioConfig {
                command: "/Users/samishukri/ao-cli/target/debug/ao".to_string(),
                args: vec![
                    "--project-root".to_string(),
                    "/Users/samishukri/ao-cli".to_string(),
                    "mcp".to_string(),
                    "serve".to_string(),
                ],
            }),
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string()],
            tool_policy_allow: Vec::new(),
            tool_policy_deny: Vec::new(),
            additional_servers: Vec::new(),
        };
        let mut env = HashMap::new();
        let mut cleanup = TempPathCleanup::default();
        let run_id = RunId("run-opencode".to_string());

        apply_native_mcp_policy(
            &mut invocation,
            &enforcement,
            &mut env,
            &run_id,
            &mut cleanup,
        )
        .expect("opencode policy should apply");

        let config_raw = env
            .get("OPENCODE_CONFIG_CONTENT")
            .expect("opencode config should be provided");
        let parsed: serde_json::Value =
            serde_json::from_str(config_raw).expect("opencode config should be valid JSON");
        assert_eq!(
            parsed
                .pointer("/mcp/ao/type")
                .and_then(serde_json::Value::as_str),
            Some("local")
        );
        assert_eq!(
            parsed
                .pointer("/mcp/ao/command/0")
                .and_then(serde_json::Value::as_str),
            Some("/Users/samishukri/ao-cli/target/debug/ao")
        );
        assert_eq!(
            parsed
                .pointer("/mcp/ao/command/4")
                .and_then(serde_json::Value::as_str),
            Some("serve")
        );
        assert!(parsed.pointer("/mcp/ao/args").is_none());
    }

    fn enforcement_with_tool_policy(allow: Vec<&str>, deny: Vec<&str>) -> McpToolEnforcement {
        McpToolEnforcement {
            enabled: true,
            endpoint: Some("http://127.0.0.1:3101/mcp/ao".to_string()),
            stdio: None,
            agent_id: "ao".to_string(),
            allowed_prefixes: vec!["ao.".to_string(), "mcp__ao__".to_string()],
            tool_policy_allow: allow.into_iter().map(ToString::to_string).collect(),
            tool_policy_deny: deny.into_iter().map(ToString::to_string).collect(),
            additional_servers: Vec::new(),
        }
    }

    #[test]
    fn tool_policy_empty_permits_all_prefixed_tools() {
        let enforcement = enforcement_with_tool_policy(vec![], vec![]);
        assert!(is_tool_call_allowed("ao.task.list", &serde_json::json!({}), &enforcement));
        assert!(is_tool_call_allowed("ao.daemon.start", &serde_json::json!({}), &enforcement));
    }

    #[test]
    fn tool_policy_allowlist_restricts_to_matching() {
        let enforcement = enforcement_with_tool_policy(vec!["ao.task.*"], vec![]);
        assert!(is_tool_call_allowed("ao.task.list", &serde_json::json!({}), &enforcement));
        assert!(is_tool_call_allowed("ao.task.get", &serde_json::json!({}), &enforcement));
        assert!(!is_tool_call_allowed("ao.daemon.start", &serde_json::json!({}), &enforcement));
    }

    #[test]
    fn tool_policy_denylist_blocks_matching() {
        let enforcement = enforcement_with_tool_policy(vec![], vec!["ao.daemon.*"]);
        assert!(is_tool_call_allowed("ao.task.list", &serde_json::json!({}), &enforcement));
        assert!(!is_tool_call_allowed("ao.daemon.start", &serde_json::json!({}), &enforcement));
        assert!(!is_tool_call_allowed("ao.daemon.stop", &serde_json::json!({}), &enforcement));
    }

    #[test]
    fn tool_policy_deny_overrides_allow() {
        let enforcement = enforcement_with_tool_policy(vec!["ao.*"], vec!["ao.daemon.*"]);
        assert!(is_tool_call_allowed("ao.task.list", &serde_json::json!({}), &enforcement));
        assert!(!is_tool_call_allowed("ao.daemon.start", &serde_json::json!({}), &enforcement));
    }

    #[test]
    fn tool_policy_does_not_affect_phase_transition() {
        let enforcement = enforcement_with_tool_policy(vec!["ao.task.*"], vec![]);
        assert!(is_tool_call_allowed("phase_transition", &serde_json::json!({}), &enforcement));
    }

    #[test]
    fn tool_policy_glob_match_basics() {
        assert!(tool_policy_glob_match("ao.*", "ao.task"));
        assert!(tool_policy_glob_match("ao.task.*", "ao.task.list"));
        assert!(tool_policy_glob_match("*", "anything"));
        assert!(!tool_policy_glob_match("ao.task.*", "ao.daemon.start"));
        assert!(tool_policy_glob_match("ao.task.list", "ao.task.list"));
        assert!(!tool_policy_glob_match("ao.task.list", "ao.task.get"));
    }

    #[test]
    fn resolve_enforcement_parses_tool_policy_from_contract() {
        let contract = serde_json::json!({
            "cli": {
                "name": "claude",
                "capabilities": { "supports_mcp": true, "supports_tool_use": true },
                "launch": { "args": ["--print", "hello"] }
            },
            "mcp": {
                "endpoint": "http://127.0.0.1:3101/mcp/ao",
                "agent_id": "ao",
                "tool_policy": {
                    "allow": ["ao.task.*", "ao.workflow.*"],
                    "deny": ["ao.task.delete"]
                }
            }
        });
        let enforcement = resolve_mcp_tool_enforcement(Some(&contract));
        assert_eq!(enforcement.tool_policy_allow, vec!["ao.task.*", "ao.workflow.*"]);
        assert_eq!(enforcement.tool_policy_deny, vec!["ao.task.delete"]);
        assert!(is_tool_call_allowed("ao.task.list", &serde_json::json!({}), &enforcement));
        assert!(!is_tool_call_allowed("ao.task.delete", &serde_json::json!({}), &enforcement));
        assert!(!is_tool_call_allowed("ao.daemon.start", &serde_json::json!({}), &enforcement));
    }

    #[test]
    fn resolve_enforcement_parses_additional_servers() {
        let contract = serde_json::json!({
            "cli": {
                "name": "claude",
                "capabilities": { "supports_mcp": true, "supports_tool_use": true },
                "launch": { "args": ["--print", "hello"] }
            },
            "mcp": {
                "endpoint": "http://127.0.0.1:3101/mcp/ao",
                "agent_id": "ao",
                "additional_servers": {
                    "my-db": {
                        "command": "/usr/local/bin/db-mcp",
                        "args": ["--port", "5432"],
                        "env": { "DB_HOST": "localhost" }
                    }
                }
            }
        });
        let enforcement = resolve_mcp_tool_enforcement(Some(&contract));
        assert_eq!(enforcement.additional_servers.len(), 1);
        assert_eq!(enforcement.additional_servers[0].name, "my-db");
        assert_eq!(enforcement.additional_servers[0].command, "/usr/local/bin/db-mcp");
        assert_eq!(enforcement.additional_servers[0].args, vec!["--port", "5432"]);
        assert_eq!(
            enforcement.additional_servers[0].env.get("DB_HOST").map(String::as_str),
            Some("localhost")
        );
    }

    #[test]
    fn claude_lockdown_includes_additional_servers() {
        let mut args = vec!["--print".to_string(), "hello".to_string()];
        let additional = vec![AdditionalMcpServer {
            name: "my-db".to_string(),
            command: "/usr/local/bin/db-mcp".to_string(),
            args: vec!["--port".to_string(), "5432".to_string()],
            env: HashMap::new(),
        }];
        apply_claude_native_mcp_lockdown(
            &mut args,
            McpServerTransport::Stdio {
                command: "/usr/local/bin/ao",
                args: &["mcp".to_string(), "serve".to_string()],
            },
            "ao",
            &additional,
        );
        let joined = args.join(" ");
        assert!(joined.contains("mcpServers"));
        let mcp_config_idx = args.iter().position(|a| a == "--mcp-config").unwrap();
        let config_json: serde_json::Value =
            serde_json::from_str(&args[mcp_config_idx + 1]).unwrap();
        assert!(config_json.pointer("/mcpServers/ao").is_some());
        assert!(config_json.pointer("/mcpServers/my-db").is_some());
        assert_eq!(
            config_json
                .pointer("/mcpServers/my-db/command")
                .and_then(serde_json::Value::as_str),
            Some("/usr/local/bin/db-mcp")
        );
    }
}
