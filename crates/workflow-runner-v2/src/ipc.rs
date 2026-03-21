#[cfg(unix)]
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use orchestrator_core::runtime_contract;
use protocol::{AgentRunEvent, IpcAuthRequest, IpcAuthResult, OutputStreamType, RunId, MAX_UNIX_SOCKET_PATH_LEN};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::{Duration, sleep};

fn scoped_ao_root(project_root: &Path) -> Option<PathBuf> {
    protocol::scoped_state_root(project_root)
}

pub fn runner_config_dir(project_root: &Path) -> PathBuf {
    let config_dir = scoped_ao_root(project_root).unwrap_or_else(|| project_root.join(".ao")).join("runner");

    normalize_runner_config_dir(config_dir)
}

fn normalize_runner_config_dir(config_dir: PathBuf) -> PathBuf {
    #[cfg(unix)]
    {
        shorten_runner_config_dir_if_needed(config_dir)
    }

    #[cfg(not(unix))]
    {
        config_dir
    }
}

#[cfg(unix)]
fn shorten_runner_config_dir_if_needed(config_dir: PathBuf) -> PathBuf {
    let socket_path = config_dir.join("agent-runner.sock");
    let socket_len = socket_path.as_os_str().to_string_lossy().len();
    if socket_len <= MAX_UNIX_SOCKET_PATH_LEN {
        return config_dir;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config_dir.to_string_lossy().hash(&mut hasher);
    let digest = hasher.finish();
    let shortened = std::env::temp_dir().join("ao-runner").join(format!("{digest:016x}"));
    let _ = std::fs::create_dir_all(&shortened);
    let _ = std::fs::write(shortened.join("origin-path.txt"), config_dir.to_string_lossy().as_bytes());
    shortened
}

/// Maximum number of socket connection retry attempts for transient failures.
const CONNECT_RUNNER_RETRY_ATTEMPTS: usize = 3;

/// Initial backoff delay between socket connection retries (milliseconds).
const CONNECT_RUNNER_INITIAL_BACKOFF_MS: u64 = 200;

/// Maximum backoff delay between socket connection retries (seconds).
const CONNECT_RUNNER_MAX_BACKOFF_SECS: u64 = 3;

#[cfg(unix)]
pub async fn connect_runner(config_dir: &Path) -> Result<tokio::net::UnixStream> {
    let socket_path = config_dir.join("agent-runner.sock");
    let connect_timeout_secs: u64 = 5;
    let mut backoff = Duration::from_millis(CONNECT_RUNNER_INITIAL_BACKOFF_MS);

    for attempt in 1..=CONNECT_RUNNER_RETRY_ATTEMPTS {
        let connect_future = tokio::net::UnixStream::connect(&socket_path);
        match tokio::time::timeout(Duration::from_secs(connect_timeout_secs), connect_future).await {
            Ok(Ok(mut stream)) => {
                match authenticate_runner_stream(&mut stream, config_dir).await {
                    Ok(()) => return Ok(stream),
                    Err(auth_error) => {
                        if attempt < CONNECT_RUNNER_RETRY_ATTEMPTS {
                            eprintln!(
                                "[ao] Runner auth failed (attempt {}/{}): {}, retrying in {:?}...",
                                attempt, CONNECT_RUNNER_RETRY_ATTEMPTS, auth_error, backoff
                            );
                            sleep(backoff).await;
                            backoff = std::cmp::min(backoff.saturating_mul(2), Duration::from_secs(CONNECT_RUNNER_MAX_BACKOFF_SECS));
                            continue;
                        }
                        return Err(anyhow!(
                            "failed to authenticate runner connection at {}: {auth_error}",
                            socket_path.display()
                        ));
                    }
                }
            }
            Ok(Err(error)) => {
                if attempt < CONNECT_RUNNER_RETRY_ATTEMPTS {
                    let base_message = format!(
                        "failed to connect to runner socket at {} (timeout={}s)",
                        socket_path.display(),
                        connect_timeout_secs
                    );
                    eprintln!(
                        "[ao] {} (attempt {}/{}): {}, retrying in {:?}...",
                        base_message, attempt, CONNECT_RUNNER_RETRY_ATTEMPTS, error, backoff
                    );
                    sleep(backoff).await;
                    backoff = std::cmp::min(backoff.saturating_mul(2), Duration::from_secs(CONNECT_RUNNER_MAX_BACKOFF_SECS));
                    continue;
                }
                let hint = if socket_path.exists() {
                    format!(
                        "failed to connect to runner socket at {} (timeout={}s). socket file exists and may be stale",
                        socket_path.display(),
                        connect_timeout_secs
                    )
                } else {
                    format!(
                        "failed to connect to runner socket at {} (timeout={}s)",
                        socket_path.display(),
                        connect_timeout_secs
                    )
                };
                return Err(anyhow!("{hint}: {error}"));
            }
            Err(_) => {
                if attempt < CONNECT_RUNNER_RETRY_ATTEMPTS {
                    eprintln!(
                        "[ao] Timed out connecting to runner socket at {} after {}s (attempt {}/{}), retrying in {:?}...",
                        socket_path.display(), connect_timeout_secs, attempt, CONNECT_RUNNER_RETRY_ATTEMPTS, backoff
                    );
                    sleep(backoff).await;
                    backoff = std::cmp::min(backoff.saturating_mul(2), Duration::from_secs(CONNECT_RUNNER_MAX_BACKOFF_SECS));
                    continue;
                }
                return Err(anyhow!(
                    "timed out connecting to runner socket at {} after {}s ({} attempts); if no runner is active, remove stale socket and restart runner",
                    socket_path.display(),
                    connect_timeout_secs,
                    CONNECT_RUNNER_RETRY_ATTEMPTS
                ));
            }
        }
    }

    Err(anyhow!(
        "exhausted {} connection attempts to runner socket at {}",
        CONNECT_RUNNER_RETRY_ATTEMPTS,
        socket_path.display()
    ))
}

#[cfg(not(unix))]
pub async fn connect_runner(config_dir: &Path) -> Result<tokio::net::TcpStream> {
    let mut stream = tokio::net::TcpStream::connect("127.0.0.1:9001")
        .await
        .map_err(|error| anyhow!("failed to connect to runner at 127.0.0.1:9001: {error}"))?;
    authenticate_runner_stream(&mut stream, config_dir)
        .await
        .map_err(|error| anyhow!("failed to authenticate runner connection at 127.0.0.1:9001: {error}"))?;
    Ok(stream)
}

pub async fn authenticate_runner_stream<S>(stream: &mut S, config_dir: &Path) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let token = protocol::Config::load_from_dir(config_dir)
        .map_err(|error| {
            anyhow!("failed to load runner config for authentication from {}: {error}", config_dir.display())
        })?
        .get_token()
        .map_err(|error| {
            format!("agent runner token unavailable; set AGENT_RUNNER_TOKEN or configure agent_runner_token: {error}")
        })
        .map_err(|msg| anyhow!(msg))?;

    write_json_line(stream, &IpcAuthRequest::new(token))
        .await
        .map_err(|error| anyhow!("failed to send runner auth payload: {error}"))?;

    let mut line = String::new();
    let read_len = tokio::time::timeout(Duration::from_secs(2), async {
        let mut reader = BufReader::new(stream);
        reader.read_line(&mut line).await
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for runner auth response"))?
    .map_err(|error| anyhow!("failed to read runner auth response: {error}"))?;

    if read_len == 0 {
        return Err(anyhow!("runner closed connection before auth completed",));
    }

    let response: IpcAuthResult = serde_json::from_str(line.trim())
        .map_err(|error| anyhow!("received malformed runner auth response: {error}"))?;
    if response.ok {
        return Ok(());
    }

    let failure_code = response.code.map(|code| code.as_str()).unwrap_or("unknown");
    let message = response.message.unwrap_or_else(|| "unauthorized".to_string());
    Err(anyhow!("runner authentication failed ({failure_code}): {message}"))
}

pub async fn write_json_line<W: AsyncWrite + Unpin, T: serde::Serialize>(writer: &mut W, payload: &T) -> Result<()> {
    let json = serde_json::to_string(payload)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

pub fn build_runtime_contract(tool: &str, model: &str, prompt: &str) -> Option<Value> {
    build_runtime_contract_with_resume(tool, model, prompt, None)
}

pub fn build_runtime_contract_with_resume(
    tool: &str,
    model: &str,
    prompt: &str,
    resume_plan: Option<&orchestrator_core::runtime_contract::CliSessionResumePlan>,
) -> Option<Value> {
    let mcp_config = protocol::McpRuntimeConfig::default();
    let mcp_endpoint = mcp_config.endpoint.clone();
    let mcp_agent_id = mcp_config.agent_id.clone();

    let runtime_contract = runtime_contract::build_runtime_contract(
        tool,
        model,
        prompt,
        resume_plan,
        None,
        mcp_endpoint.as_deref(),
        mcp_agent_id.as_deref(),
    )?;
    Some(runtime_contract)
}

pub fn event_matches_run(event: &AgentRunEvent, run_id: &RunId) -> bool {
    match event {
        AgentRunEvent::Started { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::OutputChunk { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::Metadata { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::Error { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::Finished { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::ToolCall { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::ToolResult { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::Artifact { run_id: event_run_id, .. } => event_run_id == run_id,
        AgentRunEvent::Thinking { run_id: event_run_id, .. } => event_run_id == run_id,
    }
}

pub fn ensure_safe_run_id(run_id: &str) -> Result<()> {
    if run_id.trim().is_empty() {
        return Err(anyhow!("run_id is required"));
    }
    if run_id.contains('/') || run_id.contains('\\') || run_id.contains("..") {
        return Err(anyhow!("invalid run_id: {run_id}"));
    }
    Ok(())
}

pub fn run_dir(project_root: &str, run_id: &RunId, base_override: Option<&str>) -> PathBuf {
    let base = base_override.map(PathBuf::from).unwrap_or_else(|| {
        scoped_ao_root(Path::new(project_root)).unwrap_or_else(|| Path::new(project_root).join(".ao")).join("runs")
    });
    base.join(&run_id.0)
}

pub fn persist_run_event(run_dir: &Path, event: &AgentRunEvent) -> Result<()> {
    let event_path = run_dir.join("events.jsonl");
    let line = serde_json::to_string(event)?;
    append_line(&event_path, &line)?;

    if let AgentRunEvent::OutputChunk { stream_type, text, .. } = event {
        persist_json_output(run_dir, *stream_type, text)?;
    }

    Ok(())
}

fn persist_json_output(run_dir: &Path, stream_type: OutputStreamType, text: &str) -> Result<()> {
    let path = run_dir.join("json-output.jsonl");
    for (raw, payload) in collect_json_payload_lines(text) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let entry = serde_json::json!({
            "timestamp_ms": timestamp_ms,
            "stream_type": stream_type_label(stream_type),
            "raw": raw,
            "payload": payload,
        });
        append_line(&path, &serde_json::to_string(&entry)?)?;
    }
    Ok(())
}

fn stream_type_label(stream_type: OutputStreamType) -> &'static str {
    match stream_type {
        OutputStreamType::Stdout => "stdout",
        OutputStreamType::Stderr => "stderr",
        OutputStreamType::System => "system",
    }
}

pub fn collect_json_payload_lines(text: &str) -> Vec<(String, Value)> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let parsed = serde_json::from_str::<Value>(trimmed).ok()?;
            if parsed.is_object() || parsed.is_array() {
                Some((trimmed.to_string(), parsed))
            } else {
                None
            }
        })
        .collect()
}

pub fn append_line(path: &Path, line: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{RunId, Timestamp};
    use uuid::Uuid;

    fn temp_run_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ao-ipc-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn persist_run_event_writes_events_jsonl() {
        let run_dir = temp_run_dir();
        let run_id = RunId("run-persist-001".to_string());

        persist_run_event(&run_dir, &AgentRunEvent::Started { run_id: run_id.clone(), timestamp: Timestamp::now() })
            .expect("persist started");
        persist_run_event(
            &run_dir,
            &AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "hello\n".to_string(),
            },
        )
        .expect("persist output chunk");
        persist_run_event(&run_dir, &AgentRunEvent::Finished { run_id, exit_code: Some(0), duration_ms: 100 })
            .expect("persist finished");

        let events_path = run_dir.join("events.jsonl");
        assert!(events_path.exists());
        let contents = std::fs::read_to_string(&events_path).expect("read events");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"kind\":\"started\""));
        assert!(lines[1].contains("\"kind\":\"output_chunk\""));
        assert!(lines[2].contains("\"kind\":\"finished\""));

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn persist_run_event_writes_json_output_for_output_chunk() {
        let run_dir = temp_run_dir();
        let run_id = RunId("run-persist-002".to_string());

        persist_run_event(
            &run_dir,
            &AgentRunEvent::OutputChunk {
                run_id: run_id.clone(),
                stream_type: OutputStreamType::Stdout,
                text: "plain text\n{\"type\":\"turn.completed\"}\n".to_string(),
            },
        )
        .expect("persist output chunk with json");

        let json_output_path = run_dir.join("json-output.jsonl");
        assert!(json_output_path.exists());
        let contents = std::fs::read_to_string(&json_output_path).expect("read json-output");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1, "only JSON lines are extracted");
        assert!(lines[0].contains("\"turn.completed\""));
        assert!(lines[0].contains("\"stream_type\":\"stdout\""));
        assert!(lines[0].contains("\"timestamp_ms\""));

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn persist_run_event_non_output_chunk_does_not_write_json_output() {
        let run_dir = temp_run_dir();
        let run_id = RunId("run-persist-003".to_string());

        persist_run_event(&run_dir, &AgentRunEvent::Thinking { run_id, content: "{\"kind\":\"thought\"}".to_string() })
            .expect("persist thinking");

        assert!(run_dir.join("events.jsonl").exists());
        assert!(!run_dir.join("json-output.jsonl").exists(), "Thinking events do not produce json-output");

        let _ = std::fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn collect_json_payload_lines_skips_plain_text() {
        let text = "plain text\n{\"key\":\"value\"}\n[1,2,3]\n\"just a string\"\n42\n";
        let pairs = collect_json_payload_lines(text);
        assert_eq!(pairs.len(), 2);
        assert!(pairs[0].0.contains("key"));
        assert!(pairs[1].0.contains('['));
    }

    #[test]
    fn run_dir_uses_scoped_state_root() {
        let project_root = std::env::temp_dir().join("ao-run-dir-test");
        let run_id = RunId("run-dir-abc".to_string());
        let dir = run_dir(project_root.to_str().unwrap(), &run_id, None);
        assert!(dir.ends_with("run-dir-abc"));
    }
}
