use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::cli::{ensure_flag, ensure_flag_value, parse_launch_from_runtime_contract, LaunchInvocation};
use crate::error::{Error, Result};

use super::parser::parse_claude_stdout_line;
use crate::session::{session_event::SessionEvent, session_request::SessionRequest, session_run::SessionRun};

pub(crate) async fn start_claude_session(
    request: SessionRequest,
    resume_session_id: Option<String>,
) -> Result<SessionRun> {
    let invocation = claude_invocation_for_request(&request, resume_session_id.as_deref())?;

    let mut command = Command::new(&invocation.command);
    command
        .args(&invocation.args)
        .current_dir(&request.cwd)
        .env_clear()
        .envs(request.env_vars.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn()?;
    let child_pid = child.id();

    if let Some(mut stdin) = child.stdin.take() {
        if invocation.prompt_via_stdin && !request.prompt.is_empty() {
            stdin.write_all(request.prompt.as_bytes()).await?;
        }
        drop(stdin);
    }

    let control_session_id = Uuid::new_v4().to_string();
    let control_session_id_for_run = control_session_id.clone();
    let started_session_id = Some(control_session_id.clone());
    let (event_tx, event_rx) = mpsc::channel(128);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    register_session(control_session_id.clone(), cancel_tx);

    let timeout_secs = request.timeout_secs;
    tokio::spawn(async move {
        let _ = event_tx
            .send(SessionEvent::Started { backend: "claude-native".to_string(), session_id: started_session_id })
            .await;

        if let Err(error) = manage_claude_child(child, timeout_secs, event_tx.clone(), cancel_rx).await {
            let _ = event_tx.send(SessionEvent::Error { message: error.to_string(), recoverable: false }).await;
            let _ = event_tx.send(SessionEvent::Finished { exit_code: Some(1) }).await;
        }
        unregister_session(&control_session_id);
    });

    Ok(SessionRun {
        session_id: Some(control_session_id_for_run),
        events: event_rx,
        selected_backend: "claude-native".to_string(),
        fallback_reason: None,
        child_pid,
    })
}

pub(crate) async fn terminate_claude_session(session_id: &str) -> Result<()> {
    let Some(cancel_tx) = take_session(session_id) else {
        return Err(Error::ExecutionFailed(format!(
            "claude backend does not track active child process for session '{}'",
            session_id
        )));
    };
    let _ = cancel_tx.send(());
    Ok(())
}

pub(crate) fn claude_invocation_for_request(
    request: &SessionRequest,
    resume_session_id: Option<&str>,
) -> Result<LaunchInvocation> {
    if let Some(invocation) = parse_launch_from_runtime_contract(request.extras.get("runtime_contract"))? {
        return Ok(invocation);
    }

    let mut args =
        vec!["--print".to_string(), "--verbose".to_string(), "--output-format".to_string(), "stream-json".to_string()];

    if let Some(permission_mode) = request.permission_mode.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        args.push("--permission-mode".to_string());
        args.push(permission_mode.to_string());
    } else {
        args.push("--dangerously-skip-permissions".to_string());
    }

    if let Some(session_id) = resume_session_id.map(str::trim).filter(|value| !value.is_empty()) {
        args.push("--resume".to_string());
        args.push(session_id.to_string());
    } else if let Some(session_id) = configured_claude_session_id(request) {
        args.push("--session-id".to_string());
        args.push(session_id);
    }

    if !request.model.trim().is_empty() {
        args.push("--model".to_string());
        args.push(request.model.clone());
    }

    args.push(request.prompt.clone());

    let mut invocation =
        LaunchInvocation { command: "claude".to_string(), args, env: Default::default(), prompt_via_stdin: false };
    ensure_flag(&mut invocation.args, "--verbose", 1);
    ensure_flag_value(&mut invocation.args, "--output-format", "stream-json", 2);

    Ok(invocation)
}

async fn manage_claude_child(
    mut child: Child,
    timeout_secs: Option<u64>,
    event_tx: mpsc::Sender<SessionEvent>,
    mut cancel_rx: oneshot::Receiver<()>,
) -> Result<()> {
    let stdout =
        child.stdout.take().ok_or_else(|| Error::ExecutionFailed("failed to capture claude stdout".to_string()))?;
    let stderr =
        child.stderr.take().ok_or_else(|| Error::ExecutionFailed("failed to capture claude stderr".to_string()))?;

    let stdout_tx = event_tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut last_final_text: Option<String> = None;
        let mut lines = BufReader::new(stdout).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            for event in parse_claude_stdout_line(&line) {
                if let SessionEvent::FinalText { text } = &event {
                    if last_final_text.as_deref() == Some(text.as_str()) {
                        continue;
                    }
                    last_final_text = Some(text.clone());
                }
                let _ = stdout_tx.send(event).await;
            }
        }
    });

    let stderr_tx = event_tx.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = stderr_tx.send(SessionEvent::Error { message: line, recoverable: true }).await;
        }
    });

    let exit_code = wait_for_claude_child(&mut child, timeout_secs, &mut cancel_rx).await?;

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    let _ = event_tx.send(SessionEvent::Finished { exit_code }).await;

    Ok(())
}

async fn wait_for_claude_child(
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
                    child.kill().await?;
                    Err(Error::ExecutionFailed(format!(
                        "claude session timed out after {} seconds",
                        secs
                    )))
                }
                _ = cancel_rx => {
                    child.kill().await?;
                    Err(Error::ExecutionFailed("claude session cancelled".to_string()))
                }
            }
        }
        None => {
            tokio::select! {
                status = child.wait() => Ok(status?.code()),
                _ = cancel_rx => {
                    child.kill().await?;
                    Err(Error::ExecutionFailed("claude session cancelled".to_string()))
                }
            }
        }
    }
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

fn configured_claude_session_id(request: &SessionRequest) -> Option<String> {
    request
        .extras
        .pointer("/runtime_contract/cli/session/session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}
