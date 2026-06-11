//! Thin client over [`SessionBackendResolver`] for the CLI's
//! `animus agent {run, status, cancel, control}` surface.
//!
//! v0.5.3 fold-in: the standalone `agent-runner` sidecar was deleted.
//! Provider plugins already handle the underlying CLI invocation, so this
//! module is the entire client surface — there is no Unix-socket bridge
//! and no in-process pool to query.
//!
//! Status / cancel: provider plugins do not expose a persisted run
//! registry on the wire today. The CLI status command falls back to
//! persisted JSONL logs (handled by the caller via `read_agent_status`),
//! and control/cancel returns a clear unsupported error so callers can
//! degrade gracefully or stop submitting wire requests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use animus_session_backend::session::{SessionEvent, SessionRequest, SessionRun};
use anyhow::{anyhow, Result};
use orchestrator_plugin_host::session::{discover_provider_plugins, SessionBackendResolver};
use protocol::{
    AgentRunEvent, ArtifactInfo, ArtifactType, OutputStreamType, RunId, Timestamp, TokenUsage, ToolCallInfo,
    ToolResultInfo,
};
use serde_json::Value;

use crate::shared::canonicalize_cwd_in_project;
use crate::AgentRunArgs;

/// Snapshot of a provider plugin's discovery state.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ProviderHealthSnapshot {
    pub plugin_name: String,
    pub provider_tool: String,
    pub binary_path: String,
    pub installed: bool,
}

/// Build a [`SessionBackendResolver`] bound to `project_root`.
pub(crate) fn resolver_for(project_root: &Path) -> SessionBackendResolver {
    SessionBackendResolver::with_plugin_discovery(project_root)
}

/// Construct a [`SessionRequest`] from the CLI args.
///
/// Mirrors the security + context-forwarding guarantees of the deleted
/// agent-runner sidecar:
///
/// * `cwd` is canonicalized and rejected when it escapes the project root
///   (the legacy path used `canonicalize_cwd_in_project`).
/// * Any `--context-json` payload is merged into `extras.context` so
///   plugin-forwarded fields (`system_prompt`, `mcp_servers`, ...) survive
///   the agent-runner deletion.
/// * `--runtime-contract-json` continues to populate `extras.runtime_contract`.
pub(crate) fn session_request_from_args(args: &AgentRunArgs, project_root: &str) -> Result<SessionRequest> {
    let project_root_path = PathBuf::from(project_root);

    // Parse the context JSON once so we can use it as a fallback for
    // `prompt` / `cwd` / `timeout_secs` (matching the deleted sidecar's
    // `build_agent_context` precedence: flag > context-json) before
    // hoisting the remainder into `extras`.
    let context_object: Option<serde_json::Map<String, Value>> = match &args.context_json {
        Some(context_json) => match serde_json::from_str::<Value>(context_json)? {
            Value::Object(map) => Some(map),
            _ => None,
        },
        None => None,
    };

    let context_str = |map: Option<&serde_json::Map<String, Value>>, key: &str| -> Option<String> {
        map.and_then(|m| m.get(key)).and_then(Value::as_str).map(ToOwned::to_owned)
    };
    let context_u64 = |map: Option<&serde_json::Map<String, Value>>, key: &str| -> Option<u64> {
        map.and_then(|m| m.get(key)).and_then(Value::as_u64)
    };

    let raw_cwd = args
        .cwd
        .clone()
        .or_else(|| context_str(context_object.as_ref(), "cwd"))
        .unwrap_or_else(|| project_root.to_string());
    let canonical_cwd = canonicalize_cwd_in_project(&raw_cwd, project_root)?;
    let cwd = PathBuf::from(canonical_cwd);

    let prompt = args.prompt.clone().or_else(|| context_str(context_object.as_ref(), "prompt")).unwrap_or_default();
    let timeout_secs = args.timeout_secs.or_else(|| context_u64(context_object.as_ref(), "timeout_secs"));

    let mut extras = serde_json::Map::new();

    // `PluginSessionBackend::build_run_params` reads the following keys
    // at the top level of `extras` and forwards them to the provider
    // plugin verbatim: `system_prompt`, `claude_profile`, `mcp_servers`,
    // `tools`, `response_schema`, `runtime_contract`, `session_id`.
    // To stay compatible with `--context-json '{"system_prompt": ...}'`
    // and friends, hoist every top-level field of the context JSON into
    // `extras` directly (rather than nesting under `extras.context`,
    // which the plugin host would silently drop). Skip the keys that
    // were already lifted into the typed `SessionRequest` fields above
    // so we don't pay for them twice.
    if let Some(map) = context_object.clone() {
        for (key, value) in map {
            if matches!(key.as_str(), "prompt" | "cwd" | "timeout_secs" | "tool" | "model" | "project_root") {
                continue;
            }
            extras.insert(key, value);
        }
    }

    // Reasoning effort: the `--reasoning-effort` flag wins over any
    // `reasoning_effort` forwarded through `--context-json`. Stored as a
    // lowercase string that the provider transports map to their own flag
    // (codex `-c model_reasoning_effort`, claude `--effort`).
    if let Some(level) = args.reasoning_effort {
        extras.insert("reasoning_effort".to_string(), Value::String(level.as_str().to_string()));
    }

    // `--runtime-contract-json` wins over a `runtime_contract` key
    // forwarded through `--context-json` (matches the deleted
    // sidecar's precedence). Cache the parsed value so we can read the
    // provider tool from `runtime_contract.cli.name` below.
    let runtime_contract_value: Option<Value> = match &args.runtime_contract_json {
        Some(json) => Some(serde_json::from_str::<Value>(json)?),
        None => extras.get("runtime_contract").cloned(),
    };
    if let Some(value) = runtime_contract_value.clone() {
        extras.insert("runtime_contract".to_string(), value);
    }

    // Tool / provider precedence (matches the deleted sidecar):
    //   1. `runtime_contract.cli.name` from `--runtime-contract-json` or
    //      a `runtime_contract` key inside `--context-json`.
    //   2. `tool` from `--context-json`.
    //   3. `--tool` flag (which has a clap default of `claude`).
    let runtime_contract_tool = runtime_contract_value
        .as_ref()
        .and_then(|value| value.pointer("/cli/name"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let tool = runtime_contract_tool
        .or_else(|| context_str(context_object.as_ref(), "tool"))
        .unwrap_or_else(|| args.tool.clone());

    let model = args
        .model
        .clone()
        .or_else(|| context_str(context_object.as_ref(), "model"))
        .unwrap_or_else(|| protocol::default_model_for_tool(&tool).unwrap_or("claude-sonnet-4-6").to_string());

    // When the caller did NOT supply a runtime_contract (neither
    // `--runtime-contract-json` nor a `runtime_contract` key in
    // `--context-json`), assemble one from the agent's profile/skill MCP
    // servers so an ad-hoc `animus agent run` agent sees the MCP servers its
    // profile/skill declares. A caller-supplied contract is never clobbered.
    if runtime_contract_value.is_none() {
        let scope = crate::services::runtime::agent_mcp::resolve_agent_scope(
            &project_root_path,
            &tool,
            args.agent.as_deref(),
            args.skill.as_deref(),
        )?;
        let scope_selected = args.agent.is_some() || args.skill.is_some();
        if let Some(contract) = crate::services::runtime::agent_mcp::assemble_agent_mcp_contract(
            &project_root_path,
            &tool,
            &model,
            &scope.profile_servers,
            &scope.skill_servers,
            &args.mcp_server,
            &scope.tool_policy,
            scope_selected,
            args.no_animus_mcp,
            args.agent.as_deref(),
        )? {
            // Provider CLIs that auto-discover a cwd-local `.mcp.json`
            // (claude-code) register MCP servers from that file rather than
            // the runtime contract, so materialize the per-agent set there
            // too (additive merge that preserves user-authored entries).
            crate::services::runtime::agent_mcp::materialize_mcp_json(&cwd, &contract)?;
            // Mirror the SAME resolved set onto the plugin-protocol
            // `mcp_servers` channel so providers that consume
            // `AgentRunRequest.mcp_servers` (rather than the runtime
            // contract or `.mcp.json`) see the per-agent servers too. An
            // explicit `mcp_servers` from `--context-json` wins (it was
            // hoisted into `extras` above), and an empty resolved set
            // populates nothing.
            if !extras.contains_key("mcp_servers") {
                let servers = crate::services::runtime::agent_mcp::contract_mcp_servers_for_wire(&contract);
                if !servers.is_empty() {
                    extras.insert("mcp_servers".to_string(), Value::Object(servers));
                }
            }
            extras.insert("runtime_contract".to_string(), contract);
        }
    }

    Ok(SessionRequest {
        tool,
        model,
        prompt,
        cwd,
        project_root: Some(project_root_path),
        mcp_endpoint: None,
        permission_mode: None,
        timeout_secs,
        env_vars: Vec::new(),
        extras: Value::Object(extras),
    })
}

/// Start a session through the resolver for the supplied request.
pub(crate) async fn start_session(project_root: &Path, request: SessionRequest) -> Result<SessionRun> {
    let resolver = resolver_for(project_root);
    resolver.start_session(request).await.map_err(|err| anyhow!("provider session failed: {err}"))
}

/// Translate a [`SessionEvent`] into the legacy [`AgentRunEvent`] shape so
/// existing persistence (`persist_agent_event`) and rendering
/// (`print_agent_event`) keep working unchanged.
pub(crate) fn to_agent_event(event: SessionEvent, run_id: &RunId) -> AgentRunEvent {
    match event {
        SessionEvent::Started { .. } => AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() },
        SessionEvent::TextDelta { text } | SessionEvent::FinalText { text } => {
            AgentRunEvent::OutputChunk { run_id: run_id.clone(), stream_type: OutputStreamType::Stdout, text }
        }
        SessionEvent::ToolCall { tool_name, arguments, server: _ } => AgentRunEvent::ToolCall {
            run_id: run_id.clone(),
            tool_info: ToolCallInfo { tool_name, parameters: arguments, timestamp: Timestamp::now() },
        },
        SessionEvent::ToolResult { tool_name, output, success } => AgentRunEvent::ToolResult {
            run_id: run_id.clone(),
            result_info: ToolResultInfo { tool_name, result: output, duration_ms: 0, success },
        },
        SessionEvent::Thinking { text } => AgentRunEvent::Thinking { run_id: run_id.clone(), content: text },
        SessionEvent::Artifact { artifact_id, metadata } => AgentRunEvent::Artifact {
            run_id: run_id.clone(),
            artifact_info: ArtifactInfo {
                artifact_id,
                artifact_type: ArtifactType::Other,
                file_path: metadata.get("file_path").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
                size_bytes: metadata.get("size_bytes").and_then(serde_json::Value::as_u64),
                mime_type: metadata.get("mime_type").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
            },
        },
        SessionEvent::Metadata { metadata } => AgentRunEvent::Metadata {
            run_id: run_id.clone(),
            cost: metadata.get("cost").and_then(serde_json::Value::as_f64),
            tokens: extract_token_usage(&metadata),
        },
        // Recoverable provider errors are warnings — the run continues.
        // `read_agent_status` treats `AgentRunEvent::Error` as a terminal
        // `failed` status, so route recoverable frames into stderr
        // output chunks instead (matches the deleted sidecar's
        // semantics so transient provider warnings don't poison
        // persisted status). Unrecoverable errors keep the `Error`
        // variant so callers can detect the terminal frame.
        SessionEvent::Error { message, recoverable } => {
            if recoverable {
                AgentRunEvent::OutputChunk {
                    run_id: run_id.clone(),
                    stream_type: OutputStreamType::Stderr,
                    text: message,
                }
            } else {
                AgentRunEvent::Error { run_id: run_id.clone(), error: message }
            }
        }
        SessionEvent::Finished { exit_code } => {
            AgentRunEvent::Finished { run_id: run_id.clone(), exit_code, duration_ms: 0 }
        }
    }
}

/// Map provider-emitted metadata frames into a [`TokenUsage`] when the
/// payload carries usage counters. Inspects the canonical
/// `token_usage` / `tokens` keys first; falls back to the per-provider
/// variants (`claude_usage`, `codex_usage`, `gemini_stats`, `usage`)
/// the deleted sidecar's metadata parser produced.
fn extract_token_usage(metadata: &Value) -> Option<TokenUsage> {
    if metadata.is_null() {
        return None;
    }
    const KEYS: &[&str] = &["token_usage", "tokens", "usage", "claude_usage", "codex_usage", "gemini_stats"];
    let payload = KEYS.iter().find_map(|key| metadata.get(*key)).unwrap_or(metadata);

    let read_u32 = |keys: &[&str]| -> Option<u32> {
        keys.iter().find_map(|key| payload.get(*key)).and_then(Value::as_u64).map(|n| n as u32)
    };

    let input = read_u32(&["input", "input_tokens", "prompt_tokens"])?;
    let output = read_u32(&["output", "output_tokens", "completion_tokens"])?;
    let reasoning = read_u32(&["reasoning", "reasoning_tokens"]);
    let cache_read = read_u32(&["cache_read", "cache_read_input_tokens", "cache_read_tokens"]);
    let cache_write = read_u32(&["cache_write", "cache_creation_input_tokens", "cache_write_tokens"]);

    Some(TokenUsage { input, output, reasoning, cache_read, cache_write })
}

/// Snapshot every installed provider plugin (one entry per discovered
/// binary). Used by `animus runner health` and the daemon health
/// endpoint to report provider availability without spawning the plugin.
///
/// `installed` reports `true` only when the binary both exists AND is
/// executable by the current user — a path that exists but lacks the
/// execute bit (or any platform-equivalent) is reported as not installed
/// so operators don't get a false-green health result before the
/// `EPERM`-at-spawn surfaces during the first agent run.
pub(crate) fn health_snapshot(project_root: &Path) -> Vec<ProviderHealthSnapshot> {
    discover_provider_plugins(project_root)
        .into_iter()
        .map(|plugin| {
            let installed = is_executable(&plugin.binary_path);
            ProviderHealthSnapshot {
                plugin_name: plugin.plugin_name.clone(),
                provider_tool: plugin.provider_tool.clone(),
                binary_path: plugin.binary_path.display().to_string(),
                installed,
            }
        })
        .collect()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    let mode = meta.permissions().mode();
    // Any executable bit is sufficient — `Command::spawn` will succeed
    // as long as at least the owner-executable bit is set; the user-vs.-
    // group-vs.-other distinction is enforced by the OS at exec time.
    mode & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    // On Windows we can't cheaply distinguish executable vs. non-
    // executable without parsing the binary header. Fall back to
    // existence; `Command::spawn` surfaces real failures shortly after.
    path.exists()
}

/// Convenience: are any provider plugins discovered and executable on disk?
#[allow(dead_code)]
pub(crate) fn provider_plugins_healthy(project_root: &Path) -> bool {
    health_snapshot(project_root).iter().any(|snap| snap.installed)
}

/// Owned [`SessionBackendResolver`] handle for advanced callers.
#[allow(dead_code)]
pub(crate) struct ProviderClient {
    resolver: Arc<SessionBackendResolver>,
}

#[allow(dead_code)]
impl ProviderClient {
    pub(crate) fn new(project_root: &Path) -> Self {
        Self { resolver: Arc::new(SessionBackendResolver::with_plugin_discovery(project_root)) }
    }

    pub(crate) async fn run(&self, request: SessionRequest) -> Result<SessionRun> {
        self.resolver.start_session(request).await.map_err(|err| anyhow!("provider session failed: {err}"))
    }

    /// Status RPC: provider plugins do not expose a persisted-run query
    /// surface today. Callers should fall back to persisted JSONL logs.
    pub(crate) fn status(&self, _run_id: &RunId) -> Result<()> {
        Err(anyhow!(
            "agent status query through provider plugins is not yet supported; reading persisted run logs instead"
        ))
    }

    /// Cancel RPC: same as `status` — not yet exposed by provider plugins.
    pub(crate) fn cancel(&self, _run_id: &RunId) -> Result<()> {
        Err(anyhow!(
            "agent cancel through provider plugins is not yet supported; in-flight cancel via process signal only"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentRunArgs;

    fn base_args(project_root: &str) -> AgentRunArgs {
        AgentRunArgs {
            run_id: None,
            tool: "claude".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
            prompt: Some("hi".to_string()),
            reasoning_effort: None,
            cwd: Some(project_root.to_string()),
            timeout_secs: None,
            context_json: None,
            runtime_contract_json: None,
            detach: false,
            stream: true,
            save_jsonl: false,
            jsonl_dir: None,
            start_runner: false,
            runner_scope: None,
            agent: None,
            skill: None,
            mcp_server: Vec::new(),
            no_animus_mcp: false,
        }
    }

    #[test]
    fn agent_run_without_caller_contract_gets_assembled_animus_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let args = base_args(&root_str);
        let request = session_request_from_args(&args, &root_str).expect("request builds");

        let contract = request
            .extras
            .pointer("/runtime_contract")
            .expect("a runtime_contract must be assembled when the caller supplied none");
        // Plain agent run defaults to the built-in animus stdio server.
        assert!(
            contract.pointer("/mcp/stdio/command").and_then(Value::as_str).is_some(),
            "assembled contract must wire the animus stdio server; contract: {contract}"
        );
    }

    #[test]
    fn agent_run_does_not_clobber_a_caller_supplied_runtime_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let mut args = base_args(&root_str);
        // A caller-supplied contract with a sentinel marker the assembler
        // would never produce.
        args.runtime_contract_json =
            Some(r#"{"cli":{"name":"claude"},"sentinel":"caller-owned","mcp":{}}"#.to_string());

        let request = session_request_from_args(&args, &root_str).expect("request builds");
        let contract = request.extras.pointer("/runtime_contract").expect("caller contract must survive");
        assert_eq!(
            contract.pointer("/sentinel").and_then(Value::as_str),
            Some("caller-owned"),
            "a caller-supplied runtime_contract must NOT be clobbered by the assembler"
        );
        // The assembler must not have run, so no animus stdio injection.
        assert!(
            contract.pointer("/mcp/stdio/command").is_none(),
            "assembler must not touch a caller-supplied contract; contract: {contract}"
        );
    }

    #[test]
    fn agent_run_populates_mcp_servers_from_the_assembled_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let args = base_args(&root_str);
        let request = session_request_from_args(&args, &root_str).expect("request builds");

        // The resolved set (plain run → the animus stdio server) must ride
        // extras.mcp_servers in the canonical wire shape, mirroring what
        // materialize_mcp_json wrote for the same run.
        let servers = request
            .extras
            .pointer("/mcp_servers")
            .and_then(Value::as_object)
            .expect("extras.mcp_servers must be populated from the assembled contract");
        let animus = servers.get("animus").expect("the animus stdio server is in the resolved set");
        assert!(
            animus.get("command").and_then(Value::as_str).is_some(),
            "stdio wire entry carries command; got {animus}"
        );
    }

    #[test]
    fn agent_run_context_json_mcp_servers_wins_over_the_resolved_set() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let mut args = base_args(&root_str);
        args.context_json = Some(r#"{"mcp_servers":{"caller-owned":{"command":"my-server"}}}"#.to_string());

        let request = session_request_from_args(&args, &root_str).expect("request builds");
        let servers = request
            .extras
            .pointer("/mcp_servers")
            .and_then(Value::as_object)
            .expect("the caller's mcp_servers must survive");
        assert!(
            servers.contains_key("caller-owned") && servers.len() == 1,
            "an explicit --context-json mcp_servers must not be clobbered by the resolved set; got {servers:?}"
        );
    }

    #[test]
    fn agent_run_empty_resolved_set_populates_no_mcp_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let mut args = base_args(&root_str);
        args.no_animus_mcp = true;

        let request = session_request_from_args(&args, &root_str).expect("request builds");
        assert!(
            request.extras.pointer("/mcp_servers").is_none(),
            "an empty resolved set must not populate extras.mcp_servers; extras: {}",
            request.extras
        );
    }

    #[test]
    fn agent_profile_secret_bearing_server_rides_the_wire_as_proxy_stdio() {
        // End-to-end over the ad-hoc run path: an `--agent` profile whose
        // workflow YAML `mcp_servers` scope names a manual_bearer server must
        // produce a name-keyed `extras.mcp_servers` object where that server
        // is the `animus-mcp-proxy` stdio entry — never the upstream URL with
        // a resolved Authorization header. (`build_run_params` forwards
        // `extras.mcp_servers` verbatim into the agent/run RPC params; see
        // orchestrator-plugin-host's resume_request_forwards_checkpointed_
        // mcp_servers.)
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();
        let bearer_env = "ANIMUS_TEST_PROFILE_WIRE_BEARER";
        std::env::set_var(bearer_env, "tok-profile-secret");
        std::fs::create_dir_all(root.join(".animus")).unwrap();
        std::fs::write(
            root.join(".animus").join("workflows.yaml"),
            format!(
                r#"
mcp_servers:
  trading:
    transport: http
    url: "https://trading.example.com/mcp"
    oauth:
      flow: manual_bearer
      bearer_env: {bearer_env}
agents:
  trader:
    description: "Trading agent"
    mcp_servers: [trading]
"#
            ),
        )
        .unwrap();

        let mut args = base_args(&root_str);
        args.agent = Some("trader".to_string());
        let request = session_request_from_args(&args, &root_str).expect("request builds");
        std::env::remove_var(bearer_env);

        let servers = request
            .extras
            .pointer("/mcp_servers")
            .and_then(Value::as_object)
            .expect("the trader profile's resolved set must ride extras.mcp_servers");
        let trading = servers.get("trading").expect("the profile-scoped server is in the wire map");
        let command = trading.get("command").and_then(Value::as_str).expect("proxy stdio entry carries command");
        assert!(command.contains("animus-mcp-proxy"), "expected the proxy binary; got {command}");
        assert!(trading.get("url").is_none(), "proxy entry carries no upstream url; got {trading}");
        assert!(trading.get("headers").is_none(), "no resolved header may ride the wire; got {trading}");
        let serialized = serde_json::to_string(&request.extras).unwrap();
        assert!(
            !serialized.contains("tok-profile-secret"),
            "the resolved bearer token must never appear in extras: {serialized}"
        );
    }

    #[test]
    fn agent_run_no_animus_mcp_drops_the_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let mut args = base_args(&root_str);
        args.no_animus_mcp = true;

        let request = session_request_from_args(&args, &root_str).expect("request builds");
        // With --no-animus-mcp and no other servers, the resolved set is
        // empty; the contract still builds but wires no animus stdio server.
        if let Some(contract) = request.extras.pointer("/runtime_contract") {
            assert!(
                contract.pointer("/mcp/stdio/command").is_none(),
                "--no-animus-mcp must drop the built-in animus server; contract: {contract}"
            );
        }
    }
}
