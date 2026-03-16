use anyhow::{bail, Context, Result};
use protocol::{AgentRunEvent, OutputStreamType, RunId};
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration, MissedTickBehavior};
use tracing::{debug, info, warn};

use super::lifecycle::spawn_wait_task;
use super::mcp_policy::{apply_native_mcp_policy, is_tool_call_allowed, resolve_mcp_tool_enforcement, TempPathCleanup};
use super::process_builder::{build_cli_invocation, merge_launch_env, resolve_idle_timeout_secs};
use super::process_signals::{terminate_and_untrack, untrack_after_completion, untrack_after_error};
use super::stream_bridge::spawn_stream_forwarders;
use crate::cleanup::track_process;

pub(super) fn truncate_for_log(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{}…", truncated)
}

fn canonical_cli_name(command: &str) -> String {
    let trimmed = command.trim();
    std::path::Path::new(trimmed).file_name().and_then(|value| value.to_str()).unwrap_or(trimmed).to_ascii_lowercase()
}

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
    merge_launch_env(&mut env, &invocation);
    let hard_timeout_secs = timeout_secs.filter(|value| *value > 0);
    let idle_timeout_secs = resolve_idle_timeout_secs(tool, hard_timeout_secs, runtime_contract);
    let mcp_tool_enforcement = resolve_mcp_tool_enforcement(runtime_contract);
    let mut temp_cleanup = TempPathCleanup::default();
    apply_native_mcp_policy(&mut invocation, &mcp_tool_enforcement, &mut env, run_id, &mut temp_cleanup)?;
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

    let mut child = command.spawn().with_context(|| format!("Failed to spawn CLI process '{}'", invocation.command))?;

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
    super::process_signals::setup_windows_job_object(pid);

    let stdout = child.stdout.take().context("Failed to capture stdout")?;
    let stderr = child.stderr.take().context("Failed to capture stderr")?;

    let (output_tx, mut output_rx) = mpsc::channel::<AgentRunEvent>(100);
    let (wait_tx, mut wait_rx) = tokio::sync::oneshot::channel();

    let cli_tool = canonical_cli_name(&invocation.command);
    spawn_stream_forwarders(stdout, stderr, run_id.clone(), cli_tool, output_tx.clone());

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
                            terminate_and_untrack(&run_id_for_select, pid, "MCP-only violation");
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
                            terminate_and_untrack(&run_id_for_select, pid, "idle-timed-out");
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
                    terminate_and_untrack(&run_id_for_select, pid, "cancelled");
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
                    untrack_after_error(run_id, pid);
                    return Err(e);
                }
                Err(_) => {
                    warn!(
                        run_id = %run_id.0.as_str(),
                        pid,
                        timeout_secs,
                        "CLI process timed out; terminating process group"
                    );
                    terminate_and_untrack(run_id, pid, "timed-out");
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
                untrack_after_error(run_id, pid);
                return Err(e);
            }
        },
    };

    untrack_after_completion(run_id, pid);

    let exit_code = status.code().unwrap_or(-1);
    info!(run_id = %run_id.0.as_str(), pid, exit_code, "CLI process completed");
    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use protocol::{AgentRunEvent, RunId};
    use tokio::sync::{mpsc, oneshot};

    use super::spawn_cli_process;

    #[tokio::test]
    #[cfg(unix)]
    async fn spawn_cli_process_subprocess_fallback_streams_output() {
        let run_id = RunId("run-subprocess-fallback".to_string());
        let runtime_contract = serde_json::json!({
            "cli": {
                "launch": {
                    "command": "sh",
                    "args": ["-c", "printf 'FALLBACK_OUTPUT_42\\n'"],
                    "prompt_via_stdin": false
                }
            }
        });
        let (event_tx, mut event_rx) = mpsc::channel(64);
        let (_cancel_tx, cancel_rx) = oneshot::channel();

        let exit_code = spawn_cli_process(
            "sh",
            "",
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
        .expect("subprocess fallback should succeed");

        let mut saw_output = false;
        while let Some(event) = event_rx.recv().await {
            if let AgentRunEvent::OutputChunk { text, .. } = event {
                if text.contains("FALLBACK_OUTPUT_42") {
                    saw_output = true;
                }
            }
        }

        assert_eq!(exit_code, 0);
        assert!(saw_output, "expected output from subprocess fallback path");
    }
}
