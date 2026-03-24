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
    let control_session_id = Uuid::new_v4().to_string();
    let control_session_id_for_run = control_session_id.clone();
    let (event_tx, event_rx) = mpsc::channel(128);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let (pid_tx, pid_rx) = oneshot::channel::<Option<u32>>();
    register_session(control_session_id.clone(), cancel_tx);

    tokio::spawn(async move {
        let backend_label = "claude-native".to_string();
        let session_id_for_event = Some(control_session_id.clone());

        if let Err(error) = run_claude_session(
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
        selected_backend: "claude-native".to_string(),
        fallback_reason: None,
        pid,
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
    if let Some(mut invocation) = parse_launch_from_runtime_contract(request.extras.get("runtime_contract"))? {
        // Apply provider credential routing even for pre-built launch invocations
        if !invocation.env.contains_key("ANTHROPIC_BASE_URL") {
            if let Some((base_url, api_key)) = resolve_anthropic_compatible_provider(&request.model) {
                invocation.env.insert("ANTHROPIC_BASE_URL".to_string(), base_url);
                invocation.env.insert("ANTHROPIC_API_KEY".to_string(), api_key);
                invocation.env.insert("DISABLE_PROMPT_CACHING".to_string(), "true".to_string());
                // Strip provider prefix from --model arg
                if let Some(pos) = invocation.args.iter().position(|a| a == "--model") {
                    if let Some(model_arg) = invocation.args.get_mut(pos + 1) {
                        if let Some(bare) = model_arg.split('/').nth(1) {
                            *model_arg = bare.to_string();
                        }
                    }
                }
            }
        }
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

    let mut resolved_model = request.model.clone();

    let mut env: std::collections::BTreeMap<String, String> = Default::default();
    // Inject env vars from session request (explicit overrides)
    for (key, value) in &request.env_vars {
        match key.as_str() {
            "ANTHROPIC_BASE_URL" | "ANTHROPIC_API_KEY" | "ENABLE_TOOL_SEARCH" | "DISABLE_PROMPT_CACHING" => {
                env.insert(key.clone(), value.clone());
            }
            _ => {}
        }
    }
    // Auto-resolve credentials from model prefix for Anthropic-compatible providers
    // This allows `tool: claude` to work with any provider in credentials.json
    if !env.contains_key("ANTHROPIC_BASE_URL") {
        if let Some((base_url, api_key)) = resolve_anthropic_compatible_provider(&request.model) {
            env.insert("ANTHROPIC_BASE_URL".to_string(), base_url);
            env.insert("ANTHROPIC_API_KEY".to_string(), api_key);
            env.insert("DISABLE_PROMPT_CACHING".to_string(), "true".to_string());
            // Strip provider prefix for non-Anthropic models (e.g. "minimax/MiniMax-M2.7" → "MiniMax-M2.7")
            if let Some(bare) = resolved_model.split('/').nth(1) {
                resolved_model = bare.to_string();
            }
        }
    }
    if !resolved_model.trim().is_empty() {
        args.push("--model".to_string());
        args.push(resolved_model);
    }
    args.push(request.prompt.clone());
    let mut invocation = LaunchInvocation { command: "claude".to_string(), args, env, prompt_via_stdin: false };
    ensure_flag(&mut invocation.args, "--verbose", 1);
    ensure_flag_value(&mut invocation.args, "--output-format", "stream-json", 2);

    Ok(invocation)
}

/// Resolve Anthropic-compatible provider credentials from model prefix.
/// Reads ~/.ao/credentials.json and returns (base_url, api_key) if the model's
/// provider has both fields set.
fn resolve_anthropic_compatible_provider(model: &str) -> Option<(String, String)> {
    let normalized = model.to_ascii_lowercase();
    // Extract provider prefix from model (e.g. "kimi-code/kimi-for-coding" → "kimi-code")
    let provider = normalized.split('/').next().unwrap_or(&normalized);

    // Skip if it looks like a native Anthropic model
    if provider.starts_with("claude") || provider.is_empty() {
        return None;
    }

    let home = std::env::var("HOME").ok()?;
    let creds_path = std::path::PathBuf::from(home).join(".ao").join("credentials.json");
    let content = std::fs::read_to_string(&creds_path).ok()?;
    let creds: serde_json::Value = serde_json::from_str(&content).ok()?;
    let providers = creds.get("providers")?.as_object()?;

    // Try exact provider match first, then search by model name substring
    let entry =
        providers.get(provider).or_else(|| {
            providers.iter().find_map(|(key, val)| {
                if normalized.contains(key) || key.contains(&normalized) {
                    Some(val)
                } else {
                    None
                }
            })
        })?;
    let base_url = entry.get("base_url")?.as_str()?.to_string();
    let api_key = entry.get("api_key")?.as_str()?.to_string();

    if base_url.is_empty() || api_key.is_empty() {
        return None;
    }

    Some((base_url, api_key))
}

async fn run_claude_session(
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
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env_remove("CLAUDE_CODE_SESSION_ACCESS_TOKEN")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .envs(invocation.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
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

    let exit_code = wait_for_claude_child(&mut child, request.timeout_secs, &mut cancel_rx).await?;

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
                    crate::session::kill_and_reap_child(child).await;
                    Err(Error::ExecutionFailed(format!(
                        "claude session timed out after {} seconds",
                        secs
                    )))
                }
                _ = cancel_rx => {
                    crate::session::kill_and_reap_child(child).await;
                    Err(Error::ExecutionFailed("claude session cancelled".to_string()))
                }
            }
        }
        None => {
            tokio::select! {
                status = child.wait() => Ok(status?.code()),
                _ = cancel_rx => {
                    crate::session::kill_and_reap_child(child).await;
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
    let raw = request
        .extras
        .pointer("/runtime_contract/cli/session/session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;

    // Claude CLI requires a valid UUID (36 chars with dashes).
    // If the session ID is already a valid UUID, use it directly.
    if Uuid::parse_str(raw).is_ok() {
        return Some(raw.to_string());
    }

    // Otherwise, generate a fresh UUID for this session.
    // Session resume won't work across restarts, but the session will start cleanly.
    Some(Uuid::new_v4().to_string())
}
