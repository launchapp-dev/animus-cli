use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::cli::{ensure_flag_value, parse_launch_from_runtime_contract, LaunchInvocation};
use crate::error::{Error, Result};
use crate::session::{session_event::SessionEvent, session_request::SessionRequest, session_run::SessionRun};

use super::parser::parse_gemini_json_chunk;

pub(crate) async fn start_gemini_session(
    request: SessionRequest,
    resume_session_id: Option<String>,
) -> Result<SessionRun> {
    let invocation = gemini_invocation_for_request(&request, resume_session_id.as_deref())?;
    let control_session_id = Uuid::new_v4().to_string();
    let control_session_id_for_run = control_session_id.clone();
    let (event_tx, event_rx) = mpsc::channel(128);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let (pid_tx, pid_rx) = oneshot::channel::<Option<u32>>();
    register_session(control_session_id.clone(), cancel_tx);

    tokio::spawn(async move {
        let backend_label = "gemini-native".to_string();
        let session_id_for_event = Some(control_session_id.clone());

        if let Err(error) = run_gemini_session(
            request,
            invocation,
            event_tx.clone(),
            cancel_rx,
            pid_tx,
            backend_label,
            session_id_for_event,
        )
        .await
        {
            let _ = event_tx.send(SessionEvent::Error { message: error.to_string(), recoverable: false }).await;
            let _ = event_tx.send(SessionEvent::Finished { exit_code: Some(1) }).await;
        }
        unregister_session(&control_session_id);
    });

    let pid = pid_rx.await.ok().flatten();
    Ok(SessionRun {
        session_id: Some(control_session_id_for_run),
        events: event_rx,
        selected_backend: "gemini-native".to_string(),
        fallback_reason: None,
        pid,
    })
}

pub(crate) async fn terminate_gemini_session(session_id: &str) -> Result<()> {
    let Some(cancel_tx) = take_session(session_id) else {
        return Err(Error::ExecutionFailed(format!(
            "gemini backend does not track active child process for session '{}'",
            session_id
        )));
    };
    let _ = cancel_tx.send(());
    Ok(())
}

pub(crate) fn gemini_invocation_for_request(
    request: &SessionRequest,
    resume_session_id: Option<&str>,
) -> Result<LaunchInvocation> {
    if let Some(invocation) = parse_launch_from_runtime_contract(request.extras.get("runtime_contract"))? {
        return Ok(invocation);
    }

    let mut args = Vec::new();

    if let Some(session_id) = resume_session_id.map(str::trim).filter(|value| !value.is_empty()) {
        args.push("--resume".to_string());
        args.push(session_id.to_string());
    } else if let Some(session_id) = configured_gemini_session_id(request) {
        args.push("--resume".to_string());
        args.push(session_id);
    }

    if !request.model.trim().is_empty() {
        args.push("--model".to_string());
        args.push(request.model.clone());
    }

    args.push("--output-format".to_string());
    args.push("json".to_string());
    args.push("-p".to_string());
    args.push(request.prompt.clone());

    let mut invocation =
        LaunchInvocation { command: "gemini".to_string(), args, env: Default::default(), prompt_via_stdin: false };
    let insert_at = invocation.args.len();
    ensure_flag_value(&mut invocation.args, "--output-format", "json", insert_at);

    Ok(invocation)
}

async fn run_gemini_session(
    request: SessionRequest,
    invocation: LaunchInvocation,
    event_tx: mpsc::Sender<SessionEvent>,
    mut cancel_rx: oneshot::Receiver<()>,
    pid_tx: oneshot::Sender<Option<u32>>,
    backend: String,
    session_id: Option<String>,
) -> Result<()> {
    let mut command = Command::new(&invocation.command);
    command
        .args(&invocation.args)
        .current_dir(&request.cwd)
        .env_remove("CLAUDECODE").env_remove("CLAUDE_CODE_ENTRYPOINT").env_remove("CLAUDE_CODE_SESSION_ACCESS_TOKEN").env_remove("CLAUDE_CODE_SESSION_ID")
        .envs(request.env_vars.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn()?;
    let _ = pid_tx.send(child.id());

    let pid = child.id();
    let _ = event_tx.send(SessionEvent::Started { backend, session_id, pid }).await;

    if let Some(mut stdin) = child.stdin.take() {
        if invocation.prompt_via_stdin && !request.prompt.is_empty() {
            stdin.write_all(request.prompt.as_bytes()).await?;
        }
        drop(stdin);
    }

    let stdout =
        child.stdout.take().ok_or_else(|| Error::ExecutionFailed("failed to capture gemini stdout".to_string()))?;
    let stderr =
        child.stderr.take().ok_or_else(|| Error::ExecutionFailed("failed to capture gemini stderr".to_string()))?;

    let stdout_tx = event_tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut last_final_text: Option<String> = None;
        let mut lines = BufReader::new(stdout).lines();
        let mut json_accum = String::new();
        let mut depth = 0i32;
        let mut accumulating = false;

        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let is_json_line = serde_json::from_str::<serde_json::Value>(trimmed).is_ok();
            if !is_json_line && (trimmed == "{" || accumulating) {
                if trimmed == "{" && !accumulating {
                    accumulating = true;
                    json_accum.clear();
                    depth = 0;
                }
                json_accum.push_str(trimmed);
                json_accum.push('\n');
                for ch in trimmed.chars() {
                    match ch {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                if depth <= 0 {
                    accumulating = false;
                    emit_gemini_events(&json_accum, &stdout_tx, &mut last_final_text).await;
                    json_accum.clear();
                    depth = 0;
                }
                continue;
            }

            emit_gemini_events(trimmed, &stdout_tx, &mut last_final_text).await;
        }
    });

    let stderr_tx = event_tx.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = stderr_tx.send(SessionEvent::Error { message: line, recoverable: true }).await;
        }
    });

    let exit_code = wait_for_gemini_child(&mut child, request.timeout_secs, &mut cancel_rx).await?;

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    let _ = event_tx.send(SessionEvent::Finished { exit_code }).await;

    Ok(())
}

async fn emit_gemini_events(chunk: &str, tx: &mpsc::Sender<SessionEvent>, last_final_text: &mut Option<String>) {
    for event in parse_gemini_json_chunk(chunk) {
        if let SessionEvent::FinalText { text } = &event {
            if last_final_text.as_deref() == Some(text.as_str()) {
                continue;
            }
            *last_final_text = Some(text.clone());
        }
        let _ = tx.send(event).await;
    }
}

async fn wait_for_gemini_child(
    child: &mut Child,
    timeout_secs: Option<u64>,
    cancel_rx: &mut oneshot::Receiver<()>,
) -> Result<Option<i32>> {
    match timeout_secs {
        Some(secs) => {
            let timeout_sleep = tokio::time::sleep(Duration::from_secs(secs));
            tokio::pin!(timeout_sleep);
            tokio::select! {
                status = child.wait() => Ok(status?.code()),
                _ = &mut timeout_sleep => {
                    crate::session::kill_and_reap_child(child).await;
                    Err(Error::ExecutionFailed(format!(
                        "gemini session timed out after {} seconds",
                        secs
                    )))
                }
                _ = cancel_rx => {
                    crate::session::kill_and_reap_child(child).await;
                    Err(Error::ExecutionFailed("gemini session cancelled".to_string()))
                }
            }
        }
        None => {
            tokio::select! {
                status = child.wait() => Ok(status?.code()),
                _ = cancel_rx => {
                    crate::session::kill_and_reap_child(child).await;
                    Err(Error::ExecutionFailed("gemini session cancelled".to_string()))
                }
            }
        }
    }
}

fn configured_gemini_session_id(request: &SessionRequest) -> Option<String> {
    request
        .extras
        .pointer("/runtime_contract/cli/session/session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn session_registry() -> &'static Mutex<HashMap<String, oneshot::Sender<()>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, oneshot::Sender<()>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_session(session_id: String, cancel_tx: oneshot::Sender<()>) {
    if let Ok(mut registry) = session_registry().lock() {
        registry.insert(session_id, cancel_tx);
    }
}

fn unregister_session(session_id: &str) {
    if let Ok(mut registry) = session_registry().lock() {
        registry.remove(session_id);
    }
}

fn take_session(session_id: &str) -> Option<oneshot::Sender<()>> {
    session_registry().lock().ok().and_then(|mut registry| registry.remove(session_id))
}
