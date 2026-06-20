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

use animus_session_backend::session::{SessionEvent, SessionRequest, SessionRun};
use anyhow::{anyhow, Result};
use orchestrator_config::skill_definition::SkillApplicationResult;
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

    let mut prompt = args.prompt.clone().or_else(|| context_str(context_object.as_ref(), "prompt")).unwrap_or_default();
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
            if matches!(
                key.as_str(),
                "prompt" | "cwd" | "timeout_secs" | "tool" | "model" | "project_root" | "permission_mode"
            ) {
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

    // Permission mode: the `--permission-mode` flag wins over a
    // `permission_mode` forwarded through `--context-json`, which wins over
    // the selected `--agent` profile's `permission_mode`. The value is
    // provider-specific (claude `--permission-mode`, codex
    // `-c approval_policy`, gemini approval mode) and rides the typed
    // `SessionRequest.permission_mode` field verbatim; unknown values only
    // warn, never block.
    let permission_mode = args
        .permission_mode
        .clone()
        .or_else(|| context_str(context_object.as_ref(), "permission_mode"))
        .or_else(|| profile_permission_mode(&project_root_path, args.agent.as_deref()));
    if let Some(mode) = permission_mode.as_deref() {
        warn_unknown_permission_mode(mode);
    }

    // Kernel-mediated approvals: the `--approvals` flag or an
    // `approval_policy` on the selected `--agent` profile sets
    // `extras.approvals = true`. Transports consume it (claude wires
    // `--permission-prompt-tool mcp__animus__animus_agent_request_approval`;
    // others inject a system-prompt instruction block) — the kernel only
    // sets the flag.
    //
    // Composition with `permission_mode` (verified, not a conflict): the two
    // are ORTHOGONAL layers and BOTH apply when set on one profile.
    // `permission_mode` is the transport-level guard — it rides the typed
    // `SessionRequest.permission_mode` above and maps to the provider CLI's
    // own permission flag, deciding whether the provider acts autonomously or
    // escalates at all. `approval_policy` is the kernel inbox layer — its
    // presence here flips `extras.approvals=true` so escalations that DO reach
    // the kernel are routed through `animus.agent.request_approval`, where
    // `ApprovalPolicy::evaluate` then auto-allows/denies/asks (auto_deny wins).
    // Neither overrides the other; they compose. See
    // `docs/reference/agent-runtime-config.md` and `docs/guides/agents.md`.
    if args.approvals || profile_has_approval_policy(&project_root_path, args.agent.as_deref()) {
        extras.insert("approvals".to_string(), Value::Bool(true));
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

    // Resolve the agent/skill scope once (the same resolution feeds the MCP
    // contract assembly AND the full skill application). Skipped entirely
    // when the caller supplied a runtime_contract — a hand-built contract is
    // the expert full-override channel, so skill application is disabled.
    let scope = if runtime_contract_value.is_none() {
        Some(crate::services::runtime::agent_mcp::resolve_agent_scope(
            &project_root_path,
            &tool,
            args.agent.as_deref(),
            args.skill.as_deref(),
        )?)
    } else {
        None
    };
    let skill_application =
        scope.as_ref().and_then(|scope| scope.skill_application.as_ref()).filter(|skill| !skill.is_empty());

    // Model / timeout precedence: explicit flag > context-json > skill
    // preference > compiled default.
    let model = args
        .model
        .clone()
        .or_else(|| context_str(context_object.as_ref(), "model"))
        .or_else(|| skill_application.and_then(|skill| skill.model.clone()))
        .unwrap_or_else(|| protocol::default_model_for_tool(&tool).unwrap_or("claude-sonnet-4-6").to_string());
    let timeout_secs = timeout_secs.or_else(|| skill_application.and_then(|skill| skill.timeout_secs));

    // Skill env entries the run forwards via `SessionRequest.env_vars`.
    // Explicit caller env always wins on collision (today the ad-hoc path
    // supplies no caller env, but the guard keeps the precedence rule
    // load-bearing if a channel is added).
    let mut env_vars: Vec<(String, String)> = Vec::new();

    // When the caller did NOT supply a runtime_contract (neither
    // `--runtime-contract-json` nor a `runtime_contract` key in
    // `--context-json`), assemble one from the agent's profile/skill MCP
    // servers so an ad-hoc `animus agent run` agent sees the MCP servers its
    // profile/skill declares. A caller-supplied contract is never clobbered.
    if let Some(scope) = scope.as_ref() {
        let scope_selected = args.agent.is_some() || args.skill.is_some();
        let mut contract = crate::services::runtime::agent_mcp::assemble_agent_mcp_contract(
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
        )?;

        // Apply the REST of the resolved skill (its MCP servers + tool
        // policy already rode the contract assembly above, and its model /
        // timeout preferences fed the precedence chain earlier): prompt
        // fragments, system-prompt fragments, env, and launch-affecting
        // fields (extra_args / codex_config_overrides). Precedence for every
        // field: explicit CLI flag / context-json value > skill > defaults.
        if let Some(skill) = skill_application {
            // Prompt body: prefixes, "Skill directives:" section, body,
            // suffixes — the same ordering the workflow phase renderer uses.
            prompt = animus_runtime_shared::apply_skill_prompt_to_body(&prompt, skill);

            // System prompt: an explicit `--context-json system_prompt`
            // comes FIRST, then the skill's fragments (matching the
            // workflow renderer where the configured system prompt precedes
            // skill fragments).
            let explicit_system_prompt = extras.get("system_prompt").and_then(Value::as_str).map(ToOwned::to_owned);
            if let Some(merged) =
                animus_runtime_shared::merge_skill_system_prompt(explicit_system_prompt.as_deref(), skill)
            {
                extras.insert("system_prompt".to_string(), Value::String(merged));
            }

            // Skill env → `SessionRequest.env_vars`. Note the plugin host
            // still gates the forwarded env against the provider plugin's
            // manifest `env_required` (same gate the workflow path's
            // launch-env channel passes through).
            for (key, value) in &skill.env {
                if !env_vars.iter().any(|(existing, _)| existing == key) {
                    env_vars.push((key.clone(), value.clone()));
                }
            }

            // Launch-affecting fields ride the SAME mechanism the workflow
            // path uses: `runtime_contract.cli.launch`. The launch block is
            // grafted ONLY when the skill actually declares such fields, so
            // runs without them keep the provider's own launch behavior.
            if skill_has_launch_extras(skill) {
                if let Some(grafted) = graft_skill_launch_contract(
                    contract.as_ref(),
                    &tool,
                    &model,
                    &prompt,
                    permission_mode.as_deref(),
                    extras.get("reasoning_effort").and_then(Value::as_str),
                    skill,
                ) {
                    contract = Some(grafted);
                }
            }
        }

        if let Some(contract) = contract {
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
        permission_mode,
        timeout_secs,
        env_vars,
        extras: Value::Object(extras),
    })
}

/// Whether a skill application carries launch-affecting fields that require
/// a `runtime_contract.cli.launch` block on the ad-hoc paths: `extra_args`,
/// `codex_config_overrides`, or launch `env`.
pub(crate) fn skill_has_launch_extras(skill: &SkillApplicationResult) -> bool {
    !skill.extra_args.is_empty() || !skill.codex_config_overrides.is_empty() || !skill.env.is_empty()
}

/// Graft a `cli.launch` block carrying the skill's launch-affecting fields
/// onto an assembled ad-hoc runtime contract.
///
/// The ad-hoc contract assembler deliberately strips `cli.launch` (it is
/// built from an empty placeholder prompt), letting the provider transport
/// drive its own launch from the request. Skills that declare `extra_args`,
/// `codex_config_overrides`, or `env` need the workflow path's mechanism —
/// the provider consumes `runtime_contract.cli.launch` wholesale — so this
/// rebuilds the launch block from the REAL final prompt and injects the
/// skill fields via the same `animus-runtime-shared` helpers the workflow
/// runner uses. Because a contract launch replaces the transport's own
/// argv assembly, the explicit `--permission-mode` and codex
/// `--reasoning-effort` values are re-applied here so CLI flags keep winning
/// over the skill.
///
/// Returns `None` when no launch contract can be built for `tool` (unknown
/// tool); the caller keeps the un-grafted contract in that case.
pub(crate) fn graft_skill_launch_contract(
    base_contract: Option<&Value>,
    tool: &str,
    model: &str,
    final_prompt: &str,
    permission_mode: Option<&str>,
    reasoning_effort: Option<&str>,
    skill: &SkillApplicationResult,
) -> Option<Value> {
    let built = animus_runtime_shared::build_runtime_contract(tool, model, final_prompt)?;
    let launch = built.pointer("/cli/launch")?.clone();
    let mut contract = base_contract.cloned().unwrap_or(built);
    {
        let cli = contract.get_mut("cli").and_then(Value::as_object_mut)?;
        cli.insert("launch".to_string(), launch);
    }
    // Skill fields first (workflow injection order: codex config overrides,
    // then extra args, then launch env)...
    animus_runtime_shared::inject_codex_config_overrides_list(&mut contract, tool, &skill.codex_config_overrides);
    animus_runtime_shared::inject_cli_extra_args_list(&mut contract, &skill.extra_args);
    animus_runtime_shared::inject_cli_launch_env(&mut contract, &skill.env);
    // ...then the EXPLICIT flags, applied last with replace semantics so a
    // skill override on the same key (e.g. `approval_policy` or
    // `model_reasoning_effort`) cannot invert the documented
    // CLI-flag-over-skill precedence.
    apply_permission_mode_to_launch(&mut contract, tool, permission_mode);
    if let Some(effort) = reasoning_effort.map(str::trim).filter(|effort| !effort.is_empty()) {
        animus_runtime_shared::inject_codex_config_overrides_list(
            &mut contract,
            tool,
            &[format!("model_reasoning_effort={}", effort.to_ascii_lowercase())],
        );
    }
    Some(contract)
}

/// Re-apply an explicit permission mode onto a grafted launch block so the
/// CLI flag keeps winning once the transport's own argv assembly (which maps
/// `SessionRequest.permission_mode`) is replaced by the contract launch.
/// claude: swap the default `--dangerously-skip-permissions` for
/// `--permission-mode <mode>`; codex: upsert `-c approval_policy="<mode>"`.
/// Other tools pass through (their transports do not map a mode flag today).
fn apply_permission_mode_to_launch(contract: &mut Value, tool: &str, permission_mode: Option<&str>) {
    let Some(mode) = permission_mode.map(str::trim).filter(|mode| !mode.is_empty()) else {
        return;
    };
    match tool.trim().to_ascii_lowercase().as_str() {
        "claude" => {
            if let Some(args) = contract.pointer_mut("/cli/launch/args").and_then(Value::as_array_mut) {
                // The explicit mode replaces the skip-permissions default
                // (including a duplicate a skill's extra_args may have added).
                args.retain(|arg| arg.as_str() != Some("--dangerously-skip-permissions"));
                if let Some(pos) = args.iter().position(|arg| arg.as_str() == Some("--permission-mode")) {
                    // A skill's extra_args may have injected its own mode —
                    // overwrite the value so explicit > skill holds.
                    if pos + 1 < args.len() {
                        args[pos + 1] = Value::String(mode.to_string());
                    } else {
                        args.push(Value::String(mode.to_string()));
                    }
                } else {
                    let insert_at = 1.min(args.len());
                    args.insert(insert_at, Value::String("--permission-mode".to_string()));
                    args.insert(insert_at + 1, Value::String(mode.to_string()));
                }
            }
        }
        "codex" => {
            animus_runtime_shared::inject_codex_config_overrides_list(
                contract,
                "codex",
                &[format!("approval_policy=\"{mode}\"")],
            );
        }
        _ => {}
    }
}

/// Resolve the `permission_mode` declared on an agent profile. Reads the
/// compiled agent runtime config, which already folds workflow YAML
/// `agents:` overlays onto the builtin profiles.
pub(crate) fn profile_permission_mode(project_root: &Path, agent_id: Option<&str>) -> Option<String> {
    let agent_id = agent_id?;
    orchestrator_core::load_agent_runtime_config_or_default(project_root)
        .agent_profile(agent_id)
        .and_then(|profile| profile.permission_mode.as_deref())
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .map(ToOwned::to_owned)
}

/// Whether the agent profile declares an `approval_policy`. Reads the
/// compiled agent runtime config, same as [`profile_permission_mode`].
pub(crate) fn profile_has_approval_policy(project_root: &Path, agent_id: Option<&str>) -> bool {
    let Some(agent_id) = agent_id else {
        return false;
    };
    orchestrator_core::load_agent_runtime_config_or_default(project_root)
        .agent_profile(agent_id)
        .is_some_and(|profile| profile.approval_policy.is_some())
}

/// Warn on stderr when a permission mode is not in the union of values any
/// known provider accepts. Modes are provider-specific and forwarded
/// verbatim, so this never blocks — it only catches likely typos.
pub(crate) fn warn_unknown_permission_mode(mode: &str) {
    if !orchestrator_config::agent_runtime_config::is_known_permission_mode(mode) {
        eprintln!(
            "warning: permission mode '{mode}' is not a known value for any provider \
             (claude: default|acceptEdits|bypassPermissions|plan; \
             codex: untrusted|on-failure|on-request|never; \
             gemini: default|auto_edit|yolo); passing it through verbatim"
        );
    }
}

/// Start a session through the resolver for the supplied request.
///
/// A missing provider plugin is surfaced as a typed error carrying a
/// structured `remediation` payload (with the exact install command from
/// `provider_install_command`) so machine callers don't have to scrape the
/// human-readable message. The message text matches the plain resolver
/// error path exactly.
pub(crate) async fn start_session(project_root: &Path, request: SessionRequest) -> Result<SessionRun> {
    let resolver = resolver_for(project_root);
    if let Err(err) = resolver.resolve(&request) {
        let install_command = orchestrator_plugin_host::session::provider_install_command(&request.tool);
        return Err(crate::error_with_remediation(
            crate::CliErrorKind::Internal,
            format!("provider session failed: {err}"),
            crate::missing_plugin_remediation(
                install_command,
                "Install the provider plugin for this tool, then re-run the agent command.",
            ),
        ));
    }
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
/// binary). Used by `animus plugin status` and the daemon health
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_env_lock;
    use crate::AgentRunArgs;
    use protocol::test_utils::EnvVarGuard;

    fn base_args(project_root: &str) -> AgentRunArgs {
        AgentRunArgs {
            run_id: None,
            tool: "claude".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
            prompt: Some("hi".to_string()),
            reasoning_effort: None,
            permission_mode: None,
            approvals: false,
            cwd: Some(project_root.to_string()),
            timeout_secs: None,
            context_json: None,
            runtime_contract_json: None,
            detach: false,
            stream: true,
            save_jsonl: false,
            jsonl_dir: None,
            start_runner: false,
            agent: None,
            skill: None,
            mcp_server: Vec::new(),
            no_animus_mcp: false,
        }
    }

    #[tokio::test]
    async fn start_session_missing_provider_carries_structured_remediation() {
        // A tool name no plugin can ever register keeps this hermetic even
        // on machines with real provider plugins installed/discoverable.
        let tool = "definitely-not-a-real-provider-zz9";
        let tmp = tempfile::tempdir().unwrap();
        let request = SessionRequest {
            tool: tool.to_string(),
            model: String::new(),
            prompt: "hello".to_string(),
            cwd: tmp.path().to_path_buf(),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: serde_json::json!({}),
        };
        let err = start_session(tmp.path(), request).await.expect_err("missing provider must error");
        let message = err.to_string();
        assert!(message.contains("provider session failed"), "human text preserved: {message}");
        assert!(message.contains("not installed"), "human text preserved: {message}");
        let details = crate::extract_cli_error_details(&err).expect("structured remediation details");
        assert_eq!(details.pointer("/remediation/kind").and_then(Value::as_str), Some("missing_plugin"));
        let install_command =
            details.pointer("/remediation/install_command").and_then(Value::as_str).expect("install command present");
        assert!(
            install_command.contains(&format!("animus plugin install <publisher>/animus-provider-{tool}")),
            "install command is structured, not scraped: {install_command}"
        );
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

        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&root);
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
    fn agent_run_without_permission_mode_leaves_request_field_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let args = base_args(&root_str);
        let request = session_request_from_args(&args, &root_str).expect("request builds");
        assert!(request.permission_mode.is_none(), "no flag/profile/context must leave permission_mode unset");
    }

    #[test]
    fn agent_run_permission_mode_flag_rides_the_typed_request_field() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let mut args = base_args(&root_str);
        args.permission_mode = Some("acceptEdits".to_string());
        let request = session_request_from_args(&args, &root_str).expect("request builds");
        assert_eq!(request.permission_mode.as_deref(), Some("acceptEdits"));
        assert!(
            request.extras.pointer("/permission_mode").is_none(),
            "permission_mode must ride the typed field, not extras; extras: {}",
            request.extras
        );
    }

    #[test]
    fn agent_run_permission_mode_resolves_from_the_agent_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();
        std::fs::create_dir_all(root.join(".animus")).unwrap();
        std::fs::write(
            root.join(".animus").join("workflows.yaml"),
            r#"
tools_allowlist:
  - cargo
agents:
  cautious:
    description: "Cautious agent"
    permission_mode: plan
phases:
  work:
    mode: agent
    agent_id: cautious
"#,
        )
        .unwrap();

        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&root);
        let mut args = base_args(&root_str);
        args.agent = Some("cautious".to_string());
        let request = session_request_from_args(&args, &root_str).expect("request builds");
        assert_eq!(
            request.permission_mode.as_deref(),
            Some("plan"),
            "the --agent profile's permission_mode must reach the request"
        );
    }

    #[test]
    fn agent_run_permission_mode_flag_wins_over_the_agent_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();
        std::fs::create_dir_all(root.join(".animus")).unwrap();
        std::fs::write(
            root.join(".animus").join("workflows.yaml"),
            r#"
agents:
  cautious:
    description: "Cautious agent"
    permission_mode: plan
"#,
        )
        .unwrap();

        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&root);
        let mut args = base_args(&root_str);
        args.agent = Some("cautious".to_string());
        args.permission_mode = Some("bypassPermissions".to_string());
        let request = session_request_from_args(&args, &root_str).expect("request builds");
        assert_eq!(
            request.permission_mode.as_deref(),
            Some("bypassPermissions"),
            "the explicit --permission-mode flag must win over the profile"
        );
    }

    #[test]
    fn agent_run_unknown_permission_mode_passes_through_without_blocking() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let mut args = base_args(&root_str);
        args.permission_mode = Some("totally-custom-mode".to_string());
        let request = session_request_from_args(&args, &root_str).expect("an unknown mode must not block the run");
        assert_eq!(request.permission_mode.as_deref(), Some("totally-custom-mode"));
    }

    #[test]
    fn agent_run_without_approvals_leaves_extras_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let args = base_args(&root_str);
        let request = session_request_from_args(&args, &root_str).expect("request builds");
        assert!(
            request.extras.pointer("/approvals").is_none(),
            "no flag and no profile policy must leave extras.approvals absent; extras: {}",
            request.extras
        );
    }

    #[test]
    fn agent_run_approvals_flag_sets_extras_approvals() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let mut args = base_args(&root_str);
        args.approvals = true;
        let request = session_request_from_args(&args, &root_str).expect("request builds");
        assert_eq!(request.extras.pointer("/approvals").and_then(Value::as_bool), Some(true));
    }

    #[test]
    fn agent_run_profile_approval_policy_sets_extras_approvals() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();
        std::fs::create_dir_all(root.join(".animus")).unwrap();
        std::fs::write(
            root.join(".animus").join("workflows.yaml"),
            r#"
tools_allowlist:
  - cargo
agents:
  gated:
    description: "Gated agent"
    approval_policy:
      default: ask
phases:
  work:
    mode: agent
    agent_id: gated
"#,
        )
        .unwrap();

        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&root);
        let mut args = base_args(&root_str);
        args.agent = Some("gated".to_string());
        let request = session_request_from_args(&args, &root_str).expect("request builds");
        assert_eq!(
            request.extras.pointer("/approvals").and_then(Value::as_bool),
            Some(true),
            "an --agent profile with an approval_policy must set extras.approvals"
        );
    }

    #[test]
    fn agent_run_permission_mode_and_approval_policy_compose_independently() {
        // `permission_mode` and `approval_policy` on the SAME profile are two
        // orthogonal layers and must BOTH take effect — they compose, neither
        // overrides the other. `permission_mode` is the transport-level guard
        // (rides the typed SessionRequest.permission_mode → provider's own
        // permission flag); `approval_policy` is the kernel inbox layer (its
        // mere presence flips extras.approvals=true so the provider routes
        // escalations through animus.agent.request_approval, where the policy
        // then auto-allows/denies/asks). This test pins that both signals
        // survive on one request.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();
        std::fs::create_dir_all(root.join(".animus")).unwrap();
        std::fs::write(
            root.join(".animus").join("workflows.yaml"),
            r#"
tools_allowlist:
  - cargo
agents:
  guarded:
    description: "Both-layer agent"
    permission_mode: plan
    approval_policy:
      default: ask
      auto_deny: ["git.push*"]
phases:
  work:
    mode: agent
    agent_id: guarded
"#,
        )
        .unwrap();

        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(&root);
        let mut args = base_args(&root_str);
        args.agent = Some("guarded".to_string());
        let request = session_request_from_args(&args, &root_str).expect("request builds");

        // Transport-level guard: permission_mode reaches the typed field.
        assert_eq!(
            request.permission_mode.as_deref(),
            Some("plan"),
            "the profile's permission_mode must ride the transport-level guard"
        );
        // Kernel inbox layer: approval_policy presence flips extras.approvals.
        assert_eq!(
            request.extras.pointer("/approvals").and_then(Value::as_bool),
            Some(true),
            "the profile's approval_policy must enable kernel-mediated approvals; they compose, not conflict; extras: {}",
            request.extras
        );
    }

    /// Write a project-scoped standalone skill definition named `test-skill`.
    fn write_test_skill(root: &Path, yaml: &str) {
        let dir = root.join(".animus").join("config").join("skill_definitions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("test-skill.yaml"), yaml).unwrap();
    }

    const PROMPT_SKILL_YAML: &str = r#"
name: test-skill
prompt:
  system: You are skill-guided.
  prefix: PREFIX-TEXT
  suffix: SUFFIX-TEXT
  directives:
    - Directive one
"#;

    const LAUNCH_SKILL_YAML: &str = r#"
name: test-skill
extra_args:
  - "--strict-mcp-config"
env:
  SKILL_MODE: "on"
"#;

    const CODEX_OVERRIDE_SKILL_YAML: &str = r#"
name: test-skill
codex_config_overrides:
  - model_reasoning_effort="high"
"#;

    /// Build a request for a tmp project that has `test-skill` defined,
    /// pinning HOME so user-scoped skill dirs under `~` never leak in.
    fn request_with_skill(yaml: &str, mutate: impl FnOnce(&mut AgentRunArgs)) -> SessionRequest {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", Some(tmp.path().to_string_lossy().as_ref()));
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();
        write_test_skill(&root, yaml);

        let mut args = base_args(&root_str);
        args.skill = Some("test-skill".to_string());
        mutate(&mut args);
        session_request_from_args(&args, &root_str).expect("request builds")
    }

    #[test]
    fn skill_prompt_fragments_wrap_the_prompt_and_set_the_system_prompt() {
        let request = request_with_skill(PROMPT_SKILL_YAML, |_| {});
        assert_eq!(request.prompt, "PREFIX-TEXT\n\nSkill directives:\n- Directive one\n\nhi\n\nSUFFIX-TEXT");
        assert_eq!(
            request.extras.pointer("/system_prompt").and_then(Value::as_str),
            Some("You are skill-guided."),
            "skill system fragment must ride extras.system_prompt; extras: {}",
            request.extras
        );
        // Prompt-only skills do NOT graft a launch block — the provider keeps
        // its own launch behavior.
        if let Some(contract) = request.extras.pointer("/runtime_contract") {
            assert!(contract.pointer("/cli/launch").is_none(), "prompt-only skill must not graft cli.launch");
        }
        assert!(request.env_vars.is_empty(), "prompt-only skill contributes no env");
    }

    #[test]
    fn explicit_context_json_system_prompt_precedes_the_skill_fragment() {
        let request = request_with_skill(PROMPT_SKILL_YAML, |args| {
            args.context_json = Some(r#"{"system_prompt":"EXPLICIT"}"#.to_string());
        });
        assert_eq!(
            request.extras.pointer("/system_prompt").and_then(Value::as_str),
            Some("EXPLICIT\n\nYou are skill-guided."),
            "the explicit context-json system_prompt must come FIRST; extras: {}",
            request.extras
        );
    }

    #[test]
    fn skill_extra_args_and_env_are_grafted_onto_the_launch_contract() {
        let request = request_with_skill(LAUNCH_SKILL_YAML, |_| {});
        let contract = request.extras.pointer("/runtime_contract").expect("contract assembled");
        let args: Vec<&str> = contract
            .pointer("/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch-affecting skill must graft cli.launch")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let prompt_pos = args.iter().position(|a| *a == request.prompt).expect("launch carries the final prompt");
        let extra_pos = args.iter().position(|a| *a == "--strict-mcp-config").expect("skill extra arg present");
        assert!(extra_pos < prompt_pos, "extra args insert before the trailing prompt; args: {args:?}");
        assert_eq!(
            contract.pointer("/cli/launch/env/SKILL_MODE").and_then(Value::as_str),
            Some("on"),
            "skill env must ride cli.launch.env; contract: {contract}"
        );
        assert_eq!(
            request.env_vars,
            vec![("SKILL_MODE".to_string(), "on".to_string())],
            "skill env must also ride SessionRequest.env_vars"
        );
    }

    #[test]
    fn skill_codex_overrides_apply_to_codex_and_not_to_claude() {
        let codex_request = request_with_skill(CODEX_OVERRIDE_SKILL_YAML, |args| {
            args.tool = "codex".to_string();
            args.model = Some("gpt-5.2-codex".to_string());
        });
        let codex_args: Vec<String> = codex_request
            .extras
            .pointer("/runtime_contract/cli/launch/args")
            .and_then(Value::as_array)
            .expect("codex skill must graft cli.launch")
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect();
        assert!(
            codex_args.iter().any(|a| a == "model_reasoning_effort=\"high\""),
            "codex override must land in launch args: {codex_args:?}"
        );

        let claude_request = request_with_skill(CODEX_OVERRIDE_SKILL_YAML, |_| {});
        let claude_args: Vec<String> = claude_request
            .extras
            .pointer("/runtime_contract/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch grafted (codex_config_overrides is launch-affecting)")
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect();
        assert!(
            !claude_args.iter().any(|a| a.contains("model_reasoning_effort")),
            "codex overrides are codex-gated; claude args: {claude_args:?}"
        );
    }

    const MODEL_SKILL_YAML: &str = r#"
name: test-skill
model:
  preferred: claude-opus-4-1
timeout_secs: 900
"#;

    #[test]
    fn skill_model_and_timeout_apply_when_no_explicit_values_are_given() {
        let request = request_with_skill(MODEL_SKILL_YAML, |args| {
            args.model = None;
            args.timeout_secs = None;
        });
        assert_eq!(request.model, "claude-opus-4-1", "skill model preference must beat the compiled default");
        assert_eq!(request.timeout_secs, Some(900), "skill timeout_secs must apply when no flag is given");
    }

    #[test]
    fn explicit_model_and_timeout_win_over_the_skill_preference() {
        let request = request_with_skill(MODEL_SKILL_YAML, |args| {
            args.model = Some("claude-sonnet-4-6".to_string());
            args.timeout_secs = Some(60);
        });
        assert_eq!(request.model, "claude-sonnet-4-6");
        assert_eq!(request.timeout_secs, Some(60));
    }

    #[test]
    fn explicit_reasoning_effort_wins_over_a_skill_codex_override() {
        let yaml = r#"
name: test-skill
codex_config_overrides:
  - model_reasoning_effort="low"
"#;
        let request = request_with_skill(yaml, |args| {
            args.tool = "codex".to_string();
            args.model = Some("gpt-5.2-codex".to_string());
            args.reasoning_effort = Some(crate::cli_types::ReasoningEffortArg::High);
        });
        let args: Vec<&str> = request
            .extras
            .pointer("/runtime_contract/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch grafted")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            args.contains(&"model_reasoning_effort=high"),
            "explicit --reasoning-effort must replace the skill's override: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.contains("model_reasoning_effort=\"low\"")),
            "the skill's conflicting override must be replaced: {args:?}"
        );
    }

    #[test]
    fn explicit_permission_mode_survives_the_skill_launch_graft() {
        let request = request_with_skill(LAUNCH_SKILL_YAML, |args| {
            args.permission_mode = Some("plan".to_string());
        });
        let args: Vec<&str> = request
            .extras
            .pointer("/runtime_contract/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch grafted")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let mode_pos = args.iter().position(|a| *a == "--permission-mode").expect("explicit mode must survive");
        assert_eq!(args.get(mode_pos + 1).copied(), Some("plan"));
        assert!(
            !args.contains(&"--dangerously-skip-permissions"),
            "--permission-mode replaces the skip-permissions default; args: {args:?}"
        );
    }

    #[test]
    fn explicit_permission_mode_overwrites_a_skill_supplied_mode_in_extra_args() {
        let yaml = r#"
name: test-skill
extra_args:
  - "--permission-mode"
  - acceptEdits
"#;
        let request = request_with_skill(yaml, |args| {
            args.permission_mode = Some("plan".to_string());
        });
        let args: Vec<&str> = request
            .extras
            .pointer("/runtime_contract/cli/launch/args")
            .and_then(Value::as_array)
            .expect("launch grafted")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let mode_positions: Vec<usize> =
            args.iter().enumerate().filter(|(_, a)| **a == "--permission-mode").map(|(i, _)| i).collect();
        assert_eq!(mode_positions.len(), 1, "exactly one --permission-mode flag; args: {args:?}");
        assert_eq!(args.get(mode_positions[0] + 1).copied(), Some("plan"), "explicit mode must win; args: {args:?}");
        assert!(!args.contains(&"acceptEdits"), "the skill's mode value must be replaced; args: {args:?}");
        assert!(!args.contains(&"--dangerously-skip-permissions"), "no skip-permissions default; args: {args:?}");
    }

    #[test]
    fn caller_supplied_runtime_contract_disables_skill_application() {
        let request = request_with_skill(PROMPT_SKILL_YAML, |args| {
            args.runtime_contract_json = Some(r#"{"cli":{"name":"claude"},"mcp":{}}"#.to_string());
        });
        assert_eq!(request.prompt, "hi", "a caller-supplied contract is a full override; skill prompt must not apply");
        assert!(request.extras.pointer("/system_prompt").is_none());
        assert!(request.env_vars.is_empty());
    }

    #[test]
    fn no_skill_run_is_a_noop_for_prompt_system_prompt_and_env() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", Some(tmp.path().to_string_lossy().as_ref()));
        let root = tmp.path().canonicalize().unwrap();
        let root_str = root.to_string_lossy().into_owned();

        let args = base_args(&root_str);
        let request = session_request_from_args(&args, &root_str).expect("request builds");
        assert_eq!(request.prompt, "hi");
        assert!(request.extras.pointer("/system_prompt").is_none());
        assert!(request.env_vars.is_empty());
        let contract = request.extras.pointer("/runtime_contract").expect("contract assembled");
        assert!(contract.pointer("/cli/launch").is_none(), "no skill → launch stays stripped");
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
