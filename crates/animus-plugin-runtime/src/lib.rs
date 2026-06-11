//! Shared runtime for Animus STDIO provider plugins (claude/codex/gemini/opencode/oai).
//!
//! Each provider binary plugs into this runtime by implementing
//! [`ProviderBackend`] (or wiring a plain [`SessionBackend`] via
//! [`SessionBackendProvider`]) and calling [`run_provider`] from `main`.
//! The runtime takes care of:
//!
//! - JSON-RPC stdin/stdout loop and lifecycle (initialize, $/ping, shutdown, exit)
//! - `--manifest` and `--help` CLI shortcuts
//! - `agent/run`, `agent/resume`, `agent/cancel`, `health/check` dispatch
//! - Streaming `agent/output`, `agent/thinking`, `agent/toolCall`,
//!   `agent/toolResult`, `agent/error` notifications back to the host as the
//!   wrapped `SessionBackend` emits events
//! - Final aggregated result with `output`, `metadata`, `tool_calls`,
//!   `tool_results`, `thinking`, `errors`, `exit_code`, `duration_ms`, `backend`
//!
//! With this contract, any wrapped session backend gets live-streaming and
//! collect-and-return semantics simultaneously without per-provider plumbing.

use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use animus_plugin_protocol::{
    error_codes, HealthCheckResult, HealthStatus, InitializeResult, PluginCapabilities, PluginInfo, PluginManifest,
    RpcError, RpcNotification, RpcRequest, RpcResponse, PROTOCOL_VERSION,
};
use animus_session_backend::session::{
    session_backend::SessionBackend, session_event::SessionEvent, session_request::SessionRequest,
};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

/// Manifest + identity for a provider plugin.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub plugin_name: &'static str,
    pub plugin_version: &'static str,
    pub description: &'static str,
    /// Tool name passed through into the wrapped `SessionRequest`.
    pub default_tool: &'static str,
    /// Default model when callers omit one.
    pub default_model: &'static str,
}

impl ProviderInfo {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: self.plugin_name.to_string(),
            version: self.plugin_version.to_string(),
            plugin_kind: animus_plugin_protocol::PLUGIN_KIND_PROVIDER.to_string(),
            description: self.description.to_string(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            capabilities: vec![
                "agent/run".to_string(),
                "agent/cancel".to_string(),
                "agent/resume".to_string(),
                "health/check".to_string(),
            ],
            env_required: Vec::new(),
            notification_buffer_size: None,
        }
    }

    fn initialize_result(&self) -> InitializeResult {
        InitializeResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            plugin_info: PluginInfo {
                name: self.plugin_name.to_string(),
                version: self.plugin_version.to_string(),
                plugin_kind: animus_plugin_protocol::PLUGIN_KIND_PROVIDER.to_string(),
                description: Some(self.description.to_string()),
            },
            capabilities: PluginCapabilities {
                methods: vec![
                    "agent/run".to_string(),
                    "agent/cancel".to_string(),
                    "agent/resume".to_string(),
                    "health/check".to_string(),
                ],
                streaming: true,
                progress: false,
                cancellation: true,
                projections: Vec::new(),
                subject_kinds: Vec::new(),
                mcp_tools: Vec::new(),
            },
        }
    }
}

#[derive(Debug, Deserialize)]
struct AgentRunParams {
    #[serde(default)]
    session_id: Option<String>,
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    cwd: PathBuf,
    #[serde(default)]
    project_root: Option<PathBuf>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    claude_profile: Option<String>,
    #[serde(default)]
    mcp_servers: Option<Value>,
    #[serde(default)]
    tools: Option<Value>,
    #[serde(default)]
    response_schema: Option<Value>,
    #[serde(default)]
    runtime_contract: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct AgentCancelParams {
    session_id: String,
}

/// Trait wrapping a `SessionBackend`. Most providers can use the blanket impl on
/// `Arc<dyn SessionBackend>` directly via [`SessionBackendProvider::new`].
#[async_trait]
pub trait ProviderBackend: Send + Sync + 'static {
    async fn start(
        &self,
        request: SessionRequest,
        resume_session: Option<&str>,
    ) -> animus_session_backend::error::Result<animus_session_backend::session::session_run::SessionRun>;

    async fn cancel(&self, session_id: &str) -> animus_session_backend::error::Result<()>;
}

/// Adapter that wraps any `Arc<dyn SessionBackend>` so the runtime can drive it.
pub struct SessionBackendProvider {
    backend: Arc<dyn SessionBackend>,
}

impl SessionBackendProvider {
    pub fn new(backend: Arc<dyn SessionBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ProviderBackend for SessionBackendProvider {
    async fn start(
        &self,
        request: SessionRequest,
        resume_session: Option<&str>,
    ) -> animus_session_backend::error::Result<animus_session_backend::session::session_run::SessionRun> {
        match resume_session {
            Some(sid) => self.backend.resume_session(request, sid).await,
            None => self.backend.start_session(request).await,
        }
    }

    async fn cancel(&self, session_id: &str) -> animus_session_backend::error::Result<()> {
        self.backend.terminate_session(session_id).await
    }
}

/// Stable entrypoint for a provider plugin. Call this from `#[tokio::main]`.
pub async fn run_provider<P: ProviderBackend>(info: ProviderInfo, backend: P) -> Result<()> {
    handle_cli_args(&info);

    if io::stdin().is_terminal() {
        eprintln!("{} is a STDIO plugin; pipe JSON-RPC on stdin or pass --manifest", info.plugin_name);
        std::process::exit(2);
    }

    let backend = Arc::new(backend);
    let stdout = Arc::new(Mutex::new(tokio::io::stdout()));
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: RpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(error) => {
                tracing::warn!(plugin = %info.plugin_name, %error, "invalid JSON-RPC frame");
                continue;
            }
        };

        let backend = backend.clone();
        let stdout = stdout.clone();
        let info = info.clone();
        tokio::spawn(async move {
            handle_request(request, info, backend, stdout).await;
        });
    }

    Ok(())
}

fn handle_cli_args(info: &ProviderInfo) {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--manifest" | "-m" => print_manifest_and_exit(info),
            "--help" | "-h" => {
                eprintln!("{} {} — STDIO provider plugin for Animus", info.plugin_name, info.plugin_version);
                eprintln!("Usage:");
                eprintln!("  {} --manifest    Print plugin manifest as JSON and exit", info.plugin_name);
                eprintln!("  {}               Run JSON-RPC loop on stdin/stdout", info.plugin_name);
                std::process::exit(0);
            }
            _ => {}
        }
    }
}

fn print_manifest_and_exit(info: &ProviderInfo) -> ! {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{}", serde_json::to_string(&info.manifest()).expect("serialize manifest"));
    let _ = stdout.flush();
    std::process::exit(0);
}

async fn handle_request<P: ProviderBackend>(
    request: RpcRequest,
    info: ProviderInfo,
    backend: Arc<P>,
    stdout: Arc<Mutex<tokio::io::Stdout>>,
) {
    let id = request.id.clone();
    let response = match request.method.as_str() {
        "initialize" => Some(match serde_json::to_value(info.initialize_result()) {
            Ok(value) => RpcResponse::ok(id, value),
            Err(error) => RpcResponse::err(
                id,
                RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!("failed to encode initialize result: {error}"),
                    data: None,
                },
            ),
        }),
        "initialized" => None,
        "$/ping" => Some(RpcResponse::ok(id, json!({}))),
        "health/check" => Some(
            match serde_json::to_value(HealthCheckResult {
                status: HealthStatus::Healthy,
                uptime_ms: None,
                memory_usage_bytes: None,
                last_error: None,
            }) {
                Ok(value) => RpcResponse::ok(id, value),
                Err(error) => RpcResponse::err(
                    id,
                    RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("failed to encode health result: {error}"),
                        data: None,
                    },
                ),
            },
        ),
        "agent/run" => Some(handle_agent_run(id, request.params, &info, backend.clone(), stdout.clone(), None).await),
        "agent/resume" => {
            let resume_session = request
                .params
                .as_ref()
                .and_then(|p| p.get("session_id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Some(handle_agent_run(id, request.params, &info, backend.clone(), stdout.clone(), resume_session).await)
        }
        "agent/cancel" => Some(handle_agent_cancel(id, request.params, backend.clone(), &info).await),
        "shutdown" => Some(RpcResponse::ok(id, json!({}))),
        "exit" => std::process::exit(0),
        other if other.starts_with("$/") => None,
        other => Some(RpcResponse::err(
            id,
            RpcError {
                code: error_codes::METHOD_NOT_FOUND,
                message: format!("method '{other}' not implemented by {}", info.plugin_name),
                data: None,
            },
        )),
    };

    if let Some(response) = response {
        write_frame(&stdout, &response).await;
    }
}

async fn write_frame<T: serde::Serialize>(stdout: &Arc<Mutex<tokio::io::Stdout>>, frame: &T) {
    if let Ok(mut payload) = serde_json::to_string(frame) {
        payload.push('\n');
        let mut guard = stdout.lock().await;
        let _ = guard.write_all(payload.as_bytes()).await;
        let _ = guard.flush().await;
    }
}

async fn send_notification(stdout: &Arc<Mutex<tokio::io::Stdout>>, method: impl Into<String>, params: Value) {
    let notification = RpcNotification::new(method, Some(params));
    write_frame(stdout, &notification).await;
}

async fn handle_agent_run<P: ProviderBackend>(
    id: Option<Value>,
    params: Option<Value>,
    info: &ProviderInfo,
    backend: Arc<P>,
    stdout: Arc<Mutex<tokio::io::Stdout>>,
    resume_session: Option<String>,
) -> RpcResponse {
    let params: AgentRunParams = match params.ok_or_else(|| invalid_params("missing params for agent/run")) {
        Ok(p) => match serde_json::from_value::<AgentRunParams>(p) {
            Ok(parsed) => parsed,
            Err(error) => return invalid_rpc(id, format!("invalid agent/run params: {error}")),
        },
        Err(error) => return error_rpc(id, error),
    };

    let session_request = build_session_request(info, params);
    let started_at = Instant::now();

    let run_result = backend.start(session_request, resume_session.as_deref()).await;
    let mut run = match run_result {
        Ok(run) => run,
        Err(error) => {
            return error_rpc(
                id,
                RpcError {
                    code: -1002,
                    message: format!("{} session start failed: {error}", info.plugin_name),
                    data: None,
                },
            )
        }
    };

    let session_id = run.session_id.clone();
    let backend_label = run.selected_backend.clone();
    let mut output = String::new();
    let mut metadata = Vec::<Value>::new();
    let mut tool_calls = Vec::<Value>::new();
    let mut tool_results = Vec::<Value>::new();
    let mut thinking = Vec::<String>::new();
    let mut errors = Vec::<String>::new();
    let mut exit_code: Option<i32> = None;

    while let Some(event) = run.events.recv().await {
        match event {
            SessionEvent::Started { .. } => {}
            SessionEvent::TextDelta { text } => {
                send_notification(&stdout, "agent/output", json!({ "text": text, "session_id": session_id })).await;
                output.push_str(&text);
            }
            SessionEvent::FinalText { text } => {
                send_notification(
                    &stdout,
                    "agent/output",
                    json!({ "text": text, "session_id": session_id, "final": true }),
                )
                .await;
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str(&text);
            }
            SessionEvent::Thinking { text } => {
                send_notification(&stdout, "agent/thinking", json!({ "text": text, "session_id": session_id })).await;
                thinking.push(text);
            }
            SessionEvent::ToolCall { tool_name, arguments, server } => {
                send_notification(
                    &stdout,
                    "agent/toolCall",
                    json!({
                        "name": tool_name,
                        "arguments": arguments,
                        "server": server,
                        "session_id": session_id,
                    }),
                )
                .await;
                tool_calls.push(json!({ "tool": tool_name, "arguments": arguments, "server": server }));
            }
            SessionEvent::ToolResult { tool_name, output: tool_output, success } => {
                send_notification(
                    &stdout,
                    "agent/toolResult",
                    json!({
                        "name": tool_name,
                        "output": tool_output,
                        "success": success,
                        "session_id": session_id,
                    }),
                )
                .await;
                tool_results.push(json!({ "tool": tool_name, "output": tool_output, "success": success }));
            }
            SessionEvent::Artifact { artifact_id, metadata: m } => {
                metadata.push(json!({ "artifact_id": artifact_id, "metadata": m }));
            }
            SessionEvent::Metadata { metadata: m } => metadata.push(m),
            SessionEvent::Error { message, recoverable } => {
                send_notification(
                    &stdout,
                    "agent/error",
                    json!({ "message": message, "recoverable": recoverable, "session_id": session_id }),
                )
                .await;
                errors.push(message.clone());
                if !recoverable {
                    break;
                }
            }
            SessionEvent::Finished { exit_code: code } => {
                exit_code = code;
                break;
            }
        }
    }

    let duration_ms = started_at.elapsed().as_millis() as u64;
    let result = json!({
        "session_id": session_id,
        "exit_code": exit_code.unwrap_or(0),
        "output": output,
        "metadata": metadata,
        "tool_calls": tool_calls,
        "tool_results": tool_results,
        "thinking": thinking,
        "errors": errors,
        "duration_ms": duration_ms,
        "backend": backend_label,
    });
    RpcResponse::ok(id, result)
}

async fn handle_agent_cancel<P: ProviderBackend>(
    id: Option<Value>,
    params: Option<Value>,
    backend: Arc<P>,
    info: &ProviderInfo,
) -> RpcResponse {
    let params: AgentCancelParams = match params.ok_or_else(|| invalid_params("missing params for agent/cancel")) {
        Ok(p) => match serde_json::from_value::<AgentCancelParams>(p) {
            Ok(parsed) => parsed,
            Err(error) => return invalid_rpc(id, format!("invalid agent/cancel params: {error}")),
        },
        Err(error) => return error_rpc(id, error),
    };

    match backend.cancel(&params.session_id).await {
        Ok(()) => RpcResponse::ok(id, json!({ "session_id": params.session_id, "cancelled": true })),
        Err(error) => error_rpc(
            id,
            RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: format!("{} agent/cancel failed: {error}", info.plugin_name),
                data: None,
            },
        ),
    }
}

fn build_session_request(info: &ProviderInfo, params: AgentRunParams) -> SessionRequest {
    let mut extras = serde_json::Map::new();
    if let Some(system_prompt) = params.system_prompt {
        extras.insert("system_prompt".to_string(), Value::String(system_prompt));
    }
    if let Some(profile) = params.claude_profile {
        extras.insert("claude_profile".to_string(), Value::String(profile));
    }
    if let Some(tools) = params.tools {
        extras.insert("tools".to_string(), tools);
    }
    if let Some(schema) = params.response_schema {
        extras.insert("response_schema".to_string(), schema);
    }
    if let Some(contract) = params.runtime_contract {
        extras.insert("runtime_contract".to_string(), contract);
    }
    if let Some(sid) = params.session_id {
        extras.insert("session_id".to_string(), Value::String(sid));
    }

    SessionRequest {
        tool: info.default_tool.to_string(),
        model: params.model.unwrap_or_else(|| info.default_model.to_string()),
        prompt: params.prompt,
        cwd: params.cwd,
        project_root: params.project_root,
        mcp_endpoint: None,
        mcp_servers: params.mcp_servers,
        permission_mode: params.permission_mode,
        timeout_secs: params.timeout_secs,
        env_vars: params.env.into_iter().collect(),
        extras: Value::Object(extras),
    }
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError { code: error_codes::INVALID_PARAMS, message: message.into(), data: None }
}

fn invalid_rpc(id: Option<Value>, message: impl Into<String>) -> RpcResponse {
    RpcResponse::err(id, RpcError { code: error_codes::INVALID_PARAMS, message: message.into(), data: None })
}

fn error_rpc(id: Option<Value>, error: RpcError) -> RpcResponse {
    RpcResponse::err(id, error)
}
