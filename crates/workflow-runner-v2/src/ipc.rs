#[cfg(unix)]
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use orchestrator_core::runtime_contract;
use protocol::{
    AgentRunEvent, AgentRunRequest, IpcAuthRequest, IpcAuthResult, ModelId, RunId,
    MAX_UNIX_SOCKET_PATH_LEN, PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::Duration;
use uuid::Uuid;

fn scoped_ao_root(project_root: &Path) -> Option<PathBuf> {
    protocol::scoped_state_root(project_root)
}

pub fn runner_config_dir(project_root: &Path) -> PathBuf {
    let config_dir = scoped_ao_root(project_root)
        .unwrap_or_else(|| project_root.join(".ao"))
        .join("runner");

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
    let shortened = std::env::temp_dir()
        .join("ao-runner")
        .join(format!("{digest:016x}"));
    let _ = std::fs::create_dir_all(&shortened);
    let _ = std::fs::write(
        shortened.join("origin-path.txt"),
        config_dir.to_string_lossy().as_bytes(),
    );
    shortened
}

#[cfg(unix)]
pub async fn connect_runner(config_dir: &Path) -> Result<tokio::net::UnixStream> {
    let socket_path = config_dir.join("agent-runner.sock");
    let connect_timeout_secs: u64 = 5;
    let connect_future = tokio::net::UnixStream::connect(&socket_path);
    match tokio::time::timeout(Duration::from_secs(connect_timeout_secs), connect_future).await {
        Ok(Ok(mut stream)) => {
            authenticate_runner_stream(&mut stream, config_dir)
                .await
                .map_err(|error| {
                    anyhow!(
                        "failed to authenticate runner connection at {}: {error}",
                        socket_path.display()
                    )
                })?;
            Ok(stream)
        }
        Ok(Err(error)) => {
            let base_message = format!(
                "failed to connect to runner socket at {} (timeout={}s)",
                socket_path.display(),
                connect_timeout_secs
            );
            let hint = if socket_path.exists() {
                format!("{base_message}. socket file exists and may be stale")
            } else {
                base_message
            };
            Err(anyhow!("{hint}: {error}"))
        }
        Err(_) => Err(anyhow!(
            "timed out connecting to runner socket at {} after {}s; if no runner is active, remove stale socket and restart runner",
            socket_path.display(),
            connect_timeout_secs
        )),
    }
}

#[cfg(not(unix))]
pub async fn connect_runner(config_dir: &Path) -> Result<tokio::net::TcpStream> {
    let mut stream = tokio::net::TcpStream::connect("127.0.0.1:9001")
        .await
        .map_err(|error| anyhow!("failed to connect to runner at 127.0.0.1:9001: {error}"))?;
    authenticate_runner_stream(&mut stream, config_dir)
        .await
        .map_err(|error| {
            anyhow!("failed to authenticate runner connection at 127.0.0.1:9001: {error}")
        })?;
    Ok(stream)
}

pub async fn authenticate_runner_stream<S>(stream: &mut S, config_dir: &Path) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let token = protocol::Config::load_from_dir(config_dir)
        .map_err(|error| {
            anyhow!(
                "failed to load runner config for authentication from {}: {error}",
                config_dir.display()
            )
        })?
        .get_token()
        .map_err(|error| {
            format!(
                "agent runner token unavailable; set AGENT_RUNNER_TOKEN or configure agent_runner_token: {error}"
            )
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
    let message = response
        .message
        .unwrap_or_else(|| "unauthorized".to_string());
    Err(anyhow!(
        "runner authentication failed ({failure_code}): {message}"
    ))
}

pub async fn write_json_line<W: AsyncWrite + Unpin, T: serde::Serialize>(
    writer: &mut W,
    payload: &T,
) -> Result<()> {
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

    let mut runtime_contract = runtime_contract::build_runtime_contract(
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
        AgentRunEvent::Started {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::OutputChunk {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::Metadata {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::Error {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::Finished {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::ToolCall {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::ToolResult {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::Artifact {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
        AgentRunEvent::Thinking {
            run_id: event_run_id,
            ..
        } => event_run_id == run_id,
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
        scoped_ao_root(Path::new(project_root))
            .unwrap_or_else(|| Path::new(project_root).join(".ao"))
            .join("runs")
    });
    base.join(&run_id.0)
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

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

pub async fn run_prompt_against_runner(
    project_root: &str,
    prompt: &str,
    model: &str,
    tool: &str,
    timeout_secs: u64,
) -> Result<String> {
    let run_id = RunId(format!("task-gen-{}", Uuid::new_v4()));
    let mut context = serde_json::json!({
        "tool": tool,
        "prompt": prompt,
        "cwd": project_root,
        "project_root": project_root,
        "planning_stage": "task-generation",
        "allowed_tools": ["Read", "Glob", "Grep", "WebSearch"],
        "timeout_secs": timeout_secs,
    });
    if let Some(runtime_contract) = build_runtime_contract(tool, model, prompt) {
        context["runtime_contract"] = runtime_contract;
    }

    let request = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run_id.clone(),
        model: ModelId(model.to_string()),
        context,
        timeout_secs: Some(timeout_secs),
    };

    let config_dir = runner_config_dir(Path::new(project_root));
    let stream = connect_runner(&config_dir).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut transcript = String::new();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, &run_id) {
            continue;
        }

        match event {
            AgentRunEvent::OutputChunk { text, .. } => {
                transcript.push_str(&text);
                transcript.push('\n');
            }
            AgentRunEvent::Thinking { content, .. } => {
                transcript.push_str(&content);
                transcript.push('\n');
            }
            AgentRunEvent::Error { error, .. } => {
                return Err(anyhow!("task generation run failed: {error}"));
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    return Err(anyhow!(
                        "task generation run exited with non-zero code: {:?}",
                        exit_code
                    ));
                }
                break;
            }
            _ => {}
        }
    }

    if transcript.trim().is_empty() {
        return Err(anyhow!("task generation run produced empty output"));
    }

    Ok(transcript)
}
