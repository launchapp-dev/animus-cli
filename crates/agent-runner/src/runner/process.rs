use anyhow::{bail, Context, Result};
use protocol::{AgentRunEvent, OutputStreamType, RunId};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpToolPolicyMode {
    Allowlist,
    Blocklist,
}

#[derive(Debug, Clone)]
struct McpToolEnforcement {
    enabled: bool,
    endpoint: Option<String>,
    stdio: Option<McpStdioConfig>,
    agent_id: String,
    allowed_prefixes: Vec<String>,
    blocked_prefixes: Vec<String>,
    tool_policy_mode: Option<McpToolPolicyMode>,
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

fn default_allowed_tool_prefixes(agent_id: &str) -> Vec<String> {
    let normalized = agent_id.trim().to_ascii_lowercase();
    let mut prefixes = vec![
        "ao.".to_string(),
        "mcp__ao__".to_string(),
        "mcp.ao.".to_string(),
        format!("{normalized}."),
        format!("mcp__{normalized}__"),
        format!("mcp.{normalized}."),
    ];

    let snake = normalized.replace('-', "_");
    prefixes.push(format!("{snake}."));
    prefixes.push(format!("mcp__{snake}__"));
    prefixes.push(format!("mcp.{snake}."));

    prefixes.sort();
    prefixes.dedup();
    prefixes
}

fn parse_tool_policy_mode(raw: Option<&str>) -> Option<McpToolPolicyMode> {
    let value = raw
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())?
        .to_ascii_lowercase();
    match value.as_str() {
        "allowlist" => Some(McpToolPolicyMode::Allowlist),
        "blocklist" => Some(McpToolPolicyMode::Blocklist),
        _ => None,
    }
}

fn normalized_tool_prefixes(values: Option<&serde_json::Value>) -> Vec<String> {
    values
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn tool_policy_patterns_to_prefixes(values: Option<&serde_json::Value>) -> Vec<String> {
    normalized_tool_prefixes(values)
        .into_iter()
        .filter_map(|pattern| {
            if pattern.is_empty() {
                return None;
            }
            let prefix = pattern
                .split_once('*')
                .map(|(head, _)| head)
                .unwrap_or(pattern.as_str())
                .trim()
                .to_string();
            if prefix.is_empty() {
                None
            } else {
                Some(prefix)
            }
        })
        .collect()
}

fn normalize_server_prefixed_tool_name(server: &str, tool_name: &str) -> String {
    let normalized_server = server.trim().to_ascii_lowercase();
    let normalized_tool = tool_name.trim().to_ascii_lowercase();
    let dotted_tool = normalized_tool.replace(['-', '_'], ".");
    format!("{normalized_server}.{dotted_tool}")
}

fn tool_name_candidates(tool_name: &str, server: Option<&str>) -> Vec<String> {
    let normalized = tool_name.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut candidates = vec![normalized.clone(), normalized.replace(['-', '_'], ".")];

    if let Some(server) = server
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        let server_prefixed = normalize_server_prefixed_tool_name(&server, &normalized);
        let snake_tool = normalized.replace(['.', '-'], "_");
        candidates.push(server_prefixed.clone());
        candidates.push(format!("mcp.{server_prefixed}"));
        candidates.push(format!("mcp__{server}__{snake_tool}"));
    }

    candidates.sort();
    candidates.dedup();
    candidates
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
            blocked_prefixes: Vec::new(),
            tool_policy_mode: None,
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

    let tool_policy_mode = parse_tool_policy_mode(
        contract
            .pointer("/mcp/tool_policy/mode")
            .and_then(serde_json::Value::as_str),
    );
    let tool_policy_prefixes =
        tool_policy_patterns_to_prefixes(contract.pointer("/mcp/tool_policy/patterns"));
    let mut allowed_prefixes =
        normalized_tool_prefixes(contract.pointer("/mcp/allowed_tool_prefixes"));
    let mut blocked_prefixes =
        normalized_tool_prefixes(contract.pointer("/mcp/blocked_tool_prefixes"));

    if matches!(tool_policy_mode, Some(McpToolPolicyMode::Allowlist)) {
        allowed_prefixes = tool_policy_prefixes;
    } else if matches!(tool_policy_mode, Some(McpToolPolicyMode::Blocklist))
        && blocked_prefixes.is_empty()
    {
        blocked_prefixes = tool_policy_prefixes;
    }

    if enabled && allowed_prefixes.is_empty() && tool_policy_mode.is_none() {
        allowed_prefixes = default_allowed_tool_prefixes(&agent_id);
    }

    McpToolEnforcement {
        enabled,
        endpoint,
        stdio,
        agent_id,
        allowed_prefixes,
        blocked_prefixes,
        tool_policy_mode,
    }
}

fn canonical_cli_name(command: &str) -> String {
    let trimmed = command.trim();
    Path::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

fn ensure_flag(args: &mut Vec<String>, flag: &str, insert_at: usize) {
    if args.iter().any(|value| value == flag) {
        return;
    }
    args.insert(insert_at.min(args.len()), flag.to_string());
}

fn ensure_flag_value(args: &mut Vec<String>, flag: &str, value: &str, insert_at: usize) {
    if let Some(index) = args.iter().position(|entry| entry == flag) {
        if index + 1 < args.len() {
            args[index + 1] = value.to_string();
        } else {
            args.push(value.to_string());
        }
        return;
    }

    let insert_at = insert_at.min(args.len());
    args.insert(insert_at, flag.to_string());
    args.insert((insert_at + 1).min(args.len()), value.to_string());
}

fn toml_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn ensure_codex_config_override(args: &mut Vec<String>, key: &str, value_expr: &str) {
    let key_prefix = format!("{key}=");
    let target = format!("{key}={value_expr}");

    let mut index = 0usize;
    while index + 1 < args.len() {
        let flag = args[index].as_str();
        if flag == "-c" || flag == "--config" {
            if args[index + 1].starts_with(&key_prefix) {
                args[index + 1] = target;
                return;
            }
            index += 2;
            continue;
        }
        index += 1;
    }

    // Keep prompt payload as the final argv token when present.
    let insert_at = args.len().saturating_sub(1);
    args.insert(insert_at, "-c".to_string());
    args.insert(insert_at + 1, target);
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
) {
    let server = match transport {
        McpServerTransport::Http(endpoint) => serde_json::json!({
            "type": "http",
            "url": endpoint
        }),
        McpServerTransport::Stdio { command, args } => serde_json::json!({
            "command": command,
            "args": args
        }),
    };
    let config = serde_json::json!({
        "mcpServers": {
            agent_id: server
        }
    })
    .to_string();
    ensure_flag(args, "--strict-mcp-config", 0);
    ensure_flag_value(args, "--mcp-config", &config, 0);
    ensure_flag_value(args, "--permission-mode", "bypassPermissions", 0);
}

fn apply_codex_native_mcp_lockdown(
    args: &mut Vec<String>,
    transport: McpServerTransport<'_>,
    agent_id: &str,
    configured_servers: &[String],
) {
    for server_name in configured_servers {
        if server_name.eq_ignore_ascii_case(agent_id) {
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
}

fn apply_gemini_native_mcp_lockdown(
    args: &mut Vec<String>,
    env: &mut HashMap<String, String>,
    transport: McpServerTransport<'_>,
    agent_id: &str,
    run_id: &RunId,
    temp_cleanup: &mut TempPathCleanup,
) -> Result<()> {
    ensure_flag_value(args, "--allowed-mcp-server-names", agent_id, 0);
    let server = match transport {
        McpServerTransport::Http(endpoint) => serde_json::json!({
            "type": "http",
            "url": endpoint
        }),
        McpServerTransport::Stdio { command, args } => serde_json::json!({
            "type": "stdio",
            "command": command,
            "args": args,
            // Gemini MCP currently fails on draft 2020-12 tool schemas.
            // Force AO MCP tool schemas to draft-07 for this adapter only.
            "env": {
                "AO_MCP_SCHEMA_DRAFT": "draft07"
            }
        }),
    };
    let settings = serde_json::json!({
        "tools": {
            "core": []
        },
        "mcp": {
            "allowed": [agent_id],
            "excluded": []
        },
        "mcpServers": {
            agent_id: server
        }
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
) {
    let server = match transport {
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
    let config = serde_json::json!({
        "mcp": {
            agent_id: server
        }
    });
    env.insert("OPENCODE_CONFIG_CONTENT".to_string(), config.to_string());
}

fn apply_native_mcp_policy(
    invocation: &mut super::process_builder::CliInvocation,
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

    match cli.as_str() {
        "claude" => apply_claude_native_mcp_lockdown(&mut invocation.args, transport, agent_id),
        "codex" => {
            let configured_servers = discover_codex_mcp_server_names();
            apply_codex_native_mcp_lockdown(
                &mut invocation.args,
                transport,
                agent_id,
                &configured_servers,
            );
        }
        "gemini" => apply_gemini_native_mcp_lockdown(
            &mut invocation.args,
            env,
            transport,
            agent_id,
            run_id,
            temp_cleanup,
        )?,
        "opencode" => apply_opencode_native_mcp_lockdown(env, transport, agent_id),
        _ => {
            bail!(
                "MCP-only policy enabled, but no native enforcement adapter exists for CLI command '{}'",
                invocation.command
            );
        }
    }

    Ok(())
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

    let target_server = parameters
        .get("server")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);

    if let Some(server) = target_server.as_deref() {
        if !server.eq_ignore_ascii_case(enforcement.agent_id.as_str()) {
            // Codex emits MCP discovery helper calls on its own host server.
            if server == "codex" && is_mcp_discovery_helper {
                return true;
            }
            return false;
        }
    }

    // These are MCP discovery helpers emitted by Codex itself; they do not map to a
    // concrete downstream AO tool name.
    if is_mcp_discovery_helper {
        return true;
    }

    let candidates = tool_name_candidates(tool_name, target_server.as_deref());
    if candidates.is_empty() {
        return false;
    }

    let blocked_match = enforcement.blocked_prefixes.iter().any(|prefix| {
        candidates
            .iter()
            .any(|candidate| candidate.starts_with(prefix))
    });
    if blocked_match {
        return false;
    }

    if enforcement.allowed_prefixes.is_empty() {
        if matches!(
            enforcement.tool_policy_mode,
            Some(McpToolPolicyMode::Allowlist)
        ) {
            return false;
        }
        return target_server.is_some();
    }

    enforcement.allowed_prefixes.iter().any(|prefix| {
        candidates
            .iter()
            .any(|candidate| candidate.starts_with(prefix))
    })
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
        mcp_blocked_prefixes = ?mcp_tool_enforcement.blocked_prefixes,
        mcp_tool_policy_mode = ?mcp_tool_enforcement.tool_policy_mode,
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
    fn mcp_tool_enforcement_applies_tool_policy_allowlist() {
        let contract = json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": {
                "enforce_only": true,
                "agent_id": "ao",
                "stdio": {
                    "command": "ao",
                    "args": ["mcp", "serve"]
                },
                "tool_policy": {
                    "mode": "allowlist",
                    "patterns": ["ao.task.*"]
                }
            }
        });
        let enforcement = resolve_mcp_tool_enforcement(Some(&contract));
        assert_eq!(
            enforcement.tool_policy_mode,
            Some(McpToolPolicyMode::Allowlist)
        );
        assert_eq!(enforcement.allowed_prefixes, vec!["ao.task.".to_string()]);
        assert!(is_tool_call_allowed(
            "task-list",
            &json!({ "server": "ao" }),
            &enforcement
        ));
        assert!(!is_tool_call_allowed(
            "workflow-run",
            &json!({ "server": "ao" }),
            &enforcement
        ));
    }

    #[test]
    fn mcp_tool_enforcement_allowlist_policy_overrides_allowed_prefixes() {
        let contract = json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": {
                "enforce_only": true,
                "agent_id": "ao",
                "allowed_tool_prefixes": ["ao.workflow."],
                "tool_policy": {
                    "mode": "allowlist",
                    "patterns": ["ao.task.*"]
                }
            }
        });
        let enforcement = resolve_mcp_tool_enforcement(Some(&contract));
        assert_eq!(
            enforcement.tool_policy_mode,
            Some(McpToolPolicyMode::Allowlist)
        );
        assert_eq!(enforcement.allowed_prefixes, vec!["ao.task.".to_string()]);
        assert!(is_tool_call_allowed(
            "task-list",
            &json!({ "server": "ao" }),
            &enforcement
        ));
        assert!(!is_tool_call_allowed(
            "workflow-run",
            &json!({ "server": "ao" }),
            &enforcement
        ));
    }

    #[test]
    fn mcp_tool_enforcement_applies_tool_policy_blocklist() {
        let contract = json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": {
                "enforce_only": true,
                "agent_id": "ao",
                "stdio": {
                    "command": "ao",
                    "args": ["mcp", "serve"]
                },
                "tool_policy": {
                    "mode": "blocklist",
                    "patterns": ["ao.task.delete*"]
                }
            }
        });
        let enforcement = resolve_mcp_tool_enforcement(Some(&contract));
        assert_eq!(
            enforcement.tool_policy_mode,
            Some(McpToolPolicyMode::Blocklist)
        );
        assert_eq!(
            enforcement.blocked_prefixes,
            vec!["ao.task.delete".to_string()]
        );
        assert!(is_tool_call_allowed(
            "task-list",
            &json!({ "server": "ao" }),
            &enforcement
        ));
        assert!(!is_tool_call_allowed(
            "task-delete",
            &json!({ "server": "ao" }),
            &enforcement
        ));
    }

    #[test]
    fn native_mcp_policy_rejects_unknown_cli_when_enforced() {
        let mut invocation = super::super::process_builder::CliInvocation {
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
            blocked_prefixes: Vec::new(),
            tool_policy_mode: None,
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
        let mut invocation = super::super::process_builder::CliInvocation {
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
            blocked_prefixes: Vec::new(),
            tool_policy_mode: None,
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
        let mut invocation = super::super::process_builder::CliInvocation {
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
            blocked_prefixes: Vec::new(),
            tool_policy_mode: None,
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
        let mut invocation = super::super::process_builder::CliInvocation {
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
            blocked_prefixes: Vec::new(),
            tool_policy_mode: None,
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
        let mut invocation = super::super::process_builder::CliInvocation {
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
            blocked_prefixes: Vec::new(),
            tool_policy_mode: None,
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
        let mut invocation = super::super::process_builder::CliInvocation {
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
            blocked_prefixes: Vec::new(),
            tool_policy_mode: None,
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
        let mut invocation = super::super::process_builder::CliInvocation {
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
            blocked_prefixes: Vec::new(),
            tool_policy_mode: None,
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
}
