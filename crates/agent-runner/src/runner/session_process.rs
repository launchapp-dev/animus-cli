use anyhow::{bail, Context, Result};
use cli_wrapper::{
    is_ai_cli_tool, LaunchInvocation, SessionBackendResolver, SessionEvent, SessionRequest,
};
use protocol::{
    AgentRunEvent, ArtifactInfo, ArtifactType, OutputStreamType, RunId, Timestamp, TokenUsage,
    ToolCallInfo, ToolResultInfo,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{Duration, MissedTickBehavior};
use tracing::{debug, info};

use super::mcp_policy::{apply_native_mcp_policy, resolve_mcp_tool_enforcement, TempPathCleanup};
use super::process_builder::{build_cli_invocation, resolve_idle_timeout_secs};

pub(super) fn use_native_session_backend(tool: &str, _runtime_contract: Option<&Value>) -> bool {
    if !native_sessions_enabled() {
        return false;
    }

    if !matches!(
        tool.to_ascii_lowercase().as_str(),
        "claude" | "codex" | "gemini" | "opencode" | "oai-runner" | "ao-oai-runner"
    ) {
        return false;
    }

    true
}

pub(super) fn require_native_session_backend(
    tool: &str,
    runtime_contract: Option<&Value>,
) -> Result<()> {
    if !is_ai_cli_tool(tool) {
        return Ok(());
    }

    if use_native_session_backend(tool, runtime_contract) {
        return Ok(());
    }

    if !native_sessions_enabled() {
        bail!(
            "native session backend is required for AI tool '{}' but AO_AGENT_RUNNER_NATIVE_SESSIONS is disabled",
            tool
        );
    }

    bail!(
        "native session backend is required for AI tool '{}' but no native backend is implemented",
        tool
    );
}

pub(super) async fn spawn_session_process(
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
    let enforcement = resolve_mcp_tool_enforcement(runtime_contract);
    let mut temp_cleanup = TempPathCleanup::default();
    apply_native_mcp_policy(
        &mut invocation,
        &enforcement,
        &mut env,
        run_id,
        &mut temp_cleanup,
    )?;
    let session_request = build_session_request(
        tool,
        model,
        prompt,
        runtime_contract,
        cwd,
        env,
        timeout_secs,
        invocation,
    )?;
    let idle_timeout_secs = resolve_idle_timeout_secs(tool, timeout_secs, runtime_contract);
    let resolver = SessionBackendResolver::new();
    let backend = resolver.resolve(&session_request);
    let mut run = backend
        .start_session(session_request)
        .await
        .context("failed to start native session backend")?;
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

fn native_sessions_enabled() -> bool {
    std::env::var("AO_AGENT_RUNNER_NATIVE_SESSIONS")
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "" | "0" | "false" | "off" | "no")
        })
        .unwrap_or(true)
}

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

    if merged_contract
        .get("cli")
        .and_then(Value::as_object)
        .is_none()
    {
        merged_contract["cli"] = json!({});
    }
    merged_contract["cli"]["name"] = Value::String(tool.to_string());
    merged_contract["cli"]["launch"] = json!({
        "command": invocation.command,
        "args": invocation.args,
        "prompt_via_stdin": invocation.prompt_via_stdin,
    });

    Ok(SessionRequest {
        tool: tool.to_string(),
        model: model.to_string(),
        prompt: prompt.to_string(),
        cwd: std::path::PathBuf::from(cwd),
        project_root: None,
        mcp_endpoint: merged_contract
            .pointer("/mcp/endpoint")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        permission_mode: None,
        timeout_secs,
        env_vars: env.into_iter().collect(),
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
        SessionEvent::Started {
            backend,
            session_id,
        } => {
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
        SessionEvent::ToolCall {
            tool_name,
            arguments,
            server,
        } => {
            let mut parameters = arguments.clone();
            if let Some(server_name) = server {
                if let Some(obj) = parameters.as_object_mut() {
                    obj.insert("server".to_string(), Value::String(server_name.clone()));
                }
            }
            let _ = event_tx
                .send(AgentRunEvent::ToolCall {
                    run_id: run_id.clone(),
                    tool_info: ToolCallInfo {
                        tool_name: tool_name.clone(),
                        parameters,
                        timestamp: Timestamp::now(),
                    },
                })
                .await;
            None
        }
        SessionEvent::ToolResult {
            tool_name,
            output,
            success,
        } => {
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
            let _ = event_tx
                .send(AgentRunEvent::Thinking {
                    run_id: run_id.clone(),
                    content: text.clone(),
                })
                .await;
            None
        }
        SessionEvent::Artifact {
            artifact_id,
            metadata,
        } => {
            let _ = event_tx
                .send(AgentRunEvent::Artifact {
                    run_id: run_id.clone(),
                    artifact_info: ArtifactInfo {
                        artifact_id: artifact_id.clone(),
                        artifact_type: ArtifactType::Other,
                        file_path: metadata
                            .get("file_path")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        size_bytes: metadata.get("size_bytes").and_then(Value::as_u64),
                        mime_type: metadata
                            .get("mime_type")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                    },
                })
                .await;
            None
        }
        SessionEvent::Metadata { metadata } => {
            let tokens = tokens_from_metadata(metadata);
            if tokens.is_some() {
                let _ = event_tx
                    .send(AgentRunEvent::Metadata {
                        run_id: run_id.clone(),
                        cost: None,
                        tokens,
                    })
                    .await;
            }
            None
        }
        SessionEvent::Error {
            message,
            recoverable,
        } => {
            if *recoverable {
                let _ = event_tx
                    .send(AgentRunEvent::OutputChunk {
                        run_id: run_id.clone(),
                        stream_type: OutputStreamType::Stderr,
                        text: message.clone(),
                    })
                    .await;
            } else {
                let _ = event_tx
                    .send(AgentRunEvent::Error {
                        run_id: run_id.clone(),
                        error: message.clone(),
                    })
                    .await;
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
                cache_write: usage
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
            })
        }
        Some("codex_usage") => {
            let usage = metadata.get("usage")?;
            Some(TokenUsage {
                input: usage.get("input_tokens")?.as_u64()? as u32,
                output: usage.get("output_tokens")?.as_u64()? as u32,
                reasoning: None,
                cache_read: usage
                    .get("cached_input_tokens")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
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
                output: tokens
                    .get("candidates")
                    .or_else(|| tokens.get("output"))
                    .and_then(Value::as_u64)? as u32,
                reasoning: tokens
                    .get("thoughts")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                cache_read: tokens
                    .get("cached")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
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
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("ao-agent-runner-{label}-{suffix}"))
    }

    #[cfg(unix)]
    fn write_capture_cli_shim(
        dir: &Path,
        binary_name: &str,
        fixture_path: &str,
    ) -> std::io::Result<PathBuf> {
        let script_path = dir.join(binary_name);
        let script = format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$AO_TEST_ARGS_CAPTURE\"\nenv | sort > \"$AO_TEST_ENV_CAPTURE\"\ncat \"{}\"\n",
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
        fs::read_to_string(path)
            .expect("capture file should exist")
            .lines()
            .map(ToString::to_string)
            .collect()
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

        require_native_session_backend("claude", Some(&contract))
            .expect("MCP-only AI run should stay on native path");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn spawn_session_process_bridges_claude_events() {
        let run_id = RunId("run-claude".to_string());
        let runtime_contract = json!({
            "cli": {
                "name": "claude",
                "capabilities": { "supports_mcp": true },
                "launch": {
                    "command": "sh",
                    "args": ["-c", "cat /Users/samishukri/ao-cli/crates/llm-cli-wrapper/tests/fixtures/claude_real.jsonl"],
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
            ".",
            HashMap::new(),
            Some(30),
            &run_id,
            event_tx,
            cancel_rx,
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
    async fn spawn_session_process_bridges_codex_gemini_and_oai_runner_events() {
        for (tool, fixture, expect_metadata, expect_thinking) in [
            (
                "codex",
                "/Users/samishukri/ao-cli/crates/llm-cli-wrapper/tests/fixtures/codex_real.jsonl",
                true,
                true,
            ),
            (
                "gemini",
                "/Users/samishukri/ao-cli/crates/llm-cli-wrapper/tests/fixtures/gemini_real.jsonl",
                true,
                false,
            ),
            (
                "oai-runner",
                "/Users/samishukri/ao-cli/crates/llm-cli-wrapper/tests/fixtures/oai_runner_real.jsonl",
                false,
                false,
            ),
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
                ".",
                HashMap::new(),
                Some(30),
                &run_id,
                event_tx,
                cancel_rx,
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
            assert_eq!(
                saw_thinking, expect_thinking,
                "unexpected thinking signal for {tool}"
            );
        }
    }

    #[tokio::test]
    #[cfg(unix)]
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
            ".",
            HashMap::new(),
            Some(30),
            &run_id,
            event_tx,
            cancel_rx,
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
    async fn spawn_session_process_passes_gemini_mcp_launch_env_and_args() {
        let temp_dir = unique_test_dir("gemini-mcp");
        fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let args_capture = temp_dir.join("gemini.args");
        let env_capture = temp_dir.join("gemini.env");
        let fixture =
            "/Users/samishukri/ao-cli/crates/llm-cli-wrapper/tests/fixtures/gemini_real.jsonl";
        write_capture_cli_shim(&temp_dir, "gemini", fixture).expect("gemini shim should exist");

        let run_id = RunId("run-gemini-mcp".to_string());
        let runtime_contract = json!({
            "cli": {
                "name": "gemini",
                "capabilities": { "supports_mcp": true },
                "launch": {
                    "command": "gemini",
                    "args": ["--model", "gemini-2.5-pro", "--output-format", "json", "-p", "hello"],
                    "prompt_via_stdin": false
                }
            },
            "mcp": {
                "stdio": {
                    "command": "/Users/samishukri/ao-cli/target/debug/ao",
                    "args": ["mcp", "serve", "--project-root", "/Users/samishukri/ao-cli"]
                },
                "agent_id": "ao",
                "enforce_only": true
            }
        });
        let mut env = HashMap::new();
        let original_path = std::env::var("PATH").unwrap_or_default();
        env.insert(
            "PATH".to_string(),
            format!("{}:{original_path}", temp_dir.display()),
        );
        env.insert(
            "AO_TEST_ARGS_CAPTURE".to_string(),
            args_capture.to_string_lossy().to_string(),
        );
        env.insert(
            "AO_TEST_ENV_CAPTURE".to_string(),
            env_capture.to_string_lossy().to_string(),
        );
        let (event_tx, mut event_rx) = mpsc::channel(64);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let exit_code = spawn_session_process(
            "gemini",
            "gemini-2.5-pro",
            "",
            Some(&runtime_contract),
            ".",
            env,
            Some(30),
            &run_id,
            event_tx,
            cancel_rx,
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
            args.windows(2)
                .any(|pair| pair[0] == "--allowed-mcp-server-names" && pair[1] == "ao"),
            "expected gemini launch args to include MCP allowlist, got: {args:?}"
        );
        assert!(
            env_lines
                .iter()
                .any(|line| line.starts_with("GEMINI_CLI_SYSTEM_SETTINGS_PATH=")),
            "expected gemini launch env to include settings path, got: {env_lines:?}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn spawn_session_process_passes_oai_runner_mcp_flag_after_run_subcommand() {
        let temp_dir = unique_test_dir("oai-runner-mcp");
        fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let args_capture = temp_dir.join("oai-runner.args");
        let env_capture = temp_dir.join("oai-runner.env");
        let fixture =
            "/Users/samishukri/ao-cli/crates/llm-cli-wrapper/tests/fixtures/oai_runner_real.jsonl";
        write_capture_cli_shim(&temp_dir, "ao-oai-runner", fixture)
            .expect("oai-runner shim should exist");

        let run_id = RunId("run-oai-runner-mcp".to_string());
        let runtime_contract = json!({
            "cli": {
                "name": "ao-oai-runner",
                "capabilities": { "supports_mcp": true },
                "launch": {
                    "command": "ao-oai-runner",
                    "args": ["run", "-m", "minimax/MiniMax-M2.5", "--format", "json", "hello"],
                    "prompt_via_stdin": false
                }
            },
            "mcp": {
                "stdio": {
                    "command": "/Users/samishukri/ao-cli/target/debug/ao",
                    "args": ["mcp", "serve", "--project-root", "/Users/samishukri/ao-cli"]
                },
                "agent_id": "ao",
                "enforce_only": true
            }
        });
        let mut env = HashMap::new();
        let original_path = std::env::var("PATH").unwrap_or_default();
        env.insert(
            "PATH".to_string(),
            format!("{}:{original_path}", temp_dir.display()),
        );
        env.insert(
            "AO_TEST_ARGS_CAPTURE".to_string(),
            args_capture.to_string_lossy().to_string(),
        );
        env.insert(
            "AO_TEST_ENV_CAPTURE".to_string(),
            env_capture.to_string_lossy().to_string(),
        );
        let (event_tx, mut event_rx) = mpsc::channel(64);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let exit_code = spawn_session_process(
            "ao-oai-runner",
            "minimax/MiniMax-M2.5",
            "",
            Some(&runtime_contract),
            ".",
            env,
            Some(30),
            &run_id,
            event_tx,
            cancel_rx,
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
        let mcp_idx = args
            .iter()
            .position(|arg| arg == "--mcp-config")
            .expect("oai-runner launch should include mcp config");
        assert_eq!(mcp_idx, 1, "expected mcp flag immediately after run");
    }
}
