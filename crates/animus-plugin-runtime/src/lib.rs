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
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    control_session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgentCancelParams {
    session_id: String,
}

/// Map of host-minted `control_session_id` -> backend-issued session id.
/// `agent/run` parks a [`CancelRoute::Pending`] marker before the backend
/// starts and upgrades it to [`CancelRoute::Ready`] as soon as the backend
/// returns its own session id; `agent/cancel` translates an incoming control
/// id to the backend's real id before calling [`ProviderBackend::cancel`],
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

async fn handle_request<P: ProviderBackend>(
    request: RpcRequest,
    info: ProviderInfo,
    backend: Arc<P>,
    stdout: Arc<Mutex<tokio::io::Stdout>>,
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

    let session_id = run.session_id.clone();
    if let Some(control) = control_session_id.as_deref() {
        let mut routes = cancel_routes.lock().await;
        match session_id.as_deref() {
            Some(inner) => {
                routes.insert(control.to_string(), CancelRoute::Ready(inner.to_string()));
            }
            None => {
                routes.remove(control);
            }
        }
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
    let mut extras = serde_json::Map::new();
    if let Some(system_prompt) = params.system_prompt {
        extras.insert("system_prompt".to_string(), Value::String(system_prompt));
    }
    if let Some(profile) = params.claude_profile {
        extras.insert("claude_profile".to_string(), Value::String(profile));
    }
    if let Some(mcp) = params.mcp_servers {
        extras.insert("mcp_servers".to_string(), mcp);
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

    fn fresh_routes() -> CancelRoutes {
        Arc::new(Mutex::new(HashMap::new()))
    }

    fn test_stdout() -> Arc<Mutex<tokio::io::Stdout>> {
        Arc::new(Mutex::new(tokio::io::stdout()))
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
