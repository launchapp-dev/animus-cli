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
use std::time::{Duration, Instant};

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
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
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
            plugin_kinds: Vec::new(),
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
                plugin_kinds: Vec::new(),
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
            kind_capabilities: std::collections::HashMap::new(),
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
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    control_session_id: Option<String>,
    /// Forward-compat: unknown params survive into `SessionRequest.extras`
    /// instead of being silently dropped.
    #[serde(flatten)]
    extras: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct AgentCancelParams {
    session_id: String,
}

/// Map of host-minted `control_session_id` -> backend-issued session id.
/// `agent/run` parks a [`CancelRoute::Pending`] marker before the backend
/// starts and upgrades it to [`CancelRoute::Ready`] as soon as the backend
/// surfaces its own session id (on `start` return, or via the first
/// `SessionEvent::Started` that carries one); `agent/cancel` translates an
/// incoming control id to the backend's real id before calling
/// [`ProviderBackend::cancel`],
/// briefly waiting out the `Pending` window. Unknown ids fall through
/// unchanged so plugins keep working against hosts that send the provider's
/// real id (or no control id at all).
type CancelRoutes = Arc<Mutex<HashMap<String, CancelRoute>>>;

#[derive(Debug, Clone)]
enum CancelRoute {
    /// `agent/run` accepted the control id but the backend has not produced
    /// its own session id yet; cancels for this id wait instead of falling
    /// through with an id the backend would not recognize.
    Pending,
    Ready(String),
}

/// How long `agent/cancel` waits for a [`CancelRoute::Pending`] entry to turn
/// `Ready` before falling back to the raw wire id. Kept well below the host's
/// 10s cancel deadline so the reply still arrives in time.
const CANCEL_PENDING_WAIT: Duration = Duration::from_secs(5);

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
    let cancel_routes: CancelRoutes = Arc::new(Mutex::new(HashMap::new()));
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
        let cancel_routes = cancel_routes.clone();
        tokio::spawn(async move {
            handle_request(request, info, backend, stdout, cancel_routes).await;
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

async fn handle_request<P: ProviderBackend, W: AsyncWrite + Unpin + Send + 'static>(
    request: RpcRequest,
    info: ProviderInfo,
    backend: Arc<P>,
    stdout: Arc<Mutex<W>>,
    cancel_routes: CancelRoutes,
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
        "agent/run" => Some(
            handle_agent_run(id, request.params, &info, backend.clone(), stdout.clone(), None, cancel_routes.clone())
                .await,
        ),
        "agent/resume" => {
            let resume_session = request
                .params
                .as_ref()
                .and_then(|p| p.get("session_id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Some(
                handle_agent_run(
                    id,
                    request.params,
                    &info,
                    backend.clone(),
                    stdout.clone(),
                    resume_session,
                    cancel_routes.clone(),
                )
                .await,
            )
        }
        "agent/cancel" => {
            Some(handle_agent_cancel(id, request.params, backend.clone(), &info, cancel_routes.clone()).await)
        }
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

async fn write_frame<T: serde::Serialize, W: AsyncWrite + Unpin>(stdout: &Arc<Mutex<W>>, frame: &T) {
    if let Ok(mut payload) = serde_json::to_string(frame) {
        payload.push('\n');
        let mut guard = stdout.lock().await;
        let _ = guard.write_all(payload.as_bytes()).await;
        let _ = guard.flush().await;
    }
}

async fn send_notification<W: AsyncWrite + Unpin>(stdout: &Arc<Mutex<W>>, method: impl Into<String>, params: Value) {
    let notification = RpcNotification::new(method, Some(params));
    write_frame(stdout, &notification).await;
}

async fn handle_agent_run<P: ProviderBackend, W: AsyncWrite + Unpin>(
    id: Option<Value>,
    params: Option<Value>,
    info: &ProviderInfo,
    backend: Arc<P>,
    stdout: Arc<Mutex<W>>,
    resume_session: Option<String>,
    cancel_routes: CancelRoutes,
) -> RpcResponse {
    let params: AgentRunParams = match params.ok_or_else(|| invalid_params("missing params for agent/run")) {
        Ok(p) => match serde_json::from_value::<AgentRunParams>(p) {
            Ok(parsed) => parsed,
            Err(error) => return invalid_rpc(id, format!("invalid agent/run params: {error}")),
        },
        Err(error) => return error_rpc(id, error),
    };

    let control_session_id = params.control_session_id.clone();
    let session_request = build_session_request(info, params);
    let started_at = Instant::now();

    // Park a Pending marker before the backend starts so a cancel racing the
    // startup window waits for the real session id instead of falling through
    // with the control id.
    if let Some(control) = control_session_id.as_deref() {
        cancel_routes.lock().await.insert(control.to_string(), CancelRoute::Pending);
    }
    let run_result = backend.start(session_request, resume_session.as_deref()).await;
    let mut run = match run_result {
        Ok(run) => run,
        Err(error) => {
            if let Some(control) = control_session_id.as_deref() {
                cancel_routes.lock().await.remove(control);
            }
            return error_rpc(
                id,
                RpcError {
                    code: -1002,
                    message: format!("{} session start failed: {error}", info.plugin_name),
                    data: None,
                },
            );
        }
    };

    let mut session_id = run.session_id.clone();
    if let Some(control) = control_session_id.as_deref() {
        if let Some(inner) = session_id.as_deref() {
            cancel_routes.lock().await.insert(control.to_string(), CancelRoute::Ready(inner.to_string()));
        }
        // No id yet: keep the Pending marker; the first `Started` event that
        // carries one upgrades the route below.
    }
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
            SessionEvent::Started { session_id: started_id, .. } => {
                if session_id.is_none() {
                    if let Some(inner) = started_id {
                        if let Some(control) = control_session_id.as_deref() {
                            cancel_routes.lock().await.insert(control.to_string(), CancelRoute::Ready(inner.clone()));
                        }
                        session_id = Some(inner);
                    }
                }
            }
            SessionEvent::TextDelta { text } => {
                send_notification(
                    &stdout,
                    "agent/output",
                    json!({ "text": text, "session_id": session_id, "final": false }),
                )
                .await;
                output.push_str(&text);
            }
            SessionEvent::FinalText { text } => {
                send_notification(
                    &stdout,
                    "agent/output",
                    json!({ "text": text, "session_id": session_id, "final": true }),
                )
                .await;
                // Final text is canonical: it replaces accumulated deltas so
                // the result's `output` never duplicates streamed text.
                output = text;
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
            SessionEvent::InteractionRequested { id, kind } => {
                send_notification(
                    &stdout,
                    "agent/interaction",
                    json!({ "id": id, "kind": kind, "status": "requested", "session_id": session_id }),
                )
                .await;
            }
            SessionEvent::InteractionResolved { id, decision } => {
                send_notification(
                    &stdout,
                    "agent/interaction",
                    json!({ "id": id, "decision": decision, "status": "resolved", "session_id": session_id }),
                )
                .await;
            }
            SessionEvent::Error { message, recoverable } => {
                send_notification(
                    &stdout,
                    "agent/error",
                    json!({ "message": message, "recoverable": recoverable, "session_id": session_id }),
                )
                .await;
                errors.push(message.clone());
                if !recoverable {
                    exit_code = Some(1);
                    break;
                }
            }
            SessionEvent::Finished { exit_code: code } => {
                exit_code = code;
                break;
            }
        }
    }

    if let Some(control) = control_session_id.as_deref() {
        cancel_routes.lock().await.remove(control);
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
    cancel_routes: CancelRoutes,
) -> RpcResponse {
    let params: AgentCancelParams = match params.ok_or_else(|| invalid_params("missing params for agent/cancel")) {
        Ok(p) => match serde_json::from_value::<AgentCancelParams>(p) {
            Ok(parsed) => parsed,
            Err(error) => return invalid_rpc(id, format!("invalid agent/cancel params: {error}")),
        },
        Err(error) => return error_rpc(id, error),
    };

    let deadline = Instant::now() + CANCEL_PENDING_WAIT;
    let target_session_id = loop {
        {
            let routes = cancel_routes.lock().await;
            match routes.get(&params.session_id) {
                Some(CancelRoute::Ready(inner)) => break inner.clone(),
                // The run is still starting; wait for the backend's real id.
                Some(CancelRoute::Pending) => {}
                // Unknown id: pass through unchanged (back-compat with hosts
                // that send the provider's own session id).
                None => break params.session_id.clone(),
            }
        }
        if Instant::now() >= deadline {
            break params.session_id.clone();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    match backend.cancel(&target_session_id).await {
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
    let mut extras = params.extras;
    if let Some(system_prompt) = params.system_prompt {
        extras.insert("system_prompt".to_string(), Value::String(system_prompt));
    }
    if let Some(profile) = params.claude_profile {
        extras.insert("claude_profile".to_string(), Value::String(profile));
    }
    if let Some(mcp) = &params.mcp_servers {
        // Keep the extras copy for back-compat with transports that read it
        // from there; the typed `SessionRequest.mcp_servers` field below is
        // the canonical channel as of animus-protocol v0.1.19.
        extras.insert("mcp_servers".to_string(), mcp.clone());
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
    if let Some(effort) = params.reasoning_effort {
        extras.insert("reasoning_effort".to_string(), Value::String(effort));
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
        permission_mode: params.permission_mode,
        timeout_secs: params.timeout_secs,
        env_vars: params.env.into_iter().collect(),
        mcp_servers: params.mcp_servers,
        extras: Value::Object(extras),
        // `AgentRunParams` carries no actor today; the actor reaches a provider
        // session only when relayed from the workflow runner (out-of-tree). Never
        // synthesized from local run params.
        actor: None,
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

#[cfg(test)]
mod reasoning_effort_tests {
    use super::*;
    use serde_json::json;

    fn provider_info() -> ProviderInfo {
        ProviderInfo {
            plugin_name: "test-provider",
            plugin_version: "0.0.0",
            description: "test",
            default_tool: "codex",
            default_model: "test-model",
        }
    }

    #[test]
    fn build_session_request_forwards_reasoning_effort() {
        let params: AgentRunParams = serde_json::from_value(json!({
            "prompt": "hi",
            "cwd": "/tmp",
            "reasoning_effort": "high",
        }))
        .expect("params deserialize");
        let request = build_session_request(&provider_info(), params);
        assert_eq!(
            request.extras.get("reasoning_effort").and_then(Value::as_str),
            Some("high"),
            "reasoning_effort must reach SessionRequest.extras for the transport to map"
        );
    }

    #[test]
    fn build_session_request_omits_reasoning_effort_when_absent() {
        let params: AgentRunParams = serde_json::from_value(json!({
            "prompt": "hi",
            "cwd": "/tmp",
        }))
        .expect("params deserialize");
        let request = build_session_request(&provider_info(), params);
        assert!(request.extras.get("reasoning_effort").is_none(), "absent reasoning_effort must not inject the key");
    }

    #[test]
    fn build_session_request_passes_unknown_params_through_extras() {
        let params: AgentRunParams = serde_json::from_value(json!({
            "prompt": "hi",
            "cwd": "/tmp",
            "reasoning_effort": "low",
            "control_session_id": "ctrl-9",
            "future_field": { "nested": true },
        }))
        .expect("params deserialize");
        let request = build_session_request(&provider_info(), params);
        assert_eq!(
            request.extras.get("future_field"),
            Some(&json!({ "nested": true })),
            "unknown forward-compat params must survive into SessionRequest.extras"
        );
        assert_eq!(
            request.extras.get("reasoning_effort").and_then(Value::as_str),
            Some("low"),
            "named extras keys must still be forwarded alongside flattened ones"
        );
        assert!(
            request.extras.get("control_session_id").is_none(),
            "control_session_id is a named field and must never reach extras via the catch-all"
        );
    }

    #[test]
    fn build_session_request_does_not_leak_control_session_id_into_extras() {
        let params: AgentRunParams = serde_json::from_value(json!({
            "prompt": "hi",
            "cwd": "/tmp",
            "control_session_id": "ctrl-1",
        }))
        .expect("params deserialize");
        let request = build_session_request(&provider_info(), params);
        assert!(
            request.extras.get("control_session_id").is_none(),
            "host-internal control_session_id must not reach the wrapped backend"
        );
    }
}

#[cfg(test)]
mod run_loop_tests {
    use super::*;
    use animus_session_backend::session::session_run::SessionRun;
    use serde_json::json;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn provider_info() -> ProviderInfo {
        ProviderInfo {
            plugin_name: "test-provider",
            plugin_version: "0.0.0",
            description: "test",
            default_tool: "codex",
            default_model: "test-model",
        }
    }

    struct TestBackend {
        run: Mutex<Option<SessionRun>>,
        cancels: std::sync::Mutex<Vec<String>>,
        start_gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl TestBackend {
        fn with_run(run: SessionRun) -> Self {
            Self {
                run: Mutex::new(Some(run)),
                cancels: std::sync::Mutex::new(Vec::new()),
                start_gate: Mutex::new(None),
            }
        }

        fn with_gated_start(run: SessionRun, gate: tokio::sync::oneshot::Receiver<()>) -> Self {
            Self {
                run: Mutex::new(Some(run)),
                cancels: std::sync::Mutex::new(Vec::new()),
                start_gate: Mutex::new(Some(gate)),
            }
        }

        fn cancelled(&self) -> Vec<String> {
            self.cancels.lock().expect("cancels mutex poisoned").clone()
        }
    }

    #[async_trait]
    impl ProviderBackend for TestBackend {
        async fn start(
            &self,
            _request: SessionRequest,
            _resume_session: Option<&str>,
        ) -> animus_session_backend::error::Result<SessionRun> {
            if let Some(gate) = self.start_gate.lock().await.take() {
                let _ = gate.await;
            }
            Ok(self.run.lock().await.take().expect("run prepared for test backend"))
        }

        async fn cancel(&self, session_id: &str) -> animus_session_backend::error::Result<()> {
            self.cancels.lock().expect("cancels mutex poisoned").push(session_id.to_string());
            Ok(())
        }
    }

    fn test_run(session_id: &str, events: mpsc::Receiver<SessionEvent>) -> SessionRun {
        SessionRun {
            session_id: Some(session_id.to_string()),
            events,
            selected_backend: "test".to_string(),
            fallback_reason: None,
            pid: None,
        }
    }

    fn test_run_without_id(events: mpsc::Receiver<SessionEvent>) -> SessionRun {
        SessionRun { session_id: None, events, selected_backend: "test".to_string(), fallback_reason: None, pid: None }
    }

    fn fresh_routes() -> CancelRoutes {
        Arc::new(Mutex::new(HashMap::new()))
    }

    fn test_stdout() -> Arc<Mutex<tokio::io::Stdout>> {
        Arc::new(Mutex::new(tokio::io::stdout()))
    }

    fn capture_sink() -> Arc<Mutex<Vec<u8>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    async fn captured_frames(sink: &Arc<Mutex<Vec<u8>>>) -> Vec<Value> {
        String::from_utf8(sink.lock().await.clone())
            .expect("captured frames must be utf-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("captured frame must be JSON"))
            .collect()
    }

    fn output_frames(frames: &[Value]) -> Vec<Value> {
        frames
            .iter()
            .filter(|frame| frame.get("method").and_then(Value::as_str) == Some("agent/output"))
            .filter_map(|frame| frame.get("params").cloned())
            .collect()
    }

    #[tokio::test]
    async fn non_recoverable_error_reports_nonzero_exit_code() {
        let (tx, rx) = mpsc::channel::<SessionEvent>(8);
        tx.send(SessionEvent::Error { message: "fatal provider failure".to_string(), recoverable: false })
            .await
            .unwrap();
        let backend = Arc::new(TestBackend::with_run(test_run("inner-1", rx)));

        let response = handle_agent_run(
            Some(json!(1)),
            Some(json!({ "prompt": "hi", "cwd": "/tmp" })),
            &provider_info(),
            backend,
            test_stdout(),
            None,
            fresh_routes(),
        )
        .await;

        let result = response.result.expect("agent/run must return a result payload");
        assert_eq!(
            result.get("exit_code").and_then(Value::as_i64),
            Some(1),
            "a non-recoverable error must not surface as exit_code 0 (success)"
        );
        assert_eq!(result["errors"][0], "fatal provider failure", "errors array must be preserved");
    }

    #[tokio::test]
    async fn finished_exit_code_still_wins_over_default() {
        let (tx, rx) = mpsc::channel::<SessionEvent>(8);
        tx.send(SessionEvent::Finished { exit_code: Some(0) }).await.unwrap();
        let backend = Arc::new(TestBackend::with_run(test_run("inner-1", rx)));

        let response = handle_agent_run(
            Some(json!(1)),
            Some(json!({ "prompt": "hi", "cwd": "/tmp" })),
            &provider_info(),
            backend,
            test_stdout(),
            None,
            fresh_routes(),
        )
        .await;

        let result = response.result.expect("agent/run must return a result payload");
        assert_eq!(result.get("exit_code").and_then(Value::as_i64), Some(0));
    }

    #[tokio::test]
    async fn final_text_replaces_accumulated_deltas_in_result_output() {
        let (tx, rx) = mpsc::channel::<SessionEvent>(8);
        tx.send(SessionEvent::TextDelta { text: "Hello ".to_string() }).await.unwrap();
        tx.send(SessionEvent::TextDelta { text: "world".to_string() }).await.unwrap();
        tx.send(SessionEvent::FinalText { text: "Hello world!".to_string() }).await.unwrap();
        tx.send(SessionEvent::Finished { exit_code: Some(0) }).await.unwrap();
        let backend = Arc::new(TestBackend::with_run(test_run("inner-1", rx)));
        let sink = capture_sink();

        let response = handle_agent_run(
            Some(json!(1)),
            Some(json!({ "prompt": "hi", "cwd": "/tmp" })),
            &provider_info(),
            backend,
            sink.clone(),
            None,
            fresh_routes(),
        )
        .await;

        let result = response.result.expect("agent/run must return a result payload");
        assert_eq!(
            result.get("output").and_then(Value::as_str),
            Some("Hello world!"),
            "final text is canonical and must replace accumulated deltas, not duplicate them"
        );

        let frames = captured_frames(&sink).await;
        let outputs = output_frames(&frames);
        assert_eq!(outputs.len(), 3, "two delta frames plus one final frame must be streamed");
        assert_eq!(outputs[0].get("final"), Some(&json!(false)), "delta frames must carry final=false");
        assert_eq!(outputs[1].get("final"), Some(&json!(false)), "delta frames must carry final=false");
        assert_eq!(outputs[2].get("final"), Some(&json!(true)), "the terminal output frame must carry final=true");
        assert_eq!(outputs[2].get("text").and_then(Value::as_str), Some("Hello world!"));
    }

    #[tokio::test]
    async fn delta_only_runs_keep_accumulated_output() {
        let (tx, rx) = mpsc::channel::<SessionEvent>(8);
        tx.send(SessionEvent::TextDelta { text: "Hello ".to_string() }).await.unwrap();
        tx.send(SessionEvent::TextDelta { text: "world".to_string() }).await.unwrap();
        tx.send(SessionEvent::Finished { exit_code: Some(0) }).await.unwrap();
        let backend = Arc::new(TestBackend::with_run(test_run("inner-1", rx)));

        let response = handle_agent_run(
            Some(json!(1)),
            Some(json!({ "prompt": "hi", "cwd": "/tmp" })),
            &provider_info(),
            backend,
            test_stdout(),
            None,
            fresh_routes(),
        )
        .await;

        let result = response.result.expect("agent/run must return a result payload");
        assert_eq!(
            result.get("output").and_then(Value::as_str),
            Some("Hello world"),
            "without a final text event, the accumulated deltas remain the output"
        );
    }

    #[tokio::test]
    async fn started_event_session_id_upgrades_pending_route_and_reaches_result() {
        let (tx, rx) = mpsc::channel::<SessionEvent>(8);
        let backend = Arc::new(TestBackend::with_run(test_run_without_id(rx)));
        let routes = fresh_routes();
        let info = provider_info();
        let sink = capture_sink();

        let run_backend = backend.clone();
        let run_routes = routes.clone();
        let run_info = info.clone();
        let run_sink = sink.clone();
        let run_task = tokio::spawn(async move {
            handle_agent_run(
                Some(json!(1)),
                Some(json!({ "prompt": "hi", "cwd": "/tmp", "control_session_id": "ctrl-1" })),
                &run_info,
                run_backend,
                run_sink,
                None,
                run_routes,
            )
            .await
        });

        for _ in 0..200 {
            if matches!(routes.lock().await.get("ctrl-1"), Some(CancelRoute::Pending)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            matches!(routes.lock().await.get("ctrl-1"), Some(CancelRoute::Pending)),
            "a run whose backend returns no session id must keep the Pending marker"
        );

        tx.send(SessionEvent::Started {
            backend: "test".to_string(),
            session_id: Some("late-id".to_string()),
            pid: None,
        })
        .await
        .unwrap();
        for _ in 0..200 {
            if matches!(routes.lock().await.get("ctrl-1"), Some(CancelRoute::Ready(_))) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            matches!(routes.lock().await.get("ctrl-1"), Some(CancelRoute::Ready(id)) if id == "late-id"),
            "the Started session id must upgrade the cancel route to Ready mid-run"
        );

        let response = handle_agent_cancel(
            Some(json!(2)),
            Some(json!({ "session_id": "ctrl-1" })),
            backend.clone(),
            &info,
            routes.clone(),
        )
        .await;
        assert!(response.error.is_none(), "cancel must succeed: {:?}", response.error);
        assert_eq!(
            backend.cancelled(),
            vec!["late-id".to_string()],
            "a mid-run cancel must translate to the Started-provided session id"
        );

        tx.send(SessionEvent::Started {
            backend: "test".to_string(),
            session_id: Some("second-id".to_string()),
            pid: None,
        })
        .await
        .unwrap();
        tx.send(SessionEvent::TextDelta { text: "hi".to_string() }).await.unwrap();
        tx.send(SessionEvent::Finished { exit_code: Some(0) }).await.unwrap();

        let response = run_task.await.expect("run task must finish");
        let result = response.result.expect("agent/run must return a result payload");
        assert_eq!(
            result.get("session_id").and_then(Value::as_str),
            Some("late-id"),
            "the first Started session id wins; later Started events must not override it"
        );

        let frames = captured_frames(&sink).await;
        let outputs = output_frames(&frames);
        assert_eq!(
            outputs[0].get("session_id").and_then(Value::as_str),
            Some("late-id"),
            "notifications after Started must carry the captured session id"
        );
    }

    #[tokio::test]
    async fn run_session_id_wins_over_started_event_id() {
        let (tx, rx) = mpsc::channel::<SessionEvent>(8);
        tx.send(SessionEvent::Started {
            backend: "test".to_string(),
            session_id: Some("other".to_string()),
            pid: None,
        })
        .await
        .unwrap();
        tx.send(SessionEvent::Finished { exit_code: Some(0) }).await.unwrap();
        let backend = Arc::new(TestBackend::with_run(test_run("inner-1", rx)));

        let response = handle_agent_run(
            Some(json!(1)),
            Some(json!({ "prompt": "hi", "cwd": "/tmp" })),
            &provider_info(),
            backend,
            test_stdout(),
            None,
            fresh_routes(),
        )
        .await;

        let result = response.result.expect("agent/run must return a result payload");
        assert_eq!(
            result.get("session_id").and_then(Value::as_str),
            Some("inner-1"),
            "a session id returned by start() must not be overridden by a Started event"
        );
    }

    #[tokio::test]
    async fn agent_cancel_translates_control_id_to_backend_session_id() {
        let (tx, rx) = mpsc::channel::<SessionEvent>(8);
        let backend = Arc::new(TestBackend::with_run(test_run("inner-real", rx)));
        let routes = fresh_routes();
        let info = provider_info();

        let run_backend = backend.clone();
        let run_routes = routes.clone();
        let run_info = info.clone();
        let run_task = tokio::spawn(async move {
            handle_agent_run(
                Some(json!(1)),
                Some(json!({ "prompt": "hi", "cwd": "/tmp", "control_session_id": "ctrl-1" })),
                &run_info,
                run_backend,
                test_stdout(),
                None,
                run_routes,
            )
            .await
        });

        for _ in 0..200 {
            if routes.lock().await.contains_key("ctrl-1") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            routes.lock().await.contains_key("ctrl-1"),
            "control id must be registered as soon as the backend run starts"
        );

        let response = handle_agent_cancel(
            Some(json!(2)),
            Some(json!({ "session_id": "ctrl-1" })),
            backend.clone(),
            &info,
            routes.clone(),
        )
        .await;
        assert!(response.error.is_none(), "cancel must succeed: {:?}", response.error);
        assert_eq!(
            backend.cancelled(),
            vec!["inner-real".to_string()],
            "cancel must reach the backend with ITS OWN session id, not the host control id"
        );
        let result = response.result.expect("cancel result");
        assert_eq!(result.get("session_id").and_then(Value::as_str), Some("ctrl-1"), "response echoes the wire id");

        tx.send(SessionEvent::Finished { exit_code: Some(0) }).await.unwrap();
        run_task.await.expect("run task must finish");
        assert!(!routes.lock().await.contains_key("ctrl-1"), "route entry must be cleaned up after the run completes");
    }

    #[tokio::test]
    async fn agent_cancel_falls_back_to_raw_session_id_when_unknown() {
        let (_tx, rx) = mpsc::channel::<SessionEvent>(8);
        let backend = Arc::new(TestBackend::with_run(test_run("inner-1", rx)));

        let response = handle_agent_cancel(
            Some(json!(1)),
            Some(json!({ "session_id": "provider-native-id" })),
            backend.clone(),
            &provider_info(),
            fresh_routes(),
        )
        .await;
        assert!(response.error.is_none(), "cancel must succeed: {:?}", response.error);
        assert_eq!(
            backend.cancelled(),
            vec!["provider-native-id".to_string()],
            "unknown ids must pass through unchanged (back-compat with hosts that send the provider id)"
        );
    }

    #[tokio::test]
    async fn agent_cancel_waits_out_pending_route_while_backend_starts() {
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let (tx, rx) = mpsc::channel::<SessionEvent>(8);
        let backend = Arc::new(TestBackend::with_gated_start(test_run("inner-real", rx), gate_rx));
        let routes = fresh_routes();
        let info = provider_info();

        let run_backend = backend.clone();
        let run_routes = routes.clone();
        let run_info = info.clone();
        let run_task = tokio::spawn(async move {
            handle_agent_run(
                Some(json!(1)),
                Some(json!({ "prompt": "hi", "cwd": "/tmp", "control_session_id": "ctrl-1" })),
                &run_info,
                run_backend,
                test_stdout(),
                None,
                run_routes,
            )
            .await
        });

        for _ in 0..200 {
            if matches!(routes.lock().await.get("ctrl-1"), Some(CancelRoute::Pending)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            matches!(routes.lock().await.get("ctrl-1"), Some(CancelRoute::Pending)),
            "a pending marker must be parked before the backend produces its session id"
        );

        // Fire the cancel while the backend is still starting, then release
        // the start gate: the cancel must wait for the real id instead of
        // falling through with the control id.
        let cancel_backend = backend.clone();
        let cancel_routes = routes.clone();
        let cancel_info = info.clone();
        let cancel_task = tokio::spawn(async move {
            handle_agent_cancel(
                Some(json!(2)),
                Some(json!({ "session_id": "ctrl-1" })),
                cancel_backend,
                &cancel_info,
                cancel_routes,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        gate_tx.send(()).expect("release start gate");

        let response = cancel_task.await.expect("cancel task must finish");
        assert!(response.error.is_none(), "cancel must succeed: {:?}", response.error);
        assert_eq!(
            backend.cancelled(),
            vec!["inner-real".to_string()],
            "a cancel racing backend start must still reach the backend with its own session id"
        );

        tx.send(SessionEvent::Finished { exit_code: Some(0) }).await.unwrap();
        run_task.await.expect("run task must finish");
    }
}
