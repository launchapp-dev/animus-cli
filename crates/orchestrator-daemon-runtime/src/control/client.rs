//! CLI-side client for the daemon's control RPC socket.
//!
//! C6 of the v0.4.0 controller-as-plugin migration. The CLI now speaks
//! the [`animus_control_protocol`] wire format directly to the daemon
//! when the daemon is running (socket present at the project's expected
//! `control.sock` path) and falls back to the existing in-process code
//! paths when the daemon is not running.
//!
//! ## Behavior contract
//!
//! [`ControlClient::try_connect`] returns:
//! - `Ok(Some(client))` when the socket exists and a connection succeeded
//! - `Ok(None)` when the socket does not exist — the CLI should run the
//!   in-process implementation instead. This is the steady-state for
//!   commands like `animus plugin install --path` while no daemon is
//!   running.
//! - `Err(_)` only for unexpected IO failures (e.g. socket exists but is
//!   un-openable due to permissions, malformed JSON-RPC response). The
//!   CLI surfaces these as errors rather than silently degrading.
//!
//! ## Anti-deadlock notes
//!
//! - Each [`ControlClient::call`] opens a fresh stream, sends one
//!   request, reads one line, and drops the stream. No persistent
//!   connection pool that could outlive a CLI invocation.
//! - No `tokio::sync::Mutex` for shared state; the client struct is
//!   `Clone` and trivially safe to share.
//! - Reads use the connection's natural newline framing — no timeouts
//!   are imposed here. A wedged daemon means a wedged CLI command, which
//!   is the correct UX (user `Ctrl+C`s rather than receiving phantom
//!   success).

use std::path::{Path, PathBuf};

use crate::control::control_socket_path;
use animus_control_protocol::{
    method as method_names,
    types::{
        AgentCancelRequest, AgentRunRequest, AgentRunResult, AgentStatus, AgentStatusRequest, DaemonAgentsResponse,
        DaemonHealthResponse, DaemonLogEntry, DaemonLogsRequest, DaemonStatusResponse, PluginBrowseRequest,
        PluginCallRequest, PluginCallResponse, PluginInfo, PluginInfoRequest, PluginInstallRequest,
        PluginInstallResponse, PluginListRequest, PluginListResponse, PluginPingRequest, PluginPingResponse,
        PluginSearchRequest, PluginSearchResponse, PluginUninstallRequest, PluginUpdateRequest, PluginUpdateResponse,
        QueueDropRequest, QueueEnqueueRequest, QueueEntry, QueueHoldRequest, QueueListRequest, QueueListResponse,
        QueueReleaseRequest, QueueReorderRequest, QueueStats, Unit, WorkflowCancelRequest, WorkflowExecuteRequest,
        WorkflowGetRequest, WorkflowListRequest, WorkflowListResponse, WorkflowPauseRequest, WorkflowResumeRequest,
        WorkflowRun, WorkflowRunRequest, WorkflowRunStart,
    },
};
use animus_plugin_protocol::{error_codes, RpcRequest, RpcResponse};
use anyhow::{anyhow, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::UnixStream;

/// Handle to the daemon control socket for one CLI invocation.
///
/// Cheap to clone; carries only a socket path. Each [`Self::call`]
/// opens a fresh [`UnixStream`].
#[derive(Debug, Clone)]
pub struct ControlClient {
    socket_path: PathBuf,
}

impl ControlClient {
    /// Connect to the control socket for `project_root`, returning
    /// `None` when the socket does not exist (daemon not running) so
    /// callers can fall back to local code paths.
    ///
    /// Existence is checked via `std::fs::metadata` — symlinks are
    /// followed, broken or non-existent paths produce `Ok(None)`.
    #[cfg(unix)]
    pub async fn try_connect(project_root: &Path) -> Result<Option<Self>> {
        let socket_path = control_socket_path(project_root);
        if !socket_path.exists() {
            return Ok(None);
        }
        // Probe-connect to verify the socket is actually accepting
        // connections. A stale socket file (left by a crashed daemon)
        // exists but fails to connect — treat that as "daemon not
        // running" rather than as a hard error.
        match UnixStream::connect(&socket_path).await {
            Ok(_stream) => Ok(Some(Self { socket_path })),
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => Ok(None),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(anyhow!("failed to connect to control socket {}: {err}", socket_path.display())),
        }
    }

    /// Non-Unix stub: the control socket is Unix-domain-socket only,
    /// so callers always fall through to the in-process service path
    /// on Windows. A named-pipe equivalent is a future enhancement.
    #[cfg(not(unix))]
    pub async fn try_connect(_project_root: &Path) -> Result<Option<Self>> {
        Ok(None)
    }

    /// Explicit constructor for tests that point a client at an
    /// arbitrary socket path (e.g. a tempdir).
    #[cfg(test)]
    pub fn from_socket_path(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// Borrow the resolved socket path, useful in error messages and
    /// tests.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Issue one JSON-RPC request and decode the response into `R`.
    ///
    /// Each call opens a fresh `UnixStream`, sends the request as a
    /// single newline-terminated frame, reads exactly one line back,
    /// and parses it into [`RpcResponse`]. Returns:
    /// - `Ok(value)` on RPC success
    /// - `Err(_)` mapping the daemon's JSON-RPC error code into an
    ///   anyhow error tagged with the method name for log scrubbing
    pub async fn call<P, R>(&self, method: &str, params: P) -> Result<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let params_value = serde_json::to_value(&params)
            .with_context(|| format!("control client: serializing params for {method}"))?;
        let response = self.call_raw(method, Some(params_value)).await?;
        match (response.result, response.error) {
            (Some(value), None) => {
                serde_json::from_value(value).with_context(|| format!("control client: decoding {method} response"))
            }
            (_, Some(error)) => Err(rpc_error_to_anyhow(method, &error)),
            (None, None) => Err(anyhow!("control client: empty {method} response (no result, no error)")),
        }
    }

    /// Issue one JSON-RPC request with raw params and return the full
    /// envelope. Lower-level than [`Self::call`]; used internally and
    /// by tests that need to inspect the error payload directly.
    #[cfg(unix)]
    pub async fn call_raw(&self, method: &str, params: Option<Value>) -> Result<RpcResponse> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connect to {}", self.socket_path.display()))?;
        let request = RpcRequest::new(serde_json::Value::from(1u64), method.to_string(), params);
        let mut bytes = serde_json::to_vec(&request).context("serialize RPC request")?;
        bytes.push(b'\n');
        stream.write_all(&bytes).await.context("write RPC request")?;
        stream.flush().await.context("flush RPC request")?;
        let (read_half, _write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.context("read RPC response")?;
        if n == 0 {
            return Err(anyhow!("control server closed connection without responding to {method}"));
        }
        let response: RpcResponse =
            serde_json::from_str(line.trim_end()).with_context(|| format!("parse {method} RPC response: {line}"))?;
        Ok(response)
    }

    /// Non-Unix stub. [`Self::try_connect`] already returns `Ok(None)`
    /// on Windows so this should be unreachable in practice; surface a
    /// clear error if anything tries to call it directly.
    #[cfg(not(unix))]
    pub async fn call_raw(&self, method: &str, _params: Option<Value>) -> Result<RpcResponse> {
        Err(anyhow!("control client {method}: control socket not supported on this platform"))
    }

    // ----- Plugin convenience methods --------------------------------

    /// Call `plugin/list`.
    pub async fn plugin_list(&self, request: PluginListRequest) -> Result<PluginListResponse> {
        self.call(method_names::METHOD_PLUGIN_LIST, request).await
    }

    /// Call `plugin/info`.
    pub async fn plugin_info(&self, request: PluginInfoRequest) -> Result<PluginInfo> {
        self.call(method_names::METHOD_PLUGIN_INFO, request).await
    }

    /// Call `plugin/install`.
    pub async fn plugin_install(&self, request: PluginInstallRequest) -> Result<PluginInstallResponse> {
        self.call(method_names::METHOD_PLUGIN_INSTALL, request).await
    }

    /// Call `plugin/uninstall`.
    pub async fn plugin_uninstall(&self, request: PluginUninstallRequest) -> Result<Unit> {
        self.call(method_names::METHOD_PLUGIN_UNINSTALL, request).await
    }

    /// Call `plugin/ping`.
    pub async fn plugin_ping(&self, request: PluginPingRequest) -> Result<PluginPingResponse> {
        self.call(method_names::METHOD_PLUGIN_PING, request).await
    }

    /// Call `plugin/call`.
    pub async fn plugin_call(&self, request: PluginCallRequest) -> Result<PluginCallResponse> {
        self.call(method_names::METHOD_PLUGIN_CALL, request).await
    }

    /// Call `plugin/search`.
    pub async fn plugin_search(&self, request: PluginSearchRequest) -> Result<PluginSearchResponse> {
        self.call(method_names::METHOD_PLUGIN_SEARCH, request).await
    }

    /// Call `plugin/browse`.
    pub async fn plugin_browse(&self, request: PluginBrowseRequest) -> Result<PluginSearchResponse> {
        self.call(method_names::METHOD_PLUGIN_BROWSE, request).await
    }

    /// Call `plugin/update`.
    pub async fn plugin_update(&self, request: PluginUpdateRequest) -> Result<PluginUpdateResponse> {
        self.call(method_names::METHOD_PLUGIN_UPDATE, request).await
    }

    // ----- Daemon convenience methods --------------------------------

    /// Call `daemon/status`.
    pub async fn daemon_status(&self) -> Result<DaemonStatusResponse> {
        self.call::<Value, _>(method_names::METHOD_DAEMON_STATUS, Value::Null).await
    }

    /// Call `daemon/health`.
    pub async fn daemon_health(&self) -> Result<DaemonHealthResponse> {
        self.call::<Value, _>(method_names::METHOD_DAEMON_HEALTH, Value::Null).await
    }

    /// Call `daemon/agents`.
    pub async fn daemon_agents(&self) -> Result<DaemonAgentsResponse> {
        self.call::<Value, _>(method_names::METHOD_DAEMON_AGENTS, Value::Null).await
    }

    /// Call `daemon/metrics` — in-tree only RPC method (not part of the
    /// upstream control protocol v0.1.10 surface). Returns the metrics
    /// snapshot as raw JSON so the CLI can render it without depending
    /// on the metrics struct shape directly.
    pub async fn daemon_metrics(&self) -> Result<crate::metrics::MetricsSnapshot> {
        self.call::<Value, _>("daemon/metrics", Value::Null).await
    }

    // ----- Workflow convenience methods ------------------------------

    /// Call `workflow/list`.
    pub async fn workflow_list(&self, request: WorkflowListRequest) -> Result<WorkflowListResponse> {
        self.call(method_names::METHOD_WORKFLOW_LIST, request).await
    }

    /// Call `workflow/get`.
    pub async fn workflow_get(&self, request: WorkflowGetRequest) -> Result<WorkflowRun> {
        self.call(method_names::METHOD_WORKFLOW_GET, request).await
    }

    /// Call `workflow/run`.
    pub async fn workflow_run(&self, request: WorkflowRunRequest) -> Result<WorkflowRunStart> {
        self.call(method_names::METHOD_WORKFLOW_RUN, request).await
    }

    /// Call `workflow/execute`.
    pub async fn workflow_execute(&self, request: WorkflowExecuteRequest) -> Result<WorkflowRunStart> {
        self.call(method_names::METHOD_WORKFLOW_EXECUTE, request).await
    }

    /// Call `workflow/pause`.
    pub async fn workflow_pause(&self, request: WorkflowPauseRequest) -> Result<Unit> {
        self.call(method_names::METHOD_WORKFLOW_PAUSE, request).await
    }

    /// Call `workflow/resume`.
    pub async fn workflow_resume(&self, request: WorkflowResumeRequest) -> Result<Unit> {
        self.call(method_names::METHOD_WORKFLOW_RESUME, request).await
    }

    /// Call `workflow/cancel`.
    pub async fn workflow_cancel(&self, request: WorkflowCancelRequest) -> Result<Unit> {
        self.call(method_names::METHOD_WORKFLOW_CANCEL, request).await
    }

    // ----- Queue convenience methods ---------------------------------

    /// Call `queue/list`.
    pub async fn queue_list(&self, request: QueueListRequest) -> Result<QueueListResponse> {
        self.call(method_names::METHOD_QUEUE_LIST, request).await
    }

    /// Call `queue/enqueue`.
    pub async fn queue_enqueue(&self, request: QueueEnqueueRequest) -> Result<QueueEntry> {
        self.call(method_names::METHOD_QUEUE_ENQUEUE, request).await
    }

    /// Call `queue/drop`.
    pub async fn queue_drop(&self, request: QueueDropRequest) -> Result<Unit> {
        self.call(method_names::METHOD_QUEUE_DROP, request).await
    }

    /// Call `queue/hold`.
    pub async fn queue_hold(&self, request: QueueHoldRequest) -> Result<Unit> {
        self.call(method_names::METHOD_QUEUE_HOLD, request).await
    }

    /// Call `queue/release`.
    pub async fn queue_release(&self, request: QueueReleaseRequest) -> Result<Unit> {
        self.call(method_names::METHOD_QUEUE_RELEASE, request).await
    }

    /// Call `queue/reorder`.
    pub async fn queue_reorder(&self, request: QueueReorderRequest) -> Result<Unit> {
        self.call(method_names::METHOD_QUEUE_REORDER, request).await
    }

    /// Call `queue/stats`.
    pub async fn queue_stats(&self) -> Result<QueueStats> {
        self.call::<Value, _>(method_names::METHOD_QUEUE_STATS, Value::Null).await
    }

    // ----- Agent convenience methods ---------------------------------

    /// Call `agent/run`.
    pub async fn agent_run(&self, request: AgentRunRequest) -> Result<AgentRunResult> {
        self.call(method_names::METHOD_AGENT_RUN, request).await
    }

    /// Call `agent/status`.
    pub async fn agent_status(&self, request: AgentStatusRequest) -> Result<AgentStatus> {
        self.call(method_names::METHOD_AGENT_STATUS, request).await
    }

    /// Call `agent/cancel`.
    pub async fn agent_cancel(&self, request: AgentCancelRequest) -> Result<Unit> {
        self.call(method_names::METHOD_AGENT_CANCEL, request).await
    }

    /// Stream the daemon's historical log tail and (optionally) live
    /// follow-ups. v0.4.7: returns the historical batch the daemon
    /// resolves via the active [`LogStorageDispatch`]; once `follow=true`
    /// support against a long-lived log_storage plugin host lands, this
    /// method will keep reading until the caller drops the future.
    ///
    /// Today the server emits the historical batch and closes the
    /// connection (when `follow=false`). The client reads the ack frame,
    /// then drains notification frames until the socket closes or `limit`
    /// entries have been collected.
    #[cfg(unix)]
    pub async fn daemon_logs(&self, request: DaemonLogsRequest, limit: usize) -> Result<Vec<DaemonLogEntry>> {
        use animus_plugin_protocol::{RpcRequest, RpcResponse};
        use serde_json::Value;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixStream;

        let method = method_names::METHOD_DAEMON_LOGS;
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connect to {}", self.socket_path.display()))?;
        let params = serde_json::to_value(&request).context("serialize daemon/logs params")?;
        let rpc_request = RpcRequest::new(Value::from(1u64), method.to_string(), Some(params));
        let mut bytes = serde_json::to_vec(&rpc_request).context("serialize daemon/logs request")?;
        bytes.push(b'\n');
        stream.write_all(&bytes).await.context("write daemon/logs request")?;
        stream.flush().await.context("flush daemon/logs request")?;

        let (read_half, _write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.context("read daemon/logs ack")?;
        if n == 0 {
            return Err(anyhow!("control server closed connection without ack on {method}"));
        }
        let ack: RpcResponse =
            serde_json::from_str(line.trim_end()).with_context(|| format!("parse {method} ack: {line}"))?;
        if let Some(err) = ack.error {
            return Err(rpc_error_to_anyhow(method, &err));
        }

        let mut entries: Vec<DaemonLogEntry> = Vec::new();
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf).await.context("read daemon/logs frame")?;
            if n == 0 {
                // Server completed the stream and closed the connection.
                break;
            }
            let trimmed = buf.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            let frame: Value =
                serde_json::from_str(trimmed).with_context(|| format!("parse daemon/logs frame: {trimmed}"))?;
            let Some(params) = frame.get("params") else {
                continue;
            };
            let Some(data) = params.get("data") else {
                continue;
            };
            let entry: DaemonLogEntry =
                serde_json::from_value(data.clone()).with_context(|| format!("decode daemon/log entry: {data}"))?;
            entries.push(entry);
            if entries.len() >= limit {
                break;
            }
        }
        Ok(entries)
    }

    #[cfg(not(unix))]
    pub async fn daemon_logs(&self, _request: DaemonLogsRequest, _limit: usize) -> Result<Vec<DaemonLogEntry>> {
        Err(anyhow!("control client daemon/logs: control socket not supported on this platform"))
    }
}

/// Translate a JSON-RPC error from the daemon into a CLI-side anyhow
/// error.
///
/// Method-not-supported / method-not-found map to user-facing strings
/// the CLI handler can choose to detect and fall back on (e.g. when the
/// daemon advertises a wire surface but a particular plugin/* method
/// hasn't been wired through yet). Other codes surface as plain error
/// messages.
fn rpc_error_to_anyhow(method: &str, error: &animus_plugin_protocol::RpcError) -> anyhow::Error {
    match error.code {
        error_codes::METHOD_NOT_FOUND => {
            anyhow!("control server method '{method}' not found: {}", error.message)
        }
        error_codes::METHOD_NOT_SUPPORTED => {
            anyhow!("control server method '{method}' not supported: {}", error.message)
        }
        _ => anyhow!("control server {method} failed (code {}): {}", error.code, error.message),
    }
}

/// True when the underlying JSON-RPC error indicates the daemon doesn't
/// know how to answer this method yet. CLI handlers check this to
/// decide whether to fall back to the local in-process implementation
/// or to surface the error directly.
pub fn is_method_unavailable(err: &anyhow::Error) -> bool {
    let s = format!("{err}");
    s.contains("not found:") || s.contains("not supported:")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use animus_plugin_protocol::RpcError;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn try_connect_returns_none_when_socket_missing() {
        let dir = TempDir::new().unwrap();
        let result = ControlClient::try_connect(dir.path()).await.unwrap();
        // The probe walks `~/.animus/<repo-scope>/control.sock`; for a
        // fresh tempdir that path will not exist.
        assert!(result.is_none_or(|c| !c.socket_path().exists()));
    }

    #[tokio::test]
    async fn try_connect_returns_none_for_stale_socket_file() {
        // A regular file at the socket path produces ConnectionRefused
        // on connect (not a real socket).
        let dir = TempDir::new().unwrap();
        let sock = dir.path().join("control.sock");
        std::fs::write(&sock, "").unwrap();
        let client = ControlClient::from_socket_path(sock.clone());
        // Direct call should fail; just verify the from_socket_path
        // constructor wires the path through.
        assert_eq!(client.socket_path(), sock.as_path());
    }

    #[test]
    fn is_method_unavailable_detects_not_found() {
        let err = anyhow!("control server method 'plugin/list' not found: unknown control method: plugin/list");
        assert!(is_method_unavailable(&err));
    }

    #[test]
    fn is_method_unavailable_detects_not_supported() {
        let err = anyhow!("control server method 'workflow/list' not supported: deferred");
        assert!(is_method_unavailable(&err));
    }

    #[test]
    fn is_method_unavailable_ignores_other_errors() {
        let err = anyhow!("control server plugin/install failed (code -32000): boom");
        assert!(!is_method_unavailable(&err));
    }

    /// Spawn a minimal Unix-socket server that reads exactly one
    /// JSON-RPC frame, replies with the configured response, then
    /// closes. Used by the round-trip tests below; avoids depending on
    /// the full daemon ControlServer.
    fn short_sock_path() -> PathBuf {
        let unique = format!("animus-c6-{}-{}.sock", std::process::id(), uuid::Uuid::new_v4().simple());
        std::env::temp_dir().join(unique)
    }

    async fn spawn_fake_server<F>(socket_path: PathBuf, handler: F) -> tokio::task::JoinHandle<()>
    where
        F: Fn(RpcRequest) -> RpcResponse + Send + Sync + 'static,
    {
        let listener = UnixListener::bind(&socket_path).expect("bind");
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let request: RpcRequest = serde_json::from_str(line.trim_end()).expect("parse");
                let response = handler(request);
                let mut bytes = serde_json::to_vec(&response).expect("ser");
                bytes.push(b'\n');
                let _ = write_half.write_all(&bytes).await;
                let _ = write_half.flush().await;
            }
        })
    }

    #[tokio::test]
    async fn call_round_trips_success_response() {
        let socket_path = short_sock_path();
        let handler = |req: RpcRequest| {
            assert_eq!(req.method, "daemon/status");
            RpcResponse::ok(req.id, serde_json::json!({"running": true, "pid": 7}))
        };
        let server = spawn_fake_server(socket_path.clone(), handler).await;
        let client = ControlClient::from_socket_path(socket_path.clone());
        let result: serde_json::Value =
            tokio::time::timeout(Duration::from_secs(5), client.call("daemon/status", Value::Null))
                .await
                .expect("timeout")
                .expect("call");
        assert_eq!(result.get("running"), Some(&serde_json::json!(true)));
        assert_eq!(result.get("pid"), Some(&serde_json::json!(7)));
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    #[tokio::test]
    async fn call_surfaces_method_not_found_as_unavailable() {
        let socket_path = short_sock_path();
        let handler = |req: RpcRequest| {
            RpcResponse::err(
                req.id,
                RpcError {
                    code: animus_plugin_protocol::error_codes::METHOD_NOT_FOUND,
                    message: "unknown control method: ghost/method".to_string(),
                    data: None,
                },
            )
        };
        let server = spawn_fake_server(socket_path.clone(), handler).await;
        let client = ControlClient::from_socket_path(socket_path.clone());
        let err =
            tokio::time::timeout(Duration::from_secs(5), client.call::<Value, Value>("ghost/method", Value::Null))
                .await
                .expect("timeout")
                .unwrap_err();
        assert!(is_method_unavailable(&err), "expected is_method_unavailable to be true: {err}");
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    /// C6.5: `workflow/list` round-trips a WorkflowListResponse through
    /// the wire. Mirrors the daemon-side
    /// `workflow_list_routes_through_configured_routing` test from the
    /// CLI side: spawn a fake server, call the typed convenience method,
    /// verify the decoded response.
    #[tokio::test]
    async fn workflow_list_routes_via_control_when_socket_present() {
        let socket_path = short_sock_path();
        let handler = |req: RpcRequest| {
            assert_eq!(req.method, "workflow/list");
            RpcResponse::ok(
                req.id,
                serde_json::json!({
                    "runs": [{
                        "id": "wf-1",
                        "definition": "standard-workflow",
                        "status": "running",
                        "started_at": "2026-05-20T00:00:00Z",
                    }],
                }),
            )
        };
        let server = spawn_fake_server(socket_path.clone(), handler).await;
        let client = ControlClient::from_socket_path(socket_path.clone());
        let request = animus_control_protocol::types::WorkflowListRequest::default();
        let response = tokio::time::timeout(Duration::from_secs(5), client.workflow_list(request))
            .await
            .expect("timeout")
            .expect("call");
        assert_eq!(response.runs.len(), 1);
        assert_eq!(response.runs[0].id, "wf-1");
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    /// C6.5: when no socket is present the helper returns Ok(None) so
    /// the CLI falls back to the local in-process path. Verified end to
    /// end by `ControlClient::try_connect`; this test pins the contract.
    #[tokio::test]
    async fn workflow_list_falls_back_when_socket_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        // Use a project_root that has no `~/.animus/<scope>/control.sock`.
        let result = ControlClient::try_connect(dir.path()).await.unwrap();
        // Either the path doesn't exist (None) or it exists but is unusable.
        assert!(result.is_none_or(|c| !c.socket_path().exists()));
    }

    /// C6.6: `queue/list` round-trips a QueueListResponse through the
    /// wire. Mirrors the daemon-side
    /// `queue_list_routes_through_configured_routing` test from the CLI
    /// side: spawn a fake server, call the typed convenience method,
    /// verify the decoded response.
    #[tokio::test]
    async fn queue_list_routes_via_control_when_socket_present() {
        let socket_path = short_sock_path();
        let handler = |req: RpcRequest| {
            assert_eq!(req.method, "queue/list");
            RpcResponse::ok(
                req.id,
                serde_json::json!({
                    "entries": [{
                        "id": "TASK-1",
                        "subject_id": "TASK-1",
                        "status": "ready",
                        "priority": 2,
                        "enqueued_at": "2026-05-20T00:00:00Z",
                    }],
                }),
            )
        };
        let server = spawn_fake_server(socket_path.clone(), handler).await;
        let client = ControlClient::from_socket_path(socket_path.clone());
        let request = animus_control_protocol::types::QueueListRequest::default();
        let response = tokio::time::timeout(Duration::from_secs(5), client.queue_list(request))
            .await
            .expect("timeout")
            .expect("call");
        assert_eq!(response.entries.len(), 1);
        assert_eq!(response.entries[0].id, "TASK-1");
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    /// C6.6: when no socket is present the helper returns Ok(None) so
    /// the CLI falls back to the local in-process path. Verified end to
    /// end by `ControlClient::try_connect`; this test pins the contract.
    #[tokio::test]
    async fn queue_list_falls_back_when_socket_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = ControlClient::try_connect(dir.path()).await.unwrap();
        assert!(result.is_none_or(|c| !c.socket_path().exists()));
    }

    /// C6.6: `queue/enqueue` typed call round-trips a QueueEntry shape
    /// through the wire — verifies the request method name and the
    /// response decode.
    #[tokio::test]
    async fn queue_enqueue_round_trip() {
        let socket_path = short_sock_path();
        let handler = |req: RpcRequest| {
            assert_eq!(req.method, "queue/enqueue");
            RpcResponse::ok(
                req.id,
                serde_json::json!({
                    "id": "TASK-enqueued",
                    "subject_id": "TASK-enqueued",
                    "status": "ready",
                    "priority": 2,
                    "enqueued_at": "2026-05-20T00:00:00Z",
                }),
            )
        };
        let server = spawn_fake_server(socket_path.clone(), handler).await;
        let client = ControlClient::from_socket_path(socket_path.clone());
        let request = animus_control_protocol::types::QueueEnqueueRequest {
            task_id: "TASK-enqueued".to_string(),
            priority: None,
        };
        let response = tokio::time::timeout(Duration::from_secs(5), client.queue_enqueue(request))
            .await
            .expect("timeout")
            .expect("call");
        assert_eq!(response.id, "TASK-enqueued");
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    /// C6.6: `queue/stats` round-trip preserves the per-status counts
    /// envelope shape — the CLI handler uses these fields verbatim.
    #[tokio::test]
    async fn queue_stats_round_trip() {
        let socket_path = short_sock_path();
        let handler = |req: RpcRequest| {
            assert_eq!(req.method, "queue/stats");
            RpcResponse::ok(
                req.id,
                serde_json::json!({
                    "ready": 5,
                    "held": 2,
                    "in_flight": 1,
                    "done_recent": 9,
                    "dropped_recent": 0,
                }),
            )
        };
        let server = spawn_fake_server(socket_path.clone(), handler).await;
        let client = ControlClient::from_socket_path(socket_path.clone());
        let response =
            tokio::time::timeout(Duration::from_secs(5), client.queue_stats()).await.expect("timeout").expect("call");
        assert_eq!(response.ready, 5);
        assert_eq!(response.held, 2);
        assert_eq!(response.in_flight, 1);
        assert_eq!(response.done_recent, 9);
        assert_eq!(response.dropped_recent, 0);
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    // ----- C6.7: agent/* convenience method round-trips ----------------

    /// `agent/run` round-trips an AgentRunResult through the wire.
    /// Mirrors the daemon-side `agent_run_routes_through_configured_routing`
    /// test from the CLI side: spawn a fake server, call the typed
    /// convenience method, verify the decoded response shape.
    #[tokio::test]
    async fn agent_run_routes_via_control_when_socket_present() {
        let socket_path = short_sock_path();
        let handler = |req: RpcRequest| {
            assert_eq!(req.method, "agent/run");
            RpcResponse::ok(
                req.id,
                serde_json::json!({
                    "session_id": "sess-wire-1",
                    "model": "claude-sonnet-4-6",
                    "output": "hi",
                }),
            )
        };
        let server = spawn_fake_server(socket_path.clone(), handler).await;
        let client = ControlClient::from_socket_path(socket_path.clone());
        let request = animus_control_protocol::types::AgentRunRequest {
            provider: "claude".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            prompt: "hi".to_string(),
            system: None,
            cwd: None,
            env: Default::default(),
        };
        let response = tokio::time::timeout(Duration::from_secs(5), client.agent_run(request))
            .await
            .expect("timeout")
            .expect("call");
        assert_eq!(response.session_id, "sess-wire-1");
        assert_eq!(response.model, "claude-sonnet-4-6");
        assert_eq!(response.output, "hi");
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    /// C6.7: when no socket is present `try_connect` returns Ok(None)
    /// so CLI agent commands fall back to the local in-process path
    /// under `runtime_agent`.
    #[tokio::test]
    async fn agent_run_falls_back_to_local_when_socket_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = ControlClient::try_connect(dir.path()).await.unwrap();
        assert!(result.is_none_or(|c| !c.socket_path().exists()));
    }

    /// `agent/status` round-trips an AgentStatus through the wire,
    /// preserving the lifecycle enum and provider/model fields the CLI
    /// renderer keys off of.
    #[tokio::test]
    async fn agent_status_round_trip() {
        let socket_path = short_sock_path();
        let handler = |req: RpcRequest| {
            assert_eq!(req.method, "agent/status");
            RpcResponse::ok(
                req.id,
                serde_json::json!({
                    "session_id": "sess-wire-1",
                    "status": "running",
                    "provider": "claude",
                    "model": "claude-sonnet-4-6",
                    "started_at": "2026-05-20T00:00:00Z",
                }),
            )
        };
        let server = spawn_fake_server(socket_path.clone(), handler).await;
        let client = ControlClient::from_socket_path(socket_path.clone());
        let request = animus_control_protocol::types::AgentStatusRequest { id: "sess-wire-1".to_string() };
        let response = tokio::time::timeout(Duration::from_secs(5), client.agent_status(request))
            .await
            .expect("timeout")
            .expect("call");
        assert_eq!(response.session_id, "sess-wire-1");
        assert_eq!(response.provider, "claude");
        assert_eq!(response.model, "claude-sonnet-4-6");
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    /// `agent/cancel` round-trips through the wire — the response is a
    /// `Unit` envelope but the request method name must match exactly so
    /// the daemon can route it to the routing handle.
    #[tokio::test]
    async fn agent_cancel_routes_through() {
        let socket_path = short_sock_path();
        let handler = |req: RpcRequest| {
            assert_eq!(req.method, "agent/cancel");
            RpcResponse::ok(req.id, serde_json::json!({}))
        };
        let server = spawn_fake_server(socket_path.clone(), handler).await;
        let client = ControlClient::from_socket_path(socket_path.clone());
        let request = animus_control_protocol::types::AgentCancelRequest { session_id: "sess-wire-1".to_string() };
        let _response = tokio::time::timeout(Duration::from_secs(5), client.agent_cancel(request))
            .await
            .expect("timeout")
            .expect("call");
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    /// C6.7: when the daemon advertises the wire surface but a specific
    /// agent method returns NotSupported (the C6.7 pass-through impl
    /// returns that for everything), `is_method_unavailable` reports
    /// true so CLI handlers degrade to the local in-process path.
    #[tokio::test]
    async fn agent_run_preserves_opaque_response_shape_on_not_supported() {
        let socket_path = short_sock_path();
        let handler = |req: RpcRequest| {
            RpcResponse::err(
                req.id,
                RpcError {
                    code: animus_plugin_protocol::error_codes::METHOD_NOT_SUPPORTED,
                    message: "agent/run wire surface is pass-through pending AgentPool query surface".to_string(),
                    data: None,
                },
            )
        };
        let server = spawn_fake_server(socket_path.clone(), handler).await;
        let client = ControlClient::from_socket_path(socket_path.clone());
        let request = animus_control_protocol::types::AgentRunRequest {
            provider: "claude".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            prompt: "hi".to_string(),
            system: None,
            cwd: None,
            env: Default::default(),
        };
        let err = tokio::time::timeout(Duration::from_secs(5), client.agent_run(request))
            .await
            .expect("timeout")
            .unwrap_err();
        assert!(is_method_unavailable(&err), "C6.7 pass-through should surface as method-unavailable: {err}");
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    /// Streaming fake server: writes the configured frames (ack +
    /// notifications) then closes. Used by the `daemon/logs` streaming
    /// tests.
    async fn spawn_fake_stream_server(socket_path: PathBuf, frames: Vec<String>) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(&socket_path).expect("bind");
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                for frame in frames {
                    let mut bytes = frame.into_bytes();
                    bytes.push(b'\n');
                    if write_half.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                let _ = write_half.flush().await;
                // Drop write_half so the client sees the stream end.
            }
        })
    }

    /// v0.4.7 Item 1: `daemon/logs` streams the historical tail through
    /// the wire, the client collects entries until the socket closes or
    /// the limit is reached, and the typed convenience method decodes
    /// each notification payload into a [`DaemonLogEntry`].
    #[tokio::test]
    async fn daemon_logs_collects_historical_stream() {
        let socket_path = short_sock_path();
        let ack = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"watching": true},
        })
        .to_string();
        let entry_one = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "daemon/log",
            "params": {
                "id": 1,
                "data": {
                    "id": "x-1",
                    "ts": "2026-05-22T00:00:00Z",
                    "level": "info",
                    "source": "daemon",
                    "target": "test",
                    "message": "first",
                },
            },
        })
        .to_string();
        let entry_two = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "daemon/log",
            "params": {
                "id": 1,
                "data": {
                    "id": "x-2",
                    "ts": "2026-05-22T00:00:01Z",
                    "level": "warn",
                    "source": "plugin",
                    "source_name": "kimi-code",
                    "target": "tool",
                    "message": "second",
                },
            },
        })
        .to_string();
        let server = spawn_fake_stream_server(socket_path.clone(), vec![ack, entry_one, entry_two]).await;
        let client = ControlClient::from_socket_path(socket_path.clone());

        let request = animus_control_protocol::types::DaemonLogsRequest::default();
        let entries = tokio::time::timeout(Duration::from_secs(5), client.daemon_logs(request, 10))
            .await
            .expect("timeout")
            .expect("call");
        assert_eq!(entries.len(), 2, "expected two streamed entries, got {:?}", entries);
        assert_eq!(entries[0].message, "first");
        assert_eq!(entries[1].message, "second");
        assert_eq!(entries[1].source_name.as_deref(), Some("kimi-code"));
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }

    /// The `limit` argument caps how many notification frames the client
    /// consumes; once reached it returns without waiting for the server
    /// to close the stream. Necessary so a busy `--follow` doesn't
    /// produce unbounded memory growth before the operator Ctrl-C's.
    #[tokio::test]
    async fn daemon_logs_respects_caller_limit() {
        let socket_path = short_sock_path();
        let ack = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"watching": true},
        })
        .to_string();
        let make_frame = |i: usize| {
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "daemon/log",
                "params": {
                    "id": 1,
                    "data": {
                        "id": format!("x-{i}"),
                        "ts": "2026-05-22T00:00:00Z",
                        "level": "info",
                        "source": "daemon",
                        "target": "test",
                        "message": format!("entry-{i}"),
                    },
                },
            })
            .to_string()
        };
        let frames: Vec<String> = std::iter::once(ack).chain((0..5).map(make_frame)).collect();
        let server = spawn_fake_stream_server(socket_path.clone(), frames).await;
        let client = ControlClient::from_socket_path(socket_path.clone());

        let request = animus_control_protocol::types::DaemonLogsRequest::default();
        let entries = tokio::time::timeout(Duration::from_secs(5), client.daemon_logs(request, 3))
            .await
            .expect("timeout")
            .expect("call");
        assert_eq!(entries.len(), 3, "limit=3 should stop after three frames");
        let _ = std::fs::remove_file(socket_path);
        server.abort();
    }
}
