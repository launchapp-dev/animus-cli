#[cfg(unix)]
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use orchestrator_core::runtime_contract;
use protocol::{AgentControlAction, AgentRunEvent, ModelStatus, OutputStreamType, RunId};
use serde_json::Value;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::time::Duration;

use crate::{AgentControlActionArg, AgentRunArgs, RunnerScopeArg};

#[cfg(unix)]
const MAX_UNIX_SOCKET_PATH_LEN: usize = 100;

impl From<AgentControlActionArg> for AgentControlAction {
    fn from(value: AgentControlActionArg) -> Self {
        match value {
            AgentControlActionArg::Pause => AgentControlAction::Pause,
            AgentControlActionArg::Resume => AgentControlAction::Resume,
            AgentControlActionArg::Terminate => AgentControlAction::Terminate,
        }
    }
}

pub(crate) struct RunnerScopeEnvGuard {
    previous: Option<String>,
    changed: bool,
}

impl RunnerScopeEnvGuard {
    pub(crate) fn new(scope: Option<&RunnerScopeArg>) -> Self {
        let previous = std::env::var("AO_RUNNER_SCOPE").ok();
        if let Some(scope) = scope {
            std::env::set_var("AO_RUNNER_SCOPE", runner_scope_label(scope));
            Self {
                previous,
                changed: true,
            }
        } else {
            Self {
                previous,
                changed: false,
            }
        }
    }
}

impl Drop for RunnerScopeEnvGuard {
    fn drop(&mut self) {
        if !self.changed {
            return;
        }

        if let Some(value) = &self.previous {
            std::env::set_var("AO_RUNNER_SCOPE", value);
        } else {
            std::env::remove_var("AO_RUNNER_SCOPE");
        }
    }
}

fn runner_scope_label(scope: &RunnerScopeArg) -> &'static str {
    match scope {
        RunnerScopeArg::Project => "project",
        RunnerScopeArg::Global => "global",
    }
}

fn runner_scope_from_env() -> String {
    std::env::var("AO_RUNNER_SCOPE")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "project".to_string())
}

fn canonicalize_cwd_in_project(path: &str, project_root: &str) -> Result<String> {
    let root = PathBuf::from(project_root);
    let root_canonical = root
        .canonicalize()
        .with_context(|| format!("failed to resolve project root '{}'", project_root))?;
    let candidate = PathBuf::from(path);
    let resolved_candidate = if candidate.is_absolute() {
        candidate
    } else {
        root_canonical.join(candidate)
    };
    let candidate_canonical = resolved_candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve cwd '{}'", resolved_candidate.display()))?;
    if !candidate_canonical.starts_with(&root_canonical) {
        return Err(anyhow!(
            "Security violation: cwd '{}' is not within project root '{}'",
            path,
            project_root
        ));
    }
    Ok(candidate_canonical.to_string_lossy().to_string())
}

fn default_global_config_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.launchpad.agent-orchestrator")
    }

    #[cfg(target_os = "windows")]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.launchpad.agent-orchestrator")
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("agent-orchestrator")
    }
}

pub(crate) fn runner_config_dir(project_root: &Path) -> PathBuf {
    let config_dir = if let Some(override_path) = std::env::var("AO_RUNNER_CONFIG_DIR")
        .ok()
        .or_else(|| std::env::var("AO_CONFIG_DIR").ok())
        .or_else(|| std::env::var("AGENT_ORCHESTRATOR_CONFIG_DIR").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        PathBuf::from(override_path)
    } else if runner_scope_from_env() == "global" {
        default_global_config_dir()
    } else {
        project_root.join(".ao").join("runner")
    };

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
pub(crate) async fn connect_runner(config_dir: &Path) -> Result<tokio::net::UnixStream> {
    let socket_path = config_dir.join("agent-runner.sock");
    let connect_timeout_secs = std::env::var("AO_RUNNER_CONNECT_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| secs.clamp(1, 30))
        .unwrap_or(5);
    let connect_future = tokio::net::UnixStream::connect(&socket_path);
    match tokio::time::timeout(Duration::from_secs(connect_timeout_secs), connect_future).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(error)) => {
            let stale_hint = if socket_path.exists() {
                " socket file exists and may be stale"
            } else {
                ""
            };
            Err(error).with_context(|| {
                format!(
                    "failed to connect to runner socket at {} (timeout={}s).{}",
                    socket_path.display(),
                    connect_timeout_secs,
                    stale_hint
                )
            })
        }
        Err(_) => Err(anyhow!(
            "timed out connecting to runner socket at {} after {}s; if no runner is active, remove stale socket and restart runner",
            socket_path.display(),
            connect_timeout_secs
        )),
    }
}

#[cfg(not(unix))]
pub(crate) async fn connect_runner(_config_dir: &Path) -> Result<tokio::net::TcpStream> {
    tokio::net::TcpStream::connect("127.0.0.1:9001")
        .await
        .context("failed to connect to runner at 127.0.0.1:9001")
}

pub(crate) async fn write_json_line<W: AsyncWrite + Unpin, T: serde::Serialize>(
    writer: &mut W,
    payload: &T,
) -> Result<()> {
    let json = serde_json::to_string(payload)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

pub(crate) fn build_agent_context(args: &AgentRunArgs, project_root: &str) -> Result<Value> {
    let mut context = if let Some(context_json) = &args.context_json {
        serde_json::from_str::<Value>(context_json)?
    } else {
        serde_json::json!({})
    };

    let context_obj = context
        .as_object_mut()
        .ok_or_else(|| anyhow!("agent context must be a JSON object"))?;

    context_obj
        .entry("tool".to_string())
        .or_insert_with(|| Value::String(args.tool.clone()));

    if let Some(prompt) = &args.prompt {
        context_obj
            .entry("prompt".to_string())
            .or_insert_with(|| Value::String(prompt.clone()));
    }

    let cwd = args
        .cwd
        .clone()
        .or_else(|| {
            context_obj
                .get("cwd")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| project_root.to_string());
    let cwd = canonicalize_cwd_in_project(&cwd, project_root)?;
    context_obj.insert("cwd".to_string(), Value::String(cwd));
    context_obj.insert(
        "project_root".to_string(),
        Value::String(project_root.to_string()),
    );

    if let Some(timeout_secs) = args.timeout_secs {
        context_obj
            .entry("timeout_secs".to_string())
            .or_insert_with(|| Value::from(timeout_secs));
    }

    if let Some(runtime_contract_json) = &args.runtime_contract_json {
        context_obj.insert(
            "runtime_contract".to_string(),
            serde_json::from_str::<Value>(runtime_contract_json)?,
        );
    } else if !context_obj.contains_key("runtime_contract") {
        let prompt = context_obj
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(runtime_contract) = build_runtime_contract(&args.tool, &args.model, prompt) {
            context_obj.insert("runtime_contract".to_string(), runtime_contract);
        }
    }

    Ok(context)
}

pub(crate) fn build_runtime_contract(tool: &str, model: &str, prompt: &str) -> Option<Value> {
    let mcp_endpoint = std::env::var("AO_MCP_ENDPOINT")
        .ok()
        .or_else(|| std::env::var("MCP_ENDPOINT").ok())
        .or_else(|| std::env::var("OPENCODE_MCP_ENDPOINT").ok());
    let mcp_agent_id = std::env::var("AO_MCP_AGENT_ID").ok();

    let mut runtime_contract = runtime_contract::build_runtime_contract(
        tool,
        model,
        prompt,
        None,
        None,
        mcp_endpoint.as_deref(),
        mcp_agent_id.as_deref(),
    )?;
    inject_codex_reasoning_effort_override(&mut runtime_contract, tool);
    Some(runtime_contract)
}

fn env_codex_reasoning_effort_override() -> Option<String> {
    std::env::var("AO_CODEX_REASONING_EFFORT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn inject_codex_reasoning_effort_override(runtime_contract: &mut Value, tool: &str) {
    if !tool.eq_ignore_ascii_case("codex") {
        return;
    }
    let Some(effort) = env_codex_reasoning_effort_override() else {
        return;
    };
    let Some(args) = runtime_contract
        .pointer_mut("/cli/launch/args")
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    let mut index = 0usize;
    while index + 1 < args.len() {
        let flag = args[index].as_str().unwrap_or_default();
        let value = args[index + 1].as_str().unwrap_or_default();
        if flag == "-c" && value.starts_with("model_reasoning_effort=") {
            args[index + 1] = Value::String(format!("model_reasoning_effort={effort}"));
            return;
        }
        index += 1;
    }

    let insert_at = args.len().saturating_sub(1);
    args.insert(insert_at, Value::String("-c".to_string()));
    args.insert(
        insert_at + 1,
        Value::String(format!("model_reasoning_effort={effort}")),
    );
}

pub(crate) fn print_agent_event(event: &AgentRunEvent, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "schema": "ao.agent.event.v1",
                "ok": true,
                "data": event
            }))?
        );
        return Ok(());
    }

    match event {
        AgentRunEvent::Started { run_id, .. } => {
            println!("run {} started", run_id.0);
        }
        AgentRunEvent::OutputChunk {
            stream_type, text, ..
        } => match stream_type {
            OutputStreamType::Stderr => eprintln!("{text}"),
            OutputStreamType::Stdout | OutputStreamType::System => println!("{text}"),
        },
        AgentRunEvent::Metadata {
            run_id,
            cost,
            tokens,
        } => {
            println!("run {} metadata: cost={cost:?} tokens={tokens:?}", run_id.0);
        }
        AgentRunEvent::Error { run_id, error } => {
            eprintln!("run {} error: {error}", run_id.0);
        }
        AgentRunEvent::Finished {
            run_id,
            exit_code,
            duration_ms,
        } => {
            println!(
                "run {} finished: exit_code={exit_code:?} duration_ms={duration_ms}",
                run_id.0
            );
        }
        AgentRunEvent::ToolCall { run_id, tool_info } => {
            println!("run {} tool_call {}", run_id.0, tool_info.tool_name);
        }
        AgentRunEvent::ToolResult {
            run_id,
            result_info,
        } => {
            println!(
                "run {} tool_result {} success={}",
                run_id.0, result_info.tool_name, result_info.success
            );
        }
        AgentRunEvent::Artifact {
            run_id,
            artifact_info,
        } => {
            println!("run {} artifact {}", run_id.0, artifact_info.artifact_id);
        }
        AgentRunEvent::Thinking { run_id, content } => {
            println!(
                "run {} thinking: {} chars",
                run_id.0,
                content.chars().count()
            );
        }
    }

    Ok(())
}

pub(crate) fn print_model_status(status: ModelStatus) {
    let availability = serde_json::to_string(&status.availability)
        .unwrap_or_else(|_| "unknown".to_string())
        .trim_matches('"')
        .to_string();
    if let Some(details) = status.details {
        println!("{}: {} ({})", status.model.0, availability, details);
    } else {
        println!("{}: {}", status.model.0, availability);
    }
}

pub(crate) fn default_model_status_targets() -> Vec<String> {
    let mut tools: Vec<String> = protocol::default_model_specs()
        .into_iter()
        .map(|(_, tool)| tool)
        .collect();
    tools.sort();
    tools.dedup();
    tools
}

pub(crate) fn event_matches_run(event: &AgentRunEvent, run_id: &RunId) -> bool {
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

pub(crate) fn run_dir(project_root: &str, run_id: &RunId, base_override: Option<&str>) -> PathBuf {
    let base = base_override
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(project_root).join(".ao").join("runs"));
    base.join(&run_id.0)
}

pub(crate) fn persist_agent_event(run_dir: &Path, event: &AgentRunEvent) -> Result<()> {
    let path = run_dir.join("events.jsonl");
    let line = serde_json::to_string(event)?;
    append_line(&path, &line)
}

pub(crate) fn persist_json_output(
    run_dir: &Path,
    stream_type: OutputStreamType,
    text: &str,
) -> Result<()> {
    let path = run_dir.join("json-output.jsonl");
    for (raw, payload) in collect_json_payload_lines(text) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
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

pub(crate) fn collect_json_payload_lines(text: &str) -> Vec<(String, Value)> {
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

pub(crate) fn append_line(path: &Path, line: &str) -> Result<()> {
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
