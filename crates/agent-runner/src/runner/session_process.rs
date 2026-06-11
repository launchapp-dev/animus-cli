use super::launch::{is_ai_cli_tool, LaunchInvocation};
use animus_session_backend::session::{SessionEvent, SessionRequest};
use anyhow::{anyhow, bail, Context, Result};
use orchestrator_session_host::SessionBackendResolver;
use protocol::{
    AgentRunEvent, ArtifactInfo, ArtifactType, OutputStreamType, RunId, Timestamp, TokenUsage, ToolCallInfo,
    ToolResultInfo,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{Duration, MissedTickBehavior};
use tracing::{debug, info, warn};

use super::mcp_policy::{apply_native_mcp_policy, resolve_mcp_tool_enforcement, TempPathCleanup};
use super::process_builder::{build_cli_invocation, merge_launch_env, resolve_idle_timeout_secs};
use crate::cleanup::{track_process, untrack_process};

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2).find_map(|pair| (pair[0] == flag).then_some(pair[1].as_str()))
}

fn truncate_for_log(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…")
}

fn persist_run_session_sidecar(run_id: &RunId, session_id: &str, tool: &str) -> anyhow::Result<()> {
    let runs_root = std::env::var_os("ANIMUS_RUNNER_SESSION_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".animus").join("runner-sessions")));
    let Some(runs_root) = runs_root else {
        return Ok(());
    };
    std::fs::create_dir_all(&runs_root).context("create runner sessions dir")?;
    let path = runs_root.join(format!("{}.session.json", sanitize_run_id(run_id.0.as_str())));
    let payload = json!({
        "run_id": run_id.0,
        "session_id": session_id,
        "tool": tool,
        "recorded_at": chrono::Utc::now().to_rfc3339(),
    });
    let tmp = path.with_extension("session.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&payload)?).context("write sidecar")?;
    std::fs::rename(&tmp, &path).context("rename sidecar")?;
    Ok(())
}

fn sanitize_run_id(value: &str) -> String {
    value.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect()
}

pub(super) fn use_native_session_backend(tool: &str, _runtime_contract: Option<&Value>) -> bool {
    matches!(
        tool.to_ascii_lowercase().as_str(),
        "claude" | "codex" | "gemini" | "opencode" | "oai-runner" | "animus-oai-runner"
    )
}

pub(super) fn require_native_session_backend(tool: &str, runtime_contract: Option<&Value>) -> Result<()> {
    if !is_ai_cli_tool(tool) {
        return Ok(());
    }

    if use_native_session_backend(tool, runtime_contract) {
        return Ok(());
    }

    bail!("native session backend is required for AI tool '{}' but is not available", tool);
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn spawn_session_process(
    tool: &str,
    model: &str,
    prompt: &str,
    runtime_contract: Option<&Value>,
    mcp_servers: Option<&Value>,
    cwd: &str,
    project_root: Option<&str>,
    env: HashMap<String, String>,
    timeout_secs: Option<u64>,
    run_id: &RunId,
    event_tx: mpsc::Sender<AgentRunEvent>,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
    resume_session_id: Option<&str>,
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
    let session_request = build_session_request(
        tool,
        model,
        prompt,
        runtime_contract,
        mcp_servers,
        cwd,
        project_root,
        env,
        timeout_secs,
        invocation,
    )?;
    let idle_timeout_secs = resolve_idle_timeout_secs(tool, timeout_secs, runtime_contract);
    // Project-local provider plugins live under `<project_root>/.animus/plugins`.
    // When the runner executes inside a managed worktree, `cwd` points at the
    // worktree (e.g. `~/.animus/<scope>/worktrees/...`) which has no
    // `.animus/plugins/` directory. Discover plugins against `project_root`
    // when the supervisor provided it; fall back to `cwd` for callers that
    // do not yet thread it through.
    let discovery_root = project_root.map(std::path::Path::new).unwrap_or_else(|| std::path::Path::new(cwd));
    let resolver = SessionBackendResolver::with_plugin_discovery(discovery_root);
    let backend = resolver.resolve(&session_request).context("failed to resolve provider plugin")?;
    let mut run = match resume_session_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(session_id) => backend
            .resume_session(session_request, session_id)
            .await
            .context("failed to resume native session backend")?,
        None => backend.start_session(session_request).await.context("failed to start native session backend")?,
    };

    if let Some(pid) = run.pid {
        if let Err(err) = crate::cleanup::track_process(run_id.0.as_str(), pid) {
            warn!(
                run_id = %run_id.0.as_str(),
                pid,
                error = %err,
                "Failed to register process in orphan tracker"
            );
        }
    }

    let mut run_session_id = run.session_id.clone();
    if let Some(ref session_id) = run_session_id {
        if let Err(err) = persist_run_session_sidecar(run_id, session_id, tool) {
            warn!(
                run_id = %run_id.0.as_str(),
                %err,
                "Failed to persist native session sidecar"
            );
        }
    }
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

    let result: Result<i32> = loop {
        tokio::select! {
            maybe_event = run.events.recv() => {
                let Some(event) = maybe_event else {
                    break Err(anyhow!("native session backend closed event stream unexpectedly"));
                };

                if !matches!(event, SessionEvent::Started { .. }) {
                    last_activity_at = Instant::now();
                }

                // Plugin-backed providers initially expose a host-local
                // control_session_id via `SessionRun.session_id`, then emit a
                // second `Started` event carrying the plugin's REAL
                // session_id once the agent/run response arrives. Re-persist
                // the sidecar (and update the local cache used for cancel /
                // idle-terminate) so downstream resume reads the provider's
                // real id instead of the host's control id.
                if let SessionEvent::Started { session_id: Some(emitted_sid), .. } = &event {
                    let emitted_trimmed = emitted_sid.trim();
                    if !emitted_trimmed.is_empty()
                        && run_session_id.as_deref().map(str::trim) != Some(emitted_trimmed)
                    {
                        if let Err(err) = persist_run_session_sidecar(run_id, emitted_trimmed, tool) {
                            warn!(
                                run_id = %run_id.0.as_str(),
                                %err,
                                "Failed to refresh native session sidecar with provider id"
                            );
                        }
                        run_session_id = Some(emitted_trimmed.to_string());
                    }
                }

                if let Some(exit_code) = forward_session_event(run_id, &event, &event_tx).await {
                    break Ok(exit_code);
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
                        break Err(anyhow!("Process idle timeout after {}s without activity", idle_limit_secs));
                    }
                }
            }
            _ = &mut cancel_rx => {
                if let Some(session_id) = run_session_id.as_deref() {
                    let _ = backend.terminate_session(session_id).await;
                }
                break Err(anyhow!("Process cancelled by user"));
            }
        }
    };

    if run.pid.is_some() {
        if let Err(err) = crate::cleanup::untrack_process(run_id.0.as_str()) {
            warn!(
                run_id = %run_id.0.as_str(),
                error = %err,
                "Failed to unregister process from orphan tracker"
            );
        }
    }

    result
}

#[allow(clippy::too_many_arguments)]
fn build_session_request(
    tool: &str,
    model: &str,
    prompt: &str,
    runtime_contract: Option<&Value>,
    mcp_servers: Option<&Value>,
    cwd: &str,
    project_root: Option<&str>,
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

    // Forward the host's resolved per-phase MCP server map (canonical
    // name-keyed `.mcp.json`-style entries) onto the typed
    // `SessionRequest.mcp_servers` channel so BOTH the native subprocess
    // backends and the provider-plugin RPC see the same servers the
    // runtime contract carries. Empty maps are normalized to `None`.
    let mcp_servers = mcp_servers.filter(|value| value.as_object().is_some_and(|map| !map.is_empty())).cloned();

    Ok(SessionRequest {
        tool: tool.to_string(),
        model: model.to_string(),
        prompt: prompt.to_string(),
        cwd: std::path::PathBuf::from(cwd),
        project_root: project_root.map(std::path::PathBuf::from),
        mcp_endpoint: merged_contract.pointer("/mcp/endpoint").and_then(Value::as_str).map(ToString::to_string),
        mcp_servers,
        permission_mode: None,
        timeout_secs,
        env_vars: merged_env.into_iter().collect(),
        extras: json!({
            "runtime_contract": merged_contract
        }),
    })
}

async fn forward_session_event(
    run_id: &RunId,
    event: &SessionEvent,
    event_tx: &mpsc::Sender<AgentRunEvent>,
) -> Option<i32> {
    match event {
        SessionEvent::Started { backend, session_id, pid } => {
            debug!(
                run_id = %run_id.0.as_str(),
                backend,
                session_id = ?session_id,
                pid = ?pid,
                "Native session backend started"
            );
            if let Some(pid) = pid {
                if let Err(e) = track_process(&run_id.0, *pid) {
                    warn!(
                        run_id = %run_id.0.as_str(),
                        pid,
                        error = %e,
                        "Failed to record native session process in orphan tracker"
                    );
                }
            }
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
        SessionEvent::Finished { exit_code } => {
            if let Err(e) = untrack_process(&run_id.0) {
                warn!(
                    run_id = %run_id.0.as_str(),
                    error = %e,
                    "Failed to remove native session process from orphan tracker"
                );
            }
            Some(exit_code.unwrap_or(0))
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::{mpsc, oneshot};

    fn unique_test_dir(label: &str) -> PathBuf {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock should be valid").as_nanos();
        std::env::temp_dir().join(format!("animus-agent-runner-{label}-{suffix}"))
    }

    #[cfg(unix)]
    fn write_capture_cli_shim(dir: &Path, binary_name: &str, fixture_path: &str) -> std::io::Result<PathBuf> {
        let script_path = dir.join(binary_name);
        let script = format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$ANIMUS_TEST_ARGS_CAPTURE\"\nenv | sort > \"$ANIMUS_TEST_ENV_CAPTURE\"\ncat \"{}\"\n",
            fixture_path
        );
        fs::write(&script_path, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script_path)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script_path, permissions)?;
        }
        Ok(script_path)
    }

    fn read_capture_lines(path: &Path) -> Vec<String> {
        fs::read_to_string(path).expect("capture file should exist").lines().map(ToString::to_string).collect()
    }

    #[test]
    fn native_session_backend_enabled_when_mcp_enforced() {
        let contract = json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": {
                "endpoint": "http://127.0.0.1:3101/mcp/ao",
                "enforce_only": true
            }
        });

        assert!(use_native_session_backend("claude", Some(&contract)));
    }

    #[test]
    fn native_session_backend_enabled_for_supported_tool_without_mcp_policy() {
        let contract = json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": { "enforce_only": false }
        });

        assert!(use_native_session_backend("gemini", Some(&contract)));
        assert!(use_native_session_backend("opencode", Some(&contract)));
        assert!(use_native_session_backend("oai-runner", Some(&contract)));
    }

    #[test]
    fn require_native_session_backend_accepts_mcp_only_ai_runs() {
        let contract = json!({
            "cli": { "capabilities": { "supports_mcp": true } },
            "mcp": {
                "endpoint": "http://127.0.0.1:3101/mcp/ao",
                "enforce_only": true
            }
        });

        require_native_session_backend("claude", Some(&contract)).expect("MCP-only AI run should stay on native path");
    }

    #[tokio::test]
    #[cfg(unix)]
    #[ignore = "validated the removed in-tree Claude session backend; the v0.4.12 plugin-first resolver requires a real animus-provider-claude plugin on disk"]
    async fn spawn_session_process_bridges_claude_events() {
        let run_id = RunId("run-claude".to_string());
        let claude_fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude_real.jsonl");
        let runtime_contract = json!({
            "cli": {
                "name": "claude",
                "capabilities": { "supports_mcp": true },
                "launch": {
                    "command": "sh",
                    "args": ["-c", format!("cat {claude_fixture}")],
                    "prompt_via_stdin": false
                }
            }
        });
        let (event_tx, mut event_rx) = mpsc::channel(64);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let exit_code = spawn_session_process(
            "claude",
            "claude-sonnet-4-6",
            "",
            Some(&runtime_contract),
            None,
            ".",
            None,
            HashMap::new(),
            Some(30),
            &run_id,
            event_tx,
            cancel_rx,
            None,
        )
        .await
        .expect("native claude session should succeed");

        let mut saw_metadata = false;
        let mut saw_output = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                AgentRunEvent::Metadata { .. } => saw_metadata = true,
                AgentRunEvent::OutputChunk { text, .. } if text.contains("PINEAPPLE_42") => {
                    saw_output = true;
                }
                _ => {}
            }
        }

        assert_eq!(exit_code, 0);
        assert!(saw_metadata);
        assert!(saw_output);
    }

    #[tokio::test]
    #[cfg(unix)]
    #[ignore = "validated the removed in-tree Claude session backend; the v0.4.12 plugin-first resolver requires a real animus-provider-claude plugin on disk"]
    async fn spawn_session_process_passes_claude_mcp_launch_args_and_preserves_primary_server() {
        let temp_dir = unique_test_dir("claude-mcp");
        fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let args_capture = temp_dir.join("claude.args");
        let env_capture = temp_dir.join("claude.env");
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude_real.jsonl");
        write_capture_cli_shim(&temp_dir, "claude", fixture).expect("claude shim should exist");

        let run_id = RunId("run-claude-mcp".to_string());
        let ao_binary = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug/animus");
        let workspace_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let runtime_contract = json!({
            "cli": {
                "name": "claude",
                "capabilities": { "supports_mcp": true },
                "launch": {
                    "command": "claude",
                    "args": [
                        "--print",
                        "--dangerously-skip-permissions",
                        "--verbose",
                        "--output-format",
                        "stream-json",
                        "--model",
                        "claude-sonnet-4-6",
                        "hello"
                    ],
                    "prompt_via_stdin": false
                }
            },
            "mcp": {
                "stdio": {
                    "command": ao_binary,
                    "args": ["--project-root", workspace_root, "mcp", "serve"]
                },
                "agent_id": "animus",
                "enforce_only": true,
                "additional_servers": {
                    "animus": {
                        "command": "animus",
                        "args": ["mcp", "serve"]
                    }
                }
            }
        });
        let mut env = HashMap::new();
        let original_path = std::env::var("PATH").unwrap_or_default();
        env.insert("PATH".to_string(), format!("{}:{original_path}", temp_dir.display()));
        env.insert("ANIMUS_TEST_ARGS_CAPTURE".to_string(), args_capture.to_string_lossy().to_string());
        env.insert("ANIMUS_TEST_ENV_CAPTURE".to_string(), env_capture.to_string_lossy().to_string());
        let (event_tx, mut event_rx) = mpsc::channel(64);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let exit_code = spawn_session_process(
            "claude",
            "claude-sonnet-4-6",
            "",
            Some(&runtime_contract),
            None,
            ".",
            None,
            env,
            Some(30),
            &run_id,
            event_tx,
            cancel_rx,
            None,
        )
        .await
        .expect("native claude session should succeed");

        let mut saw_output = false;
        while let Some(event) = event_rx.recv().await {
            if let AgentRunEvent::OutputChunk { text, .. } = event {
                if text.contains("PINEAPPLE_42") {
                    saw_output = true;
                }
            }
        }

        let args = read_capture_lines(&args_capture);
        assert_eq!(exit_code, 0);
        assert!(saw_output, "expected claude fixture output");
        let mcp_idx =
            args.iter().position(|arg| arg == "--mcp-config").expect("claude launch should include mcp config");
        let parsed: serde_json::Value =
            serde_json::from_str(args.get(mcp_idx + 1).expect("claude mcp config payload should exist"))
                .expect("claude mcp config should parse");
        assert_eq!(parsed.pointer("/mcpServers/animus/command").and_then(serde_json::Value::as_str), Some(ao_binary));
        assert_eq!(
            parsed.pointer("/mcpServers/animus/args").and_then(serde_json::Value::as_array).cloned(),
            Some(vec![
                serde_json::Value::String("--project-root".to_string()),
                serde_json::Value::String(workspace_root.to_string()),
                serde_json::Value::String("mcp".to_string()),
                serde_json::Value::String("serve".to_string()),
            ])
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[ignore = "validated the removed in-tree codex/gemini/oai-runner session backends; the v0.4.12 plugin-first resolver requires real animus-provider-* plugins on disk"]
    async fn spawn_session_process_bridges_codex_gemini_and_oai_runner_events() {
        for (tool, fixture, expect_metadata, expect_thinking) in [
            ("codex", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex_real.jsonl"), true, true),
            ("gemini", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gemini_real.jsonl"), true, false),
            ("oai-runner", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/oai_runner_real.jsonl"), false, false),
        ] {
            let run_id = RunId(format!("run-{tool}"));
            let runtime_contract = json!({
                "cli": {
                    "name": tool,
                    "capabilities": { "supports_mcp": true },
                    "launch": {
                        "command": "sh",
                        "args": ["-c", format!("cat {fixture}")],
                        "prompt_via_stdin": false
                    }
                }
            });
            let (event_tx, mut event_rx) = mpsc::channel(64);
            let (_cancel_tx, cancel_rx) = oneshot::channel();

            let exit_code = spawn_session_process(
                tool,
                "test-model",
                "",
                Some(&runtime_contract),
                None,
                ".",
                None,
                HashMap::new(),
                Some(30),
                &run_id,
                event_tx,
                cancel_rx,
                None,
            )
            .await
            .expect("native session should succeed");

            let mut saw_metadata = false;
            let mut saw_output = false;
            let mut saw_thinking = false;
            while let Some(event) = event_rx.recv().await {
                match event {
                    AgentRunEvent::Metadata { .. } => saw_metadata = true,
                    AgentRunEvent::OutputChunk { text, .. } if text.contains("PINEAPPLE_42") => {
                        saw_output = true;
                    }
                    AgentRunEvent::Thinking { .. } => saw_thinking = true,
                    _ => {}
                }
            }

            assert_eq!(exit_code, 0, "expected successful exit for {tool}");
            assert_eq!(saw_metadata, expect_metadata, "unexpected metadata for {tool}");
            assert!(saw_output, "expected output for {tool}");
            assert_eq!(saw_thinking, expect_thinking, "unexpected thinking signal for {tool}");
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    #[ignore = "validated the removed in-tree OpenCode session backend; the v0.4.12 plugin-first resolver requires a real animus-provider-opencode plugin on disk"]
    async fn spawn_session_process_bridges_opencode_events() {
        let run_id = RunId("run-opencode".to_string());
        let runtime_contract = json!({
            "cli": {
                "name": "opencode",
                "capabilities": { "supports_mcp": true },
                "launch": {
                    "command": "sh",
                    "args": ["-c", "printf '%s\\n%s\\n' '{\"type\":\"text\",\"text\":\"PINEAPPLE_42\"}' '{\"content\":\"PINEAPPLE_42\"}'"],
                    "prompt_via_stdin": false
                }
            }
        });
        let (event_tx, mut event_rx) = mpsc::channel(64);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let exit_code = spawn_session_process(
            "opencode",
            "test-model",
            "",
            Some(&runtime_contract),
            None,
            ".",
            None,
            HashMap::new(),
            Some(30),
            &run_id,
            event_tx,
            cancel_rx,
            None,
        )
        .await
        .expect("native opencode session should succeed");

        let mut saw_output = false;
        while let Some(event) = event_rx.recv().await {
            if let AgentRunEvent::OutputChunk { text, .. } = event {
                if text.contains("PINEAPPLE_42") {
                    saw_output = true;
                }
            }
        }

        assert_eq!(exit_code, 0);
        assert!(saw_output);
    }

    #[tokio::test]
    #[cfg(unix)]
    #[ignore = "validated the removed in-tree Gemini session backend; the v0.4.12 plugin-first resolver requires a real animus-provider-gemini plugin on disk"]
    async fn spawn_session_process_passes_gemini_mcp_launch_env_and_args() {
        let temp_dir = unique_test_dir("gemini-mcp");
        fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let args_capture = temp_dir.join("gemini.args");
        let env_capture = temp_dir.join("gemini.env");
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gemini_real.jsonl");
        write_capture_cli_shim(&temp_dir, "gemini", fixture).expect("gemini shim should exist");

        let run_id = RunId("run-gemini-mcp".to_string());
        let ao_binary = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug/animus");
        let workspace_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let runtime_contract = json!({
            "cli": {
                "name": "gemini",
                "capabilities": { "supports_mcp": true },
                "launch": {
                    "command": "gemini",
                    "args": ["--model", "gemini-2.5-pro", "--output-format", "json", "-p", "hello"],
                    "env": {
                        "SKILL_LAUNCH_ENV": "review-mode"
                    },
                    "prompt_via_stdin": false
                }
            },
            "mcp": {
                "stdio": {
                    "command": ao_binary,
                    "args": ["mcp", "serve", "--project-root", workspace_root]
                },
                "agent_id": "animus",
                "enforce_only": true
            }
        });
        let mut env = HashMap::new();
        let original_path = std::env::var("PATH").unwrap_or_default();
        env.insert("PATH".to_string(), format!("{}:{original_path}", temp_dir.display()));
        env.insert("ANIMUS_TEST_ARGS_CAPTURE".to_string(), args_capture.to_string_lossy().to_string());
        env.insert("ANIMUS_TEST_ENV_CAPTURE".to_string(), env_capture.to_string_lossy().to_string());
        let (event_tx, mut event_rx) = mpsc::channel(64);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let exit_code = spawn_session_process(
            "gemini",
            "gemini-2.5-pro",
            "",
            Some(&runtime_contract),
            None,
            ".",
            None,
            env,
            Some(30),
            &run_id,
            event_tx,
            cancel_rx,
            None,
        )
        .await
        .expect("native gemini session should succeed");

        let mut saw_output = false;
        while let Some(event) = event_rx.recv().await {
            if let AgentRunEvent::OutputChunk { text, .. } = event {
                if text.contains("PINEAPPLE_42") {
                    saw_output = true;
                }
            }
        }

        let args = read_capture_lines(&args_capture);
        let env_lines = read_capture_lines(&env_capture);
        assert_eq!(exit_code, 0);
        assert!(saw_output, "expected gemini fixture output");
        assert!(
            args.windows(2).any(|pair| pair[0] == "--allowed-mcp-server-names" && pair[1] == "animus"),
            "expected gemini launch args to include MCP allowlist, got: {args:?}"
        );
        assert!(
            env_lines.iter().any(|line| line.starts_with("GEMINI_CLI_SYSTEM_SETTINGS_PATH=")),
            "expected gemini launch env to include settings path, got: {env_lines:?}"
        );
        assert!(
            env_lines.iter().any(|line| line == "SKILL_LAUNCH_ENV=review-mode"),
            "expected gemini launch env to preserve runtime contract env, got: {env_lines:?}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[ignore = "validated the removed in-tree OAI-runner session backend; the v0.4.12 plugin-first resolver requires a real animus-provider-oai plugin on disk"]
    async fn spawn_session_process_passes_oai_runner_mcp_flag_after_run_subcommand() {
        let temp_dir = unique_test_dir("oai-runner-mcp");
        fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let args_capture = temp_dir.join("oai-runner.args");
        let env_capture = temp_dir.join("oai-runner.env");
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/oai_runner_real.jsonl");
        write_capture_cli_shim(&temp_dir, "animus-oai-runner", fixture).expect("oai-runner shim should exist");

        let run_id = RunId("run-oai-runner-mcp".to_string());
        let ao_binary = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug/animus");
        let workspace_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let runtime_contract = json!({
            "cli": {
                "name": "animus-oai-runner",
                "capabilities": { "supports_mcp": true },
                "launch": {
                    "command": "animus-oai-runner",
                    "args": ["run", "-m", "minimax/MiniMax-M2.5", "--format", "json", "hello"],
                    "prompt_via_stdin": false
                }
            },
            "mcp": {
                "stdio": {
                    "command": ao_binary,
                    "args": ["mcp", "serve", "--project-root", workspace_root]
                },
                "agent_id": "animus",
                "enforce_only": true
            }
        });
        let mut env = HashMap::new();
        let original_path = std::env::var("PATH").unwrap_or_default();
        env.insert("PATH".to_string(), format!("{}:{original_path}", temp_dir.display()));
        env.insert("ANIMUS_TEST_ARGS_CAPTURE".to_string(), args_capture.to_string_lossy().to_string());
        env.insert("ANIMUS_TEST_ENV_CAPTURE".to_string(), env_capture.to_string_lossy().to_string());
        let (event_tx, mut event_rx) = mpsc::channel(64);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let exit_code = spawn_session_process(
            "animus-oai-runner",
            "minimax/MiniMax-M2.5",
            "",
            Some(&runtime_contract),
            None,
            ".",
            None,
            env,
            Some(30),
            &run_id,
            event_tx,
            cancel_rx,
            None,
        )
        .await
        .expect("native oai-runner session should succeed");

        let mut saw_output = false;
        while let Some(event) = event_rx.recv().await {
            if let AgentRunEvent::OutputChunk { text, .. } = event {
                if text.contains("PINEAPPLE_42") {
                    saw_output = true;
                }
            }
        }

        let args = read_capture_lines(&args_capture);
        assert_eq!(exit_code, 0);
        assert!(saw_output, "expected oai-runner fixture output");
        assert_eq!(args.first().map(String::as_str), Some("run"));
        let mcp_idx =
            args.iter().position(|arg| arg == "--mcp-config").expect("oai-runner launch should include mcp config");
        assert_eq!(mcp_idx, 1, "expected mcp flag immediately after run");
    }

    #[tokio::test]
    #[cfg(unix)]
    #[ignore = "validated resume-session routing through the removed in-tree Codex session backend; the v0.4.12 plugin-first resolver requires a real animus-provider-codex plugin on disk"]
    async fn spawn_session_process_resume_session_id_routes_to_backend_resume() {
        let run_id = RunId("run-resume-codex".to_string());
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex_real.jsonl");
        let runtime_contract = json!({
            "cli": {
                "name": "codex",
                "capabilities": { "supports_mcp": true },
                "launch": {
                    "command": "sh",
                    "args": ["-c", format!("cat {fixture}")],
                    "prompt_via_stdin": false
                }
            }
        });
        let (event_tx, mut event_rx) = mpsc::channel(64);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let exit_code = spawn_session_process(
            "codex",
            "test-model",
            "",
            Some(&runtime_contract),
            None,
            ".",
            None,
            HashMap::new(),
            Some(30),
            &run_id,
            event_tx,
            cancel_rx,
            Some("existing-session-abc"),
        )
        .await
        .expect("resume session should succeed without error");

        let mut saw_output = false;
        while let Some(event) = event_rx.recv().await {
            if let AgentRunEvent::OutputChunk { text, .. } = event {
                if text.contains("PINEAPPLE_42") {
                    saw_output = true;
                }
            }
        }

        assert_eq!(exit_code, 0);
        assert!(saw_output, "expected output forwarded through resume session path");
    }

    fn empty_invocation() -> LaunchInvocation {
        LaunchInvocation {
            command: "/usr/bin/true".to_string(),
            args: vec![],
            env: std::collections::BTreeMap::new(),
            prompt_via_stdin: false,
        }
    }

    #[test]
    fn build_session_request_carries_project_root_when_supplied() {
        let req = build_session_request(
            "claude",
            "claude-sonnet-4-6",
            "hi",
            None,
            None,
            "/tmp/some-worktree",
            Some("/tmp/some-real-project-root"),
            HashMap::new(),
            None,
            empty_invocation(),
        )
        .expect("session request should build");
        assert_eq!(req.cwd, PathBuf::from("/tmp/some-worktree"));
        assert_eq!(req.project_root.as_deref(), Some(Path::new("/tmp/some-real-project-root")));
    }

    #[test]
    fn build_session_request_leaves_project_root_none_when_absent() {
        let req = build_session_request(
            "claude",
            "claude-sonnet-4-6",
            "hi",
            None,
            None,
            "/tmp/some-cwd",
            None,
            HashMap::new(),
            None,
            empty_invocation(),
        )
        .expect("session request should build");
        assert!(req.project_root.is_none());
    }

    #[test]
    fn build_session_request_forwards_mcp_servers_map() {
        let servers = serde_json::json!({
            "trading": { "type": "http", "url": "https://api.example.com/mcp", "headers": { "Authorization": "Bearer tok" } },
            "docs": { "command": "docs-mcp", "args": ["--serve"] }
        });
        let req = build_session_request(
            "claude",
            "claude-sonnet-4-6",
            "hi",
            None,
            Some(&servers),
            "/tmp/some-cwd",
            None,
            HashMap::new(),
            None,
            empty_invocation(),
        )
        .expect("session request should build");
        assert_eq!(
            req.mcp_servers,
            Some(servers),
            "the host's resolved mcp_servers map must ride the typed SessionRequest field"
        );
    }

    #[test]
    fn build_session_request_normalizes_empty_mcp_servers_to_none() {
        let empty = serde_json::json!({});
        let req = build_session_request(
            "claude",
            "claude-sonnet-4-6",
            "hi",
            None,
            Some(&empty),
            "/tmp/some-cwd",
            None,
            HashMap::new(),
            None,
            empty_invocation(),
        )
        .expect("session request should build");
        assert!(req.mcp_servers.is_none(), "an empty map must not be forwarded");
    }

    /// Exercises the bug fix: when a task runs in a managed worktree (`cwd`
    /// points at the worktree) but the project-local provider plugin lives
    /// under the REAL project root, discovery must use `project_root` —
    /// not `cwd` — to find the plugin. Drops a fake `animus-provider-*`
    /// binary under `<project_root>/.animus/plugins/` and asserts the
    /// resolver wires it up; the negative branch with `project_root` set
    /// to a worktree-style path proves the assertion is meaningful.
    #[cfg(unix)]
    #[test]
    fn discovery_uses_project_root_for_project_local_provider_plugins() {
        use std::os::unix::fs::PermissionsExt;

        // Lay out a fake project root with a project-local plugin.
        let project_root = unique_test_dir("plugin-discovery-pr");
        let plugins_dir = project_root.join(".animus").join("plugins");
        fs::create_dir_all(&plugins_dir).expect("create project-local plugins dir");
        let plugin_path = plugins_dir.join("animus-provider-h3test");
        let manifest = serde_json::json!({
            "name": "animus-provider-h3test",
            "version": "0.0.1",
            "plugin_kind": "provider",
            "description": "fake provider plugin for project-local discovery test",
            "protocol_version": "0.1.0",
            "capabilities": ["agent/run"],
        })
        .to_string();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--manifest\" ]; then\n  printf '%s' '{manifest}'\n  exit 0\nfi\nexec sleep 60\n"
        );
        fs::write(&plugin_path, script).expect("write fake plugin");
        let mut perms = fs::metadata(&plugin_path).expect("plugin metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin_path, perms).expect("plugin perms");

        // Distinct "worktree" path with no `.animus/plugins/` — mirrors what
        // the supervisor sets `cwd` to once the runner switches into the
        // managed worktree under `~/.animus/<scope>/worktrees/...`.
        let worktree = unique_test_dir("plugin-discovery-wt");
        fs::create_dir_all(&worktree).expect("create worktree");

        // Block fallback discovery sources so the test isolates the
        // project_root branch we care about. EnvVarGuard serializes
        // process-global env mutations against every other test that uses
        // the same guard so the empty dir we point at isn't observed by an
        // unrelated test running in parallel; it also restores the previous
        // value on drop even if an assertion below panics.
        let empty_plugin_dir = unique_test_dir("plugin-discovery-empty");
        fs::create_dir_all(&empty_plugin_dir).expect("create empty plugin dir");
        let empty_str = empty_plugin_dir.to_string_lossy().into_owned();
        let _plugin_path_guard = protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_PATH", Some(&empty_str));
        let _plugin_dir_guard = protocol::test_utils::EnvVarGuard::set("ANIMUS_PLUGIN_DIR", Some(&empty_str));
        let _config_dir_guard = protocol::test_utils::EnvVarGuard::set("ANIMUS_CONFIG_DIR", Some(&empty_str));

        let resolver_with_root = SessionBackendResolver::with_plugin_discovery(&project_root);
        let request_with_root = animus_session_backend::session::SessionRequest {
            tool: "h3test".to_string(),
            model: String::new(),
            prompt: "probe".to_string(),
            cwd: worktree.clone(),
            project_root: Some(project_root.clone()),
            mcp_endpoint: None,
            mcp_servers: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
        };
        let found_with_root = resolver_with_root.resolve(&request_with_root).is_ok();

        // Negative: point discovery at the worktree (the pre-fix behavior)
        // and confirm the plugin does NOT resolve. Without this branch the
        // positive test could pass for unrelated reasons.
        let resolver_without_root = SessionBackendResolver::with_plugin_discovery(&worktree);
        let found_without_root = resolver_without_root.resolve(&request_with_root).is_ok();

        let _ = fs::remove_dir_all(&project_root);
        let _ = fs::remove_dir_all(&worktree);
        let _ = fs::remove_dir_all(&empty_plugin_dir);

        assert!(
            found_with_root,
            "project-local plugin must be discoverable when SessionBackendResolver scans project_root"
        );
        assert!(
            !found_without_root,
            "negative case: project-local plugin should NOT be discoverable when discovery scans the worktree (cwd)"
        );
    }
}
