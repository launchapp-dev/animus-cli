use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::cli::{
    ensure_flag, ensure_flag_value, parse_launch_from_runtime_contract, LaunchInvocation,
};
use crate::error::{Error, Result};

use super::{
    claude_session_parser::parse_claude_stdout_line, session_event::SessionEvent,
    session_request::SessionRequest, session_run::SessionRun,
};

pub(crate) async fn start_claude_session(
    request: SessionRequest,
    resume_session_id: Option<String>,
) -> Result<SessionRun> {
    let invocation = claude_invocation_for_request(&request, resume_session_id.as_deref())?;
    let selected_session_id = configured_claude_session_id(&request).or(resume_session_id.clone());
    let started_session_id = selected_session_id.clone();
    let (event_tx, event_rx) = mpsc::channel(128);

    tokio::spawn(async move {
        let _ = event_tx
            .send(SessionEvent::Started {
                backend: "claude-native".to_string(),
                session_id: started_session_id,
            })
            .await;

        if let Err(error) = run_claude_session(request, invocation, event_tx.clone()).await {
            let _ = event_tx
                .send(SessionEvent::Error {
                    message: error.to_string(),
                    recoverable: false,
                })
                .await;
            let _ = event_tx
                .send(SessionEvent::Finished { exit_code: Some(1) })
                .await;
        }
    });

    Ok(SessionRun {
        session_id: selected_session_id,
        events: event_rx,
        selected_backend: "claude-native".to_string(),
        fallback_reason: None,
    })
}

pub(crate) fn claude_invocation_for_request(
    request: &SessionRequest,
    resume_session_id: Option<&str>,
) -> Result<LaunchInvocation> {
    if let Some(invocation) =
        parse_launch_from_runtime_contract(request.extras.get("runtime_contract"))?
    {
        return Ok(invocation);
    }

    let mut args = vec![
        "--print".to_string(),
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];

    if let Some(permission_mode) = request
        .permission_mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        args.push("--permission-mode".to_string());
        args.push(permission_mode.to_string());
    } else {
        args.push("--dangerously-skip-permissions".to_string());
    }

    if let Some(session_id) = resume_session_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
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

    let mut invocation = LaunchInvocation {
        command: "claude".to_string(),
        args,
        prompt_via_stdin: false,
    };
    ensure_flag(&mut invocation.args, "--verbose", 1);
    ensure_flag_value(&mut invocation.args, "--output-format", "stream-json", 2);

    Ok(invocation)
}

async fn run_claude_session(
    request: SessionRequest,
    invocation: LaunchInvocation,
    event_tx: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    let mut child = Command::new(&invocation.command)
        .args(&invocation.args)
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        if invocation.prompt_via_stdin && !request.prompt.is_empty() {
            stdin.write_all(request.prompt.as_bytes()).await?;
        }
        drop(stdin);
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::ExecutionFailed("failed to capture claude stdout".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::ExecutionFailed("failed to capture claude stderr".to_string()))?;

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
            let _ = stderr_tx
                .send(SessionEvent::Error {
                    message: line,
                    recoverable: true,
                })
                .await;
        }
    });

    let exit_code = wait_for_claude_child(&mut child, request.timeout_secs).await?;

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    let _ = event_tx.send(SessionEvent::Finished { exit_code }).await;

    Ok(())
}

async fn wait_for_claude_child(
    child: &mut Child,
    timeout_secs: Option<u64>,
) -> Result<Option<i32>> {
    match timeout_secs {
        Some(secs) => match timeout(Duration::from_secs(secs), child.wait()).await {
            Ok(status) => Ok(status?.code()),
            Err(_) => {
                child.kill().await?;
                Err(Error::ExecutionFailed(format!(
                    "claude session timed out after {} seconds",
                    secs
                )))
            }
        },
        None => Ok(child.wait().await?.code()),
    }
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
