use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::cli::{
    ensure_machine_json_output, parse_cli_type, parse_launch_from_runtime_contract, CliType, LaunchInvocation,
};
use crate::error::{Error, Result};
use crate::parser::{extract_text_from_line, NormalizedTextEvent};

use super::{
    session_backend::SessionBackend, session_backend_info::SessionBackendInfo,
    session_backend_kind::SessionBackendKind, session_capabilities::SessionCapabilities, session_event::SessionEvent,
    session_request::SessionRequest, session_run::SessionRun, session_stability::SessionStability,
};

pub struct SubprocessSessionBackend;

impl Default for SubprocessSessionBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SubprocessSessionBackend {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionBackend for SubprocessSessionBackend {
    fn info(&self) -> SessionBackendInfo {
        SessionBackendInfo {
            kind: SessionBackendKind::Subprocess,
            provider_tool: "subprocess".to_string(),
            stability: SessionStability::Stable,
            display_name: "Subprocess Backend".to_string(),
        }
    }

    fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            supports_resume: false,
            supports_terminate: false,
            supports_permissions: false,
            supports_mcp: true,
            supports_tool_events: false,
            supports_thinking_events: false,
            supports_artifact_events: false,
            supports_usage_metadata: false,
        }
    }

    async fn start_session(&self, request: SessionRequest) -> Result<SessionRun> {
        let session_id = Uuid::new_v4().to_string();
        let session_id_for_run = session_id.clone();
        let invocation = launch_invocation_for_request(&request)?;
        let backend_label =
            request.extras.get("backend_label").and_then(serde_json::Value::as_str).unwrap_or("subprocess").to_string();
        let started_backend_label = backend_label.clone();
        let fallback_reason =
            request.extras.get("fallback_reason").and_then(serde_json::Value::as_str).map(ToOwned::to_owned);

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

        let tool = request.tool.clone();
        let (event_tx, event_rx) = mpsc::channel(128);

        tokio::spawn(async move {
            let _ = event_tx
                .send(SessionEvent::Started { backend: started_backend_label.clone(), session_id: Some(session_id) })
                .await;

            if let Err(error) = manage_subprocess_child(child, tool, event_tx.clone()).await {
                let _ = event_tx.send(SessionEvent::Error { message: error.to_string(), recoverable: false }).await;
                let _ = event_tx.send(SessionEvent::Finished { exit_code: Some(1) }).await;
            }
        });

        Ok(SessionRun {
            session_id: Some(session_id_for_run),
            events: event_rx,
            selected_backend: backend_label,
            fallback_reason,
            child_pid,
        })
    }

    async fn resume_session(&self, _request: SessionRequest, session_id: &str) -> Result<SessionRun> {
        Err(Error::ExecutionFailed(format!("subprocess backend does not support resume for session '{}'", session_id)))
    }

    async fn terminate_session(&self, session_id: &str) -> Result<()> {
        Err(Error::ExecutionFailed(format!(
            "subprocess backend does not track active child processes for session '{}'",
            session_id
        )))
    }
}

async fn manage_subprocess_child(
    mut child: Child,
    tool: String,
    event_tx: mpsc::Sender<SessionEvent>,
) -> Result<()> {
    let stdout =
        child.stdout.take().ok_or_else(|| Error::ExecutionFailed("failed to capture child stdout".to_string()))?;
    let stderr =
        child.stderr.take().ok_or_else(|| Error::ExecutionFailed("failed to capture child stderr".to_string()))?;

    let stdout_tool = tool;
    let stdout_tx = event_tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match extract_text_from_line(&line, &stdout_tool) {
                NormalizedTextEvent::TextChunk { text } => {
                    let _ = stdout_tx.send(SessionEvent::TextDelta { text }).await;
                }
                NormalizedTextEvent::FinalResult { text } => {
                    let _ = stdout_tx.send(SessionEvent::FinalText { text }).await;
                }
                NormalizedTextEvent::Ignored => {
                    let _ = stdout_tx.send(SessionEvent::TextDelta { text: line }).await;
                }
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

    let status = child.wait().await?;

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    let _ = event_tx.send(SessionEvent::Finished { exit_code: status.code() }).await;

    Ok(())
}

fn launch_invocation_for_request(request: &SessionRequest) -> Result<LaunchInvocation> {
    if let Some(invocation) = parse_launch_from_runtime_contract(request.extras.get("runtime_contract"))? {
        return Ok(invocation);
    }

    let mut invocation = match parse_cli_type(&request.tool) {
        Some(CliType::Claude) => {
            let mut args = vec!["--print".to_string()];
            if !request.model.trim().is_empty() {
                args.push("--model".to_string());
                args.push(request.model.clone());
            }
            args.push(request.prompt.clone());
            LaunchInvocation { command: "claude".to_string(), args, env: Default::default(), prompt_via_stdin: false }
        }
        Some(CliType::Codex) => {
            let mut args = vec!["exec".to_string(), "--skip-git-repo-check".to_string()];
            if !request.model.trim().is_empty() {
                args.push("--model".to_string());
                args.push(request.model.clone());
            }
            args.push(request.prompt.clone());
            LaunchInvocation { command: "codex".to_string(), args, env: Default::default(), prompt_via_stdin: false }
        }
        Some(CliType::Gemini) => {
            let mut args = Vec::new();
            if !request.model.trim().is_empty() {
                args.push("--model".to_string());
                args.push(request.model.clone());
            }
            args.push("-p".to_string());
            args.push(request.prompt.clone());
            LaunchInvocation { command: "gemini".to_string(), args, env: Default::default(), prompt_via_stdin: false }
        }
        Some(CliType::OpenCode) => LaunchInvocation {
            command: "opencode".to_string(),
            args: vec!["run".to_string(), request.prompt.clone()],
            env: Default::default(),
            prompt_via_stdin: false,
        },
        Some(CliType::OaiRunner) => {
            let mut args = vec!["run".to_string()];
            if !request.model.trim().is_empty() {
                args.push("-m".to_string());
                args.push(request.model.clone());
            }
            args.push(request.prompt.clone());
            LaunchInvocation {
                command: "ao-oai-runner".to_string(),
                args,
                env: Default::default(),
                prompt_via_stdin: false,
            }
        }
        _ => LaunchInvocation {
            command: request.tool.clone(),
            args: vec![request.prompt.clone()],
            env: Default::default(),
            prompt_via_stdin: false,
        },
    };

    ensure_machine_json_output(&mut invocation);
    Ok(invocation)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::SubprocessSessionBackend;
    use crate::session::{SessionBackend, SessionEvent, SessionRequest};

    #[tokio::test]
    #[cfg(unix)]
    async fn subprocess_backend_streams_started_text_and_finished_events() {
        let backend = SubprocessSessionBackend::new();
        let request = SessionRequest {
            tool: "sh".to_string(),
            model: String::new(),
            prompt: String::new(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({
                "runtime_contract": {
                    "cli": {
                        "launch": {
                            "command": "sh",
                            "args": ["-c", "printf 'hello\\n'"],
                            "prompt_via_stdin": false
                        }
                    }
                }
            }),
        };

        let mut run = backend.start_session(request).await.expect("session should start");

        let started = run.events.recv().await.expect("started event");
        assert!(matches!(started, SessionEvent::Started { .. }));

        let text = run.events.recv().await.expect("text event");
        assert_eq!(text, SessionEvent::TextDelta { text: "hello".to_string() });

        let finished = run.events.recv().await.expect("finished event");
        assert_eq!(finished, SessionEvent::Finished { exit_code: Some(0) });
    }
}
