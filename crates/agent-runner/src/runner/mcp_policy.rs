use anyhow::{bail, Context, Result};
use cli_wrapper::{ensure_codex_config_override, ensure_flag, ensure_flag_value, LaunchInvocation};
use protocol::RunId;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tracing::debug;

#[derive(Debug, Clone)]
pub(super) struct McpStdioConfig {
    pub(super) command: String,
    pub(super) args: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct AdditionalMcpServer {
    pub(super) name: String,
    pub(super) command: String,
    pub(super) args: Vec<String>,
    pub(super) env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub(super) struct McpToolEnforcement {
    pub(super) enabled: bool,
    pub(super) endpoint: Option<String>,
    pub(super) stdio: Option<McpStdioConfig>,
    pub(super) agent_id: String,
    pub(super) allowed_prefixes: Vec<String>,
    pub(super) tool_policy_allow: Vec<String>,
    pub(super) tool_policy_deny: Vec<String>,
    pub(super) additional_servers: Vec<AdditionalMcpServer>,
}

#[derive(Debug, Default)]
pub(super) struct TempPathCleanup {
    paths: Vec<PathBuf>,
}

impl TempPathCleanup {
    pub(super) fn track(&mut self, path: PathBuf) {
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

pub(super) fn resolve_mcp_tool_enforcement(runtime_contract: Option<&serde_json::Value>) -> McpToolEnforcement {
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
        contract.pointer("/cli/capabilities/supports_mcp").and_then(serde_json::Value::as_bool).unwrap_or(false);
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
    let stdio = stdio_command.map(|command| McpStdioConfig { command, args: stdio_args });
    let has_endpoint = endpoint.is_some();
    let has_stdio = stdio.is_some();
    let agent_id = contract
        .pointer("/mcp/agent_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ao")
        .to_string();
    let explicit_enforce = contract.pointer("/mcp/enforce_only").and_then(serde_json::Value::as_bool);
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
                    command: entry.get("command").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                    args: entry
                        .get("args")
                        .and_then(serde_json::Value::as_array)
                        .map(|a| a.iter().filter_map(serde_json::Value::as_str).map(ToString::to_string).collect())
                        .unwrap_or_default(),
                    env: entry
                        .get("env")
                        .and_then(serde_json::Value::as_object)
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
            stderr = %super::process::truncate_for_log(&stderr, 200),
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

fn resolve_mcp_server_transport<'a>(enforcement: &'a McpToolEnforcement) -> Result<McpServerTransport<'a>> {
    if let Some(stdio) = enforcement.stdio.as_ref() {
        return Ok(McpServerTransport::Stdio { command: stdio.command.trim(), args: &stdio.args });
    }
    if let Some(endpoint) = enforcement.endpoint.as_deref() {
        return Ok(McpServerTransport::Http(endpoint));
    }

    bail!("MCP-only policy is enabled, but neither mcp.endpoint nor mcp.stdio.command is configured");
}

fn sanitize_token_for_filename(raw: &str) -> String {
    raw.chars().map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '_' }).collect()
}

fn write_temp_json_file(run_id: &RunId, prefix: &str, value: &serde_json::Value) -> Result<PathBuf> {
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
        let mut config = serde_json::Map::new();
        config.insert("command".to_string(), serde_json::Value::String(server.command.clone()));
        config.insert("args".to_string(), serde_json::to_value(&server.args).expect("server args should serialize"));
        if !server.env.is_empty() {
            config.insert("env".to_string(), serde_json::to_value(&server.env).expect("server env should serialize"));
        }
        mcp_servers.insert(server.name.clone(), serde_json::Value::Object(config));
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
        let toml_args = format!("[{}]", server.args.iter().map(|arg| toml_string(arg)).collect::<Vec<_>>().join(", "));
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
        let mut config = serde_json::Map::new();
        config.insert("type".to_string(), serde_json::Value::String("stdio".to_string()));
        config.insert("command".to_string(), serde_json::Value::String(server.command.clone()));
        config.insert("args".to_string(), serde_json::to_value(&server.args).expect("server args should serialize"));
        if !server.env.is_empty() {
            config.insert("env".to_string(), serde_json::to_value(&server.env).expect("server env should serialize"));
        }
        mcp_servers.insert(server.name.clone(), serde_json::Value::Object(config));
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
        let mut config = serde_json::Map::new();
        config.insert("type".to_string(), serde_json::Value::String("local".to_string()));
        config.insert(
            "command".to_string(),
            serde_json::to_value(command_with_args).expect("server command should serialize"),
        );
        config.insert("enabled".to_string(), serde_json::Value::Bool(true));
        if !server.env.is_empty() {
            config.insert("env".to_string(), serde_json::to_value(&server.env).expect("server env should serialize"));
        }
        mcp_entries.insert(server.name.clone(), serde_json::Value::Object(config));
    }
    let config = serde_json::json!({ "mcp": mcp_entries });
    env.insert("OPENCODE_CONFIG_CONTENT".to_string(), config.to_string());
}

fn apply_oai_runner_native_mcp_lockdown(args: &mut Vec<String>, transport: McpServerTransport<'_>) {
    let config = match transport {
        McpServerTransport::Stdio { command, args: stdio_args } => {
            serde_json::json!([{ "command": command, "args": stdio_args }])
        }
        McpServerTransport::Http(_) => return,
    };
    let insert_at = args.iter().position(|entry| entry == "run").map(|index| index + 1).unwrap_or(0);
    ensure_flag_value(args, "--mcp-config", &config.to_string(), insert_at);
}

pub(super) fn apply_native_mcp_policy(
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
        "claude" => apply_claude_native_mcp_lockdown(&mut invocation.args, transport, agent_id, additional),
        "codex" => {
            let configured_servers = discover_codex_mcp_server_names();
            apply_codex_native_mcp_lockdown(&mut invocation.args, transport, agent_id, &configured_servers, additional);
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
            (Some(b'*'), _) => inner(&pat[1..], val) || (!val.is_empty() && inner(pat, &val[1..])),
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
        enforcement.tool_policy_allow.iter().any(|p| tool_policy_glob_match(p, tool_name))
    };
    if !allowed {
        return false;
    }
    !enforcement.tool_policy_deny.iter().any(|p| tool_policy_glob_match(p, tool_name))
}

pub(super) fn is_tool_call_allowed(
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

    let is_mcp_discovery_helper =
        matches!(normalized.as_str(), "list_mcp_resources" | "list_mcp_resource_templates" | "read_mcp_resource");

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
        if server == "codex" && is_mcp_discovery_helper {
            return true;
        }
        return false;
    }

    if is_mcp_discovery_helper {
        return true;
    }

    let prefix_allowed = enforcement.allowed_prefixes.iter().any(|prefix| normalized.starts_with(prefix));
    if !prefix_allowed {
        return false;
    }
    is_tool_policy_permitted(&normalized, enforcement)
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
        assert_eq!(enforcement.endpoint.as_deref(), Some("http://127.0.0.1:3101/mcp/ao"));
        assert_eq!(enforcement.agent_id, "ao");
        assert!(enforcement.allowed_prefixes.iter().any(|prefix| prefix == "ao."));
        assert!(enforcement.allowed_prefixes.iter().any(|prefix| prefix == "mcp__ao__"));
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
        assert!(is_tool_call_allowed("ao.task.list", &json!({}), &enforcement));
        assert!(is_tool_call_allowed("phase_transition", &json!({}), &enforcement));
        assert!(!is_tool_call_allowed("Bash", &json!({}), &enforcement));
        assert!(!is_tool_call_allowed("stories-search", &json!({ "server": "shortcut" }), &enforcement));
        assert!(is_tool_call_allowed("requirements-get", &json!({ "server": "ao" }), &enforcement));
        assert!(is_tool_call_allowed("list_mcp_resources", &json!({}), &enforcement));
        assert!(is_tool_call_allowed("list_mcp_resources", &json!({ "server": "codex" }), &enforcement));
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

        let err = apply_native_mcp_policy(&mut invocation, &enforcement, &mut env, &run_id, &mut cleanup)
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

        let err = apply_native_mcp_policy(&mut invocation, &enforcement, &mut env, &run_id, &mut cleanup)
            .expect_err("missing transport should fail closed");

        assert!(err.to_string().contains("neither mcp.endpoint nor mcp.stdio.command"));
    }

    #[test]
    fn native_mcp_policy_adds_codex_mcp_server_override() {
        let mut invocation = LaunchInvocation {
            command: "codex".to_string(),
            args: vec!["exec".to_string(), "--json".to_string(), "hello".to_string()],
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

        apply_native_mcp_policy(&mut invocation, &enforcement, &mut env, &run_id, &mut cleanup)
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

        apply_native_mcp_policy(&mut invocation, &enforcement, &mut env, &run_id, &mut cleanup)
            .expect("claude policy should apply");

        assert!(invocation
            .args
            .windows(2)
            .any(|pair| { pair[0] == "--permission-mode" && pair[1] == "bypassPermissions" }));
        assert!(invocation.args.iter().any(|arg| arg == "--strict-mcp-config"));
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
        assert_eq!(parse_codex_mcp_server_names(payload), vec!["ao".to_string(), "shortcut".to_string()]);
    }

    #[test]
    fn codex_native_lockdown_disables_non_target_servers() {
        let mut args = vec!["exec".to_string(), "--json".to_string(), "hello".to_string()];
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
        let mut args = vec!["exec".to_string(), "--json".to_string(), "hello".to_string()];

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
        assert!(joined.contains("mcp_servers.ao.command=\"/Users/samishukri/ao-cli/target/debug/ao\""));
        assert!(joined
            .contains("mcp_servers.ao.args=[\"--project-root\", \"/Users/samishukri/ao-cli\", \"mcp\", \"serve\"]"));
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

        apply_native_mcp_policy(&mut invocation, &enforcement, &mut env, &run_id, &mut cleanup)
            .expect("gemini policy should apply");

        let settings_path =
            env.get("GEMINI_CLI_SYSTEM_SETTINGS_PATH").expect("gemini settings path should be set").to_string();
        assert!(invocation.args.windows(2).any(|pair| pair[0] == "--allowed-mcp-server-names" && pair[1] == "ao"));
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

        apply_native_mcp_policy(&mut invocation, &enforcement, &mut env, &run_id, &mut cleanup)
            .expect("gemini policy should apply");

        let settings_path =
            env.get("GEMINI_CLI_SYSTEM_SETTINGS_PATH").expect("gemini settings path should be set").to_string();
        let settings = std::fs::read_to_string(&settings_path).expect("read gemini settings");
        assert!(settings.contains("\"type\":\"http\""), "expected http transport in gemini settings, got: {settings}");
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
            args: vec!["run".to_string(), "--format".to_string(), "json".to_string()],
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

        apply_native_mcp_policy(&mut invocation, &enforcement, &mut env, &run_id, &mut cleanup)
            .expect("opencode policy should apply");

        let config_raw = env.get("OPENCODE_CONFIG_CONTENT").expect("opencode config should be provided");
        let parsed: serde_json::Value = serde_json::from_str(config_raw).expect("opencode config should be valid JSON");
        assert_eq!(parsed.pointer("/mcp/ao/type").and_then(serde_json::Value::as_str), Some("local"));
        assert_eq!(
            parsed.pointer("/mcp/ao/command/0").and_then(serde_json::Value::as_str),
            Some("/Users/samishukri/ao-cli/target/debug/ao")
        );
        assert_eq!(parsed.pointer("/mcp/ao/command/4").and_then(serde_json::Value::as_str), Some("serve"));
        assert!(parsed.pointer("/mcp/ao/args").is_none());
    }

    #[test]
    fn native_mcp_policy_inserts_oai_runner_mcp_config_after_run_subcommand() {
        let mut invocation = LaunchInvocation {
            command: "ao-oai-runner".to_string(),
            args: vec![
                "run".to_string(),
                "-m".to_string(),
                "minimax/MiniMax-M2.5".to_string(),
                "--format".to_string(),
                "json".to_string(),
                "hello".to_string(),
            ],
            prompt_via_stdin: false,
        };
        let enforcement = McpToolEnforcement {
            enabled: true,
            endpoint: None,
            stdio: Some(McpStdioConfig {
                command: "/Users/samishukri/ao-cli/target/debug/ao".to_string(),
                args: vec![
                    "mcp".to_string(),
                    "serve".to_string(),
                    "--project-root".to_string(),
                    "/Users/samishukri/ao-cli".to_string(),
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
        let run_id = RunId("run-oai-runner".to_string());

        apply_native_mcp_policy(&mut invocation, &enforcement, &mut env, &run_id, &mut cleanup)
            .expect("oai-runner policy should apply");

        let mcp_idx =
            invocation.args.iter().position(|arg| arg == "--mcp-config").expect("mcp config flag should be present");
        assert_eq!(invocation.args.first().map(String::as_str), Some("run"));
        assert_eq!(mcp_idx, 1, "mcp config should follow the run subcommand");
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
        assert_eq!(enforcement.additional_servers[0].env.get("DB_HOST").map(String::as_str), Some("localhost"));
    }

    #[test]
    fn claude_lockdown_includes_additional_servers() {
        let mut args = vec!["--print".to_string(), "hello".to_string()];
        let additional = vec![AdditionalMcpServer {
            name: "my-db".to_string(),
            command: "/usr/local/bin/db-mcp".to_string(),
            args: vec!["--port".to_string(), "5432".to_string()],
            env: HashMap::from([("DB_HOST".to_string(), "localhost".to_string())]),
        }];
        apply_claude_native_mcp_lockdown(
            &mut args,
            McpServerTransport::Stdio { command: "/usr/local/bin/ao", args: &["mcp".to_string(), "serve".to_string()] },
            "ao",
            &additional,
        );
        let joined = args.join(" ");
        assert!(joined.contains("mcpServers"));
        let mcp_config_idx = args.iter().position(|a| a == "--mcp-config").unwrap();
        let config_json: serde_json::Value = serde_json::from_str(&args[mcp_config_idx + 1]).unwrap();
        assert!(config_json.pointer("/mcpServers/ao").is_some());
        assert!(config_json.pointer("/mcpServers/my-db").is_some());
        assert_eq!(
            config_json.pointer("/mcpServers/my-db/command").and_then(serde_json::Value::as_str),
            Some("/usr/local/bin/db-mcp")
        );
        assert_eq!(
            config_json.pointer("/mcpServers/my-db/env/DB_HOST").and_then(serde_json::Value::as_str),
            Some("localhost")
        );
    }
}
