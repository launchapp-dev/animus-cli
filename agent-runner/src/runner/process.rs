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
    let invocation = build_cli_invocation(tool, model, prompt, runtime_contract).await?;
    let hard_timeout_secs = timeout_secs.filter(|value| *value > 0);
    let idle_timeout_secs = resolve_idle_timeout_secs(tool, hard_timeout_secs, runtime_contract);
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

    let run_loop = async move {
        loop {
            tokio::select! {
                Some(evt) = output_rx.recv() => {
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
