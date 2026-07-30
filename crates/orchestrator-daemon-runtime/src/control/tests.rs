//! Integration tests for the daemon-side control server.
//!
//! These tests exercise the full wire path (UnixListener → JSON-RPC
//! framing → [`ControlSurface`] dispatch) using a hand-rolled test
//! double for the surface. The server's UNIX-only socket path means
//! these tests are gated on `cfg(unix)`; they're skipped on Windows
//! CI.
//!
//! Anti-deadlock rules:
//!
//! - Every test wraps in a `tokio::time::timeout` so a stuck server
//!   bails after 2 minutes rather than hanging the suite.
//! - No `std::sync::Mutex` held across `.await` in any test fixture.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use animus_control_protocol::{
    control_trait::{ControlSurface, DaemonEventStream, DaemonLogStream, SubjectWatchStream},
    types::{
        AgentCancelRequest, AgentRunRequest, AgentRunResult, AgentStatus, AgentStatusRequest,
        CodingFleetStatusResponse, DaemonAgentsResponse, DaemonEventsRequest, DaemonHealthResponse, DaemonHealthStatus,
        DaemonLogsRequest, DaemonStatusResponse, PluginBrowseRequest, PluginCallRequest, PluginCallResponse,
        PluginInfo, PluginInfoRequest, PluginInstallRequest, PluginInstallResponse, PluginListRequest,
        PluginListResponse, PluginPingRequest, PluginPingResponse, PluginSearchRequest, PluginSearchResponse,
        PluginUninstallRequest, PluginUpdateRequest, PluginUpdateResponse, ProjectInfo, ProjectInitRequest,
        ProjectSetupRequest, ProjectStatusResponse, QueueDropRequest, QueueEnqueueRequest, QueueEntry,
        QueueHoldRequest, QueueListRequest, QueueListResponse, QueueReleaseRequest, QueueReorderRequest, QueueStats,
        SubjectCreateRequest, SubjectGetRequest, SubjectListRequest, SubjectListResponse, SubjectNextRequest,
        SubjectNextResponse, SubjectStatusRequest, SubjectUpdateRequest, SubjectWatchRequest, Unit,
        WorkflowCancelRequest, WorkflowExecuteRequest, WorkflowGetRequest, WorkflowListRequest, WorkflowListResponse,
        WorkflowPauseRequest, WorkflowResumeRequest, WorkflowRun, WorkflowRunRequest, WorkflowRunStart,
    },
    ControlError,
};
use animus_plugin_protocol::{RpcRequest, RpcResponse};
use animus_subject_protocol_wire::{Subject, SubjectChangedEvent};
use async_trait::async_trait;
use futures_util::stream;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::control::{control_server_disable_env_set, ControlServer, CONTROL_SERVER_DISABLE_ENV};

const TEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Build a short test socket path under `/tmp` so it fits in `SUN_LEN`
/// (104 bytes on macOS, 108 on Linux). The default `tempfile::tempdir`
/// already lives under `/tmp/<random>` but `scoped_state_root` re-roots
/// the socket under `$HOME/.animus/<scope-hash>/control.sock`, and other
/// tests in the suite may set `$HOME` to a long path. Tests that don't
/// need to exercise scoped-state resolution should call this helper and
/// pass the result to [`ControlServer::start_with_socket`].
fn short_test_socket() -> PathBuf {
    let unique = format!("animus-ctl-{}-{}.sock", std::process::id(), uuid::Uuid::new_v4().simple());
    std::env::temp_dir().join(unique)
}

/// A minimal test double surface: every method returns a recognizable
/// canned response. Used to verify framing, routing, and error mapping
/// without standing up the full daemon.
#[derive(Default)]
struct TestSurface {
    daemon_pid: u32,
    daemon_version: String,
    project_root: PathBuf,
    received_methods: Arc<Mutex<Vec<String>>>,
    subject_list_response: Option<SubjectListResponse>,
    /// When set, `daemon_events` returns a stream that emits these
    /// pre-canned events once and then closes.
    canned_events: Option<Vec<animus_control_protocol::types::DaemonRunEvent>>,
}

impl TestSurface {
    fn new() -> Self {
        Self {
            daemon_pid: 9999,
            daemon_version: "test-0.1.0".to_string(),
            project_root: PathBuf::from("/tmp/test-project"),
            ..Default::default()
        }
    }

    fn with_subject_list(mut self, response: SubjectListResponse) -> Self {
        self.subject_list_response = Some(response);
        self
    }

    fn with_canned_events(mut self, events: Vec<animus_control_protocol::types::DaemonRunEvent>) -> Self {
        self.canned_events = Some(events);
        self
    }

    async fn record(&self, method: &str) {
        self.received_methods.lock().await.push(method.to_string());
    }

    async fn methods(&self) -> Vec<String> {
        self.received_methods.lock().await.clone()
    }
}

#[async_trait]
impl ControlSurface for TestSurface {
    async fn subject_list(&self, _request: SubjectListRequest) -> Result<SubjectListResponse, ControlError> {
        self.record("subject/list").await;
        self.subject_list_response
            .clone()
            .ok_or_else(|| ControlError::NotSupported("test surface did not configure subject/list".to_string()))
    }

    async fn subject_get(&self, _request: SubjectGetRequest) -> Result<Subject, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn subject_create(&self, _request: SubjectCreateRequest) -> Result<Subject, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn subject_update(&self, _request: SubjectUpdateRequest) -> Result<Subject, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn subject_next(&self, _request: SubjectNextRequest) -> Result<SubjectNextResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn subject_status(&self, _request: SubjectStatusRequest) -> Result<Subject, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn subject_watch(&self, _request: SubjectWatchRequest) -> Result<SubjectWatchStream, ControlError> {
        Ok(Box::pin(stream::empty::<SubjectChangedEvent>())
            as Pin<Box<dyn futures_core::Stream<Item = SubjectChangedEvent> + Send>>)
    }

    async fn plugin_list(&self, _request: PluginListRequest) -> Result<PluginListResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn plugin_info(&self, _request: PluginInfoRequest) -> Result<PluginInfo, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn plugin_install(&self, _request: PluginInstallRequest) -> Result<PluginInstallResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn plugin_uninstall(&self, _request: PluginUninstallRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn plugin_ping(&self, _request: PluginPingRequest) -> Result<PluginPingResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn plugin_call(&self, _request: PluginCallRequest) -> Result<PluginCallResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn plugin_search(&self, _request: PluginSearchRequest) -> Result<PluginSearchResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn plugin_browse(&self, _request: PluginBrowseRequest) -> Result<PluginSearchResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn plugin_update(&self, _request: PluginUpdateRequest) -> Result<PluginUpdateResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn daemon_status(&self) -> Result<DaemonStatusResponse, ControlError> {
        self.record("daemon/status").await;
        Ok(DaemonStatusResponse {
            running: true,
            pid: Some(self.daemon_pid),
            uptime_seconds: Some(42),
            version: Some(self.daemon_version.clone()),
            project_root: Some(self.project_root.clone()),
            log_path: None,
        })
    }

    async fn daemon_health(&self) -> Result<DaemonHealthResponse, ControlError> {
        Ok(DaemonHealthResponse { status: DaemonHealthStatus::Healthy, plugins: Vec::new(), last_error: None })
    }

    async fn daemon_start(&self) -> Result<Unit, ControlError> {
        Ok(Unit::default())
    }

    async fn daemon_stop(&self) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn daemon_restart(&self) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn daemon_agents(&self) -> Result<DaemonAgentsResponse, ControlError> {
        Ok(DaemonAgentsResponse { agents: Vec::new() })
    }

    async fn fleet_status(&self) -> Result<CodingFleetStatusResponse, ControlError> {
        self.record("fleet/status").await;
        Ok(CodingFleetStatusResponse {
            capacity: 5,
            active: 0,
            available: 5,
            owner_id: "test-daemon".to_string(),
            reservations: Vec::new(),
        })
    }

    async fn daemon_events(&self, _request: DaemonEventsRequest) -> Result<DaemonEventStream, ControlError> {
        let events = self.canned_events.clone().unwrap_or_default();
        Ok(Box::pin(stream::iter(events)))
    }

    async fn daemon_logs(&self, _request: DaemonLogsRequest) -> Result<DaemonLogStream, ControlError> {
        Ok(Box::pin(stream::empty()))
    }

    async fn workflow_list(&self, _request: WorkflowListRequest) -> Result<WorkflowListResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn workflow_get(&self, _request: WorkflowGetRequest) -> Result<WorkflowRun, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn workflow_run(&self, _request: WorkflowRunRequest) -> Result<WorkflowRunStart, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn workflow_execute(&self, _request: WorkflowExecuteRequest) -> Result<WorkflowRunStart, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn workflow_pause(&self, _request: WorkflowPauseRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn workflow_resume(&self, _request: WorkflowResumeRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn workflow_cancel(&self, _request: WorkflowCancelRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn agent_run(&self, _request: AgentRunRequest) -> Result<AgentRunResult, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn agent_status(&self, _request: AgentStatusRequest) -> Result<AgentStatus, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn agent_cancel(&self, _request: AgentCancelRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn queue_list(&self, _request: QueueListRequest) -> Result<QueueListResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn queue_enqueue(&self, _request: QueueEnqueueRequest) -> Result<QueueEntry, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn queue_drop(&self, _request: QueueDropRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn queue_hold(&self, _request: QueueHoldRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn queue_release(&self, _request: QueueReleaseRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn queue_reorder(&self, _request: QueueReorderRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn queue_stats(&self) -> Result<QueueStats, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn project_init(&self, _request: ProjectInitRequest) -> Result<ProjectInfo, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn project_setup(&self, _request: ProjectSetupRequest) -> Result<ProjectInfo, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }

    async fn project_status(&self) -> Result<ProjectStatusResponse, ControlError> {
        Err(ControlError::NotSupported("test".to_string()))
    }
}

/// Write a JSON-RPC frame onto the socket and flush.
async fn send_frame(writer: &mut tokio::io::WriteHalf<UnixStream>, request: &RpcRequest) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(request).unwrap();
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}

/// Read one JSON-RPC frame from the socket.
async fn read_frame_value(reader: &mut BufReader<tokio::io::ReadHalf<UnixStream>>) -> std::io::Result<Value> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed"));
    }
    serde_json::from_str(line.trim()).map_err(std::io::Error::other)
}

#[tokio::test]
async fn server_binds_socket_with_0700_perms() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let handle = ControlServer::start_with_socket(short_test_socket(), surface).await.unwrap();
        let socket = handle.socket_path().to_path_buf();
        assert!(socket.exists(), "socket file must exist");
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "socket must be chmod 0700 (got {mode:o})");
        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn server_removes_stale_socket_on_start() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());

        // Pre-create a stale "socket" file (any regular file at the path
        // should also be cleaned).
        let socket_path = short_test_socket();
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        std::fs::write(&socket_path, b"stale").unwrap();
        assert!(socket_path.exists());

        let handle = ControlServer::start_with_socket(socket_path, surface).await.unwrap();
        assert!(handle.socket_path().exists(), "fresh socket must bind");
        // Mode must be 0700 even after stale cleanup.
        let mode = std::fs::metadata(handle.socket_path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn daemon_status_round_trip() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let test_surface = Arc::new(TestSurface::new());
        let surface: Arc<dyn ControlSurface> = test_surface.clone();
        let handle = ControlServer::start_with_socket(short_test_socket(), surface).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(1, "daemon/status", None);
        send_frame(&mut write_half, &request).await.unwrap();

        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert_eq!(response.id, Some(json!(1)));
        assert!(response.error.is_none(), "expected ok, got {:?}", response.error);
        let result = response.result.expect("result");
        let parsed: DaemonStatusResponse = serde_json::from_value(result).unwrap();
        assert!(parsed.running);
        assert_eq!(parsed.pid, Some(9999));
        assert_eq!(parsed.version.as_deref(), Some("test-0.1.0"));

        let recorded = test_surface.methods().await;
        assert!(recorded.iter().any(|m| m == "daemon/status"));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn daemon_metrics_round_trip_emits_snapshot() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        crate::metrics::global().incr_counter("workflow_runs_total{status=completed}");
        crate::metrics::global().incr_counter_by("phase_executions_total{status=completed}", 4);
        crate::metrics::global().set_gauge("workflow_in_flight", 2.0);

        let test_surface = Arc::new(TestSurface::new());
        let surface: Arc<dyn ControlSurface> = test_surface.clone();
        let handle = ControlServer::start_with_socket(short_test_socket(), surface).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(42, "daemon/metrics", None);
        send_frame(&mut write_half, &request).await.unwrap();

        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert_eq!(response.id, Some(json!(42)));
        assert!(response.error.is_none(), "expected ok, got {:?}", response.error);
        let result = response.result.expect("metrics result present");
        let snapshot: crate::metrics::MetricsSnapshot = serde_json::from_value(result).unwrap();
        assert!(
            snapshot.counters.get("workflow_runs_total{status=completed}").copied().unwrap_or(0) >= 1,
            "counter must be at least 1, got {:?}",
            snapshot.counters.get("workflow_runs_total{status=completed}")
        );
        assert!(
            snapshot.counters.get("phase_executions_total{status=completed}").copied().unwrap_or(0) >= 4,
            "phase counter must be at least 4, got {:?}",
            snapshot.counters.get("phase_executions_total{status=completed}")
        );
        assert!(snapshot.gauges.contains_key("daemon_uptime_seconds"));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// `daemon/nudge` is an in-tree wire-string method: it must reply
/// `{"nudged": true}` and wake the process-global scheduler nudge handle
/// when one is installed (i.e. a daemon loop is running in this process).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn daemon_nudge_round_trip_wakes_installed_notify() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        // Serialize against the in-crate unit tests that install/clear the
        // process-global nudge slot.
        let _slot_lock = crate::daemon::nudge_test_lock().lock().unwrap_or_else(|p| p.into_inner());

        let notify = Arc::new(tokio::sync::Notify::new());
        crate::daemon::install_scheduler_nudge(notify.clone());

        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let handle = ControlServer::start_with_socket(short_test_socket(), surface).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(7, "daemon/nudge", None);
        send_frame(&mut write_half, &request).await.unwrap();

        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert_eq!(response.id, Some(json!(7)));
        assert!(response.error.is_none(), "expected ok, got {:?}", response.error);
        assert_eq!(response.result, Some(json!({"nudged": true})));

        // The nudge must have stored a permit on the installed handle.
        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .expect("daemon/nudge must wake the installed scheduler notify");

        crate::daemon::clear_scheduler_nudge();
        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn subject_list_routes_to_surface() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let canned = SubjectListResponse { subjects: Vec::new(), next_cursor: None, fetched_at: chrono::Utc::now() };
        let test_surface = Arc::new(TestSurface::new().with_subject_list(canned.clone()));
        let surface: Arc<dyn ControlSurface> = test_surface.clone();
        let handle = ControlServer::start_with_socket(short_test_socket(), surface).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(2, "subject/list", Some(json!({"filter": {}})));
        send_frame(&mut write_half, &request).await.unwrap();

        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert!(response.error.is_none());
        let result = response.result.expect("result");
        let parsed: SubjectListResponse = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.subjects.len(), 0);

        let recorded = test_surface.methods().await;
        assert!(recorded.iter().any(|m| m == "subject/list"));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn streaming_subscribe_emits_notifications() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let event = animus_control_protocol::types::DaemonRunEvent {
            id: "evt-1".to_string(),
            occurred_at: chrono::Utc::now(),
            kind: "tick".to_string(),
            payload: json!({"interval": 5}),
        };
        let test_surface = Arc::new(TestSurface::new().with_canned_events(vec![event.clone()]));
        let surface: Arc<dyn ControlSurface> = test_surface;
        let handle = ControlServer::start_with_socket(short_test_socket(), surface).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(3, "daemon/events", Some(json!({})));
        send_frame(&mut write_half, &request).await.unwrap();

        // First frame: the ack response.
        let ack_value = read_frame_value(&mut reader).await.unwrap();
        let ack: RpcResponse = serde_json::from_value(ack_value).unwrap();
        assert!(ack.error.is_none());
        assert_eq!(ack.result, Some(json!({"watching": true})), "expected watching=true ack");

        // Second frame: the streamed notification.
        let notif_value = read_frame_value(&mut reader).await.unwrap();
        assert_eq!(notif_value.get("method").and_then(|v| v.as_str()), Some("daemon/event"));
        let params = notif_value.get("params").expect("notification params");
        let data = params.get("data").expect("notification data");
        assert_eq!(data.get("kind").and_then(|v| v.as_str()), Some("tick"));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn unknown_method_returns_method_not_found_error() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let handle = ControlServer::start_with_socket(short_test_socket(), surface).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(4, "does/not-exist", None);
        send_frame(&mut write_half, &request).await.unwrap();

        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        let error = response.error.expect("error frame");
        assert_eq!(error.code, animus_plugin_protocol::error_codes::METHOD_NOT_FOUND);
        assert!(error.message.contains("does/not-exist"), "method name should be echoed; got {}", error.message);

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn connection_closes_cleanly_on_client_disconnect() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let handle = ControlServer::start_with_socket(short_test_socket(), surface).await.unwrap();

        // Connect, send one request, drop without reading reply.
        {
            let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
            let (_read_half, mut write_half) = tokio::io::split(stream);
            let request = RpcRequest::new(5, "daemon/status", None);
            send_frame(&mut write_half, &request).await.unwrap();
            // Drop here.
        }

        // Server should still accept a fresh connection.
        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(6, "daemon/status", None);
        send_frame(&mut write_half, &request).await.unwrap();
        let _value = read_frame_value(&mut reader).await.unwrap();

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn disable_env_var_is_respected_by_helper() {
    // The outer guard holds the process-wide env lock for the whole test
    // and restores the var's original value even on panic; the raw
    // mutations inside the loops are safe under that lock.
    let _guard = protocol::test_utils::EnvVarGuard::set(CONTROL_SERVER_DISABLE_ENV, None);
    let truthy = ["1", "true", "yes", "TRUE", "anything"];
    let falsey = ["", "0", "false", "no", "off"];

    for v in truthy {
        std::env::set_var(CONTROL_SERVER_DISABLE_ENV, v);
        assert!(control_server_disable_env_set(), "value {v:?} should be truthy");
    }
    for v in falsey {
        std::env::set_var(CONTROL_SERVER_DISABLE_ENV, v);
        assert!(!control_server_disable_env_set(), "value {v:?} should be falsey");
    }
    std::env::remove_var(CONTROL_SERVER_DISABLE_ENV);
    assert!(!control_server_disable_env_set(), "unset is falsey");
}

#[tokio::test]
async fn parse_error_returns_parse_error_frame() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let handle = ControlServer::start_with_socket(short_test_socket(), surface).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        // Send garbage JSON.
        write_half.write_all(b"not-json\n").await.unwrap();
        write_half.flush().await.unwrap();

        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        let error = response.error.expect("error frame");
        assert_eq!(error.code, animus_plugin_protocol::error_codes::PARSE_ERROR);

        // Server should still accept follow-up valid frames.
        let request = RpcRequest::new(7, "daemon/status", None);
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert!(response.error.is_none());

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

// =====================================================================
// C6: plugin/* + daemon/* routing handles wired through the surface.
// =====================================================================

use crate::control::{DaemonOpsRouting, InProcessSurface, PluginRouting};

#[derive(Default)]
struct StubPluginRouting {
    seen: Arc<Mutex<Vec<String>>>,
}

impl StubPluginRouting {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        (Self { seen: seen.clone() }, seen)
    }

    async fn record(&self, method: &str) {
        self.seen.lock().await.push(method.to_string());
    }
}

#[async_trait]
impl PluginRouting for StubPluginRouting {
    async fn plugin_list(&self, _request: PluginListRequest) -> Result<PluginListResponse, ControlError> {
        self.record("plugin/list").await;
        Ok(PluginListResponse {
            plugins: vec![PluginInfo {
                name: "stub-plugin".to_string(),
                version: "0.1.0".to_string(),
                kind: "subject_backend".to_string(),
                source: Some("local".to_string()),
                signature_verified: false,
                description: Some("stub for c6 test".to_string()),
                binary_path: None,
            }],
            warnings: Vec::new(),
        })
    }
    async fn plugin_info(&self, _request: PluginInfoRequest) -> Result<PluginInfo, ControlError> {
        Err(ControlError::NotSupported("stub".to_string()))
    }
    async fn plugin_install(&self, _request: PluginInstallRequest) -> Result<PluginInstallResponse, ControlError> {
        Err(ControlError::NotSupported("stub".to_string()))
    }
    async fn plugin_uninstall(&self, _request: PluginUninstallRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".to_string()))
    }
    async fn plugin_ping(&self, _request: PluginPingRequest) -> Result<PluginPingResponse, ControlError> {
        Ok(PluginPingResponse { ok: true, latency_ms: Some(3), error: None })
    }
    async fn plugin_call(&self, _request: PluginCallRequest) -> Result<PluginCallResponse, ControlError> {
        Err(ControlError::NotSupported("stub".to_string()))
    }
    async fn plugin_search(&self, _request: PluginSearchRequest) -> Result<PluginSearchResponse, ControlError> {
        Err(ControlError::NotSupported("stub".to_string()))
    }
    async fn plugin_browse(&self, _request: PluginBrowseRequest) -> Result<PluginSearchResponse, ControlError> {
        Err(ControlError::NotSupported("stub".to_string()))
    }
    async fn plugin_update(&self, _request: PluginUpdateRequest) -> Result<PluginUpdateResponse, ControlError> {
        Err(ControlError::NotSupported("stub".to_string()))
    }
}

#[derive(Default)]
struct StubDaemonOpsRouting;

#[async_trait]
impl DaemonOpsRouting for StubDaemonOpsRouting {
    async fn daemon_status(&self) -> Result<DaemonStatusResponse, ControlError> {
        Ok(DaemonStatusResponse {
            running: true,
            pid: Some(42),
            uptime_seconds: Some(99),
            version: Some("stub-1.2.3".to_string()),
            project_root: Some(PathBuf::from("/tmp/stub-project")),
            log_path: None,
        })
    }
    async fn daemon_health(&self) -> Result<DaemonHealthResponse, ControlError> {
        Ok(DaemonHealthResponse {
            status: DaemonHealthStatus::Degraded,
            plugins: Vec::new(),
            last_error: Some("stub-degraded".to_string()),
        })
    }
    async fn daemon_agents(&self) -> Result<DaemonAgentsResponse, ControlError> {
        Ok(DaemonAgentsResponse { agents: Vec::new() })
    }
}

/// `plugin/list` routes through the configured PluginRouting handle.
/// Without the handle the surface returns NotSupported; with it the
/// canned response from StubPluginRouting flows back over the wire.
#[tokio::test]
async fn plugin_list_routes_through_configured_routing() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (routing, seen) = StubPluginRouting::new();
        let routing_arc: Arc<dyn PluginRouting> = Arc::new(routing);
        let surface = InProcessSurface::builder(PathBuf::from("/tmp/c6-test")).plugin_routing(routing_arc).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(101, "plugin/list", Some(json!({})));
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert!(response.error.is_none(), "plugin/list should succeed when routing is wired");
        let result = response.result.expect("result");
        let plugins = result.get("plugins").and_then(|v| v.as_array()).unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].get("name").and_then(|v| v.as_str()), Some("stub-plugin"));

        // Verify the routing handle was actually invoked.
        let calls = seen.lock().await.clone();
        assert_eq!(calls, vec!["plugin/list".to_string()]);

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// Without a PluginRouting handle attached, `plugin/list` returns the
/// NotSupported error frame (preserves pre-C6 behavior for tests / MCP
/// callers that haven't been wired yet).
#[tokio::test]
async fn plugin_list_without_routing_returns_not_supported() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface = InProcessSurface::builder(PathBuf::from("/tmp/c6-test-noplug")).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(102, "plugin/list", Some(json!({})));
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        let error = response.error.expect("error frame");
        assert_eq!(error.code, animus_plugin_protocol::error_codes::METHOD_NOT_SUPPORTED);

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// `daemon/status` routes through the configured DaemonOpsRouting
/// handle when present (e.g. when the daemon is up and the CLI is
/// asking for live process info via the wire).
#[tokio::test]
async fn daemon_status_routes_through_configured_routing() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let routing_arc: Arc<dyn DaemonOpsRouting> = Arc::new(StubDaemonOpsRouting);
        let surface =
            InProcessSurface::builder(PathBuf::from("/tmp/c6-test-daemon")).daemon_ops_routing(routing_arc).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(103, "daemon/status", None);
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert!(response.error.is_none(), "daemon/status should succeed when routing is wired");
        let result = response.result.expect("result");
        assert_eq!(result.get("pid").and_then(|v| v.as_u64()), Some(42));
        assert_eq!(result.get("version").and_then(|v| v.as_str()), Some("stub-1.2.3"));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// `daemon/health` routes through the routing handle and surfaces a
/// Degraded health verdict end-to-end via the wire.
#[tokio::test]
async fn daemon_health_routes_through_configured_routing() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let routing_arc: Arc<dyn DaemonOpsRouting> = Arc::new(StubDaemonOpsRouting);
        let surface =
            InProcessSurface::builder(PathBuf::from("/tmp/c6-test-health")).daemon_ops_routing(routing_arc).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(104, "daemon/health", None);
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert!(response.error.is_none());
        let result = response.result.expect("result");
        assert_eq!(result.get("status").and_then(|v| v.as_str()), Some("degraded"));
        assert_eq!(result.get("last_error").and_then(|v| v.as_str()), Some("stub-degraded"));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

// =====================================================================
// C6.5: workflow/* routing handle wired through the surface.
// =====================================================================

use crate::control::WorkflowRouting;

#[derive(Default)]
struct StubWorkflowRouting {
    seen: Arc<Mutex<Vec<String>>>,
}

impl StubWorkflowRouting {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        (Self { seen: seen.clone() }, seen)
    }

    async fn record(&self, method: &str) {
        self.seen.lock().await.push(method.to_string());
    }
}

#[async_trait]
impl WorkflowRouting for StubWorkflowRouting {
    async fn workflow_list(&self, _request: WorkflowListRequest) -> Result<WorkflowListResponse, ControlError> {
        self.record("workflow/list").await;
        Ok(WorkflowListResponse {
            runs: vec![animus_control_protocol::types::WorkflowRunSummary {
                id: "wf-stub-1".to_string(),
                definition: "standard-workflow".to_string(),
                status: animus_control_protocol::types::WorkflowStatus::Running,
                subject_id: None,
                started_at: chrono::Utc::now(),
                finished_at: None,
            }],
            total: Some(1),
            next_cursor: None,
        })
    }

    async fn workflow_get(&self, _request: WorkflowGetRequest) -> Result<WorkflowRun, ControlError> {
        self.record("workflow/get").await;
        Ok(WorkflowRun {
            summary: animus_control_protocol::types::WorkflowRunSummary {
                id: "wf-stub-1".to_string(),
                definition: "standard-workflow".to_string(),
                status: animus_control_protocol::types::WorkflowStatus::Paused,
                subject_id: None,
                started_at: chrono::Utc::now(),
                finished_at: None,
            },
            detail: json!({"phases": ["plan", "impl"], "machine_state": "paused"}),
        })
    }

    async fn workflow_run(&self, _request: WorkflowRunRequest) -> Result<WorkflowRunStart, ControlError> {
        self.record("workflow/run").await;
        Ok(WorkflowRunStart {
            workflow_id: "wf-stub-new".to_string(),
            status: animus_control_protocol::types::WorkflowStatus::Pending,
            started_at: chrono::Utc::now(),
        })
    }

    async fn workflow_execute(&self, _request: WorkflowExecuteRequest) -> Result<WorkflowRunStart, ControlError> {
        Err(ControlError::NotSupported("stub".to_string()))
    }

    async fn workflow_pause(&self, _request: WorkflowPauseRequest) -> Result<Unit, ControlError> {
        self.record("workflow/pause").await;
        Ok(Unit::default())
    }

    async fn workflow_resume(&self, _request: WorkflowResumeRequest) -> Result<Unit, ControlError> {
        self.record("workflow/resume").await;
        Ok(Unit::default())
    }

    async fn workflow_cancel(&self, _request: WorkflowCancelRequest) -> Result<Unit, ControlError> {
        self.record("workflow/cancel").await;
        Ok(Unit::default())
    }
}

/// `workflow/list` routes through the configured WorkflowRouting handle.
/// Without the handle the surface returns NotSupported (preserves the
/// pre-C6.5 behavior for MCP/WebAPI callers that land later); with it
/// the canned response from StubWorkflowRouting flows back over the
/// wire.
#[tokio::test]
async fn workflow_list_routes_through_configured_routing() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (routing, seen) = StubWorkflowRouting::new();
        let routing_arc: Arc<dyn WorkflowRouting> = Arc::new(routing);
        let surface =
            InProcessSurface::builder(PathBuf::from("/tmp/c65-test-list")).workflow_routing(routing_arc).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(201, "workflow/list", Some(json!({})));
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert!(response.error.is_none(), "workflow/list should succeed when routing is wired: {response:?}");
        let result = response.result.expect("result");
        let runs = result.get("runs").and_then(|v| v.as_array()).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].get("id").and_then(|v| v.as_str()), Some("wf-stub-1"));

        let calls = seen.lock().await.clone();
        assert_eq!(calls, vec!["workflow/list".to_string()]);

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// Without a WorkflowRouting handle attached, `workflow/list` returns the
/// NotSupported error frame (preserves pre-C6.5 behavior for tests / MCP
/// callers that haven't been wired yet).
#[tokio::test]
async fn workflow_list_without_routing_returns_not_supported() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface = InProcessSurface::builder(PathBuf::from("/tmp/c65-test-noflow")).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(202, "workflow/list", Some(json!({})));
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        let error = response.error.expect("error frame");
        assert_eq!(error.code, animus_plugin_protocol::error_codes::METHOD_NOT_SUPPORTED);

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// `workflow/pause`, `workflow/resume`, and `workflow/cancel` round-trip
/// through the configured routing handle and the stub records the
/// invocation order.
#[tokio::test]
async fn workflow_pause_resume_cancel_round_trip() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (routing, seen) = StubWorkflowRouting::new();
        let routing_arc: Arc<dyn WorkflowRouting> = Arc::new(routing);
        let surface =
            InProcessSurface::builder(PathBuf::from("/tmp/c65-test-lifecycle")).workflow_routing(routing_arc).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        for (id, method) in [(203, "workflow/pause"), (204, "workflow/resume"), (205, "workflow/cancel")] {
            let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut reader = BufReader::new(read_half);
            let request = RpcRequest::new(id, method, Some(json!({"id": "wf-stub-1"})));
            send_frame(&mut write_half, &request).await.unwrap();
            let value = read_frame_value(&mut reader).await.unwrap();
            let response: RpcResponse = serde_json::from_value(value).unwrap();
            assert!(response.error.is_none(), "{method} should succeed: {response:?}");
        }

        let calls = seen.lock().await.clone();
        assert_eq!(
            calls,
            vec!["workflow/pause".to_string(), "workflow/resume".to_string(), "workflow/cancel".to_string()]
        );

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// `workflow/get` preserves the opaque `detail` payload end-to-end so
/// callers that need the rich daemon-side schema can decode it. The
/// summary fields (id, definition, status, started_at) live alongside.
#[tokio::test]
async fn workflow_get_preserves_opaque_detail_shape() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (routing, _) = StubWorkflowRouting::new();
        let routing_arc: Arc<dyn WorkflowRouting> = Arc::new(routing);
        let surface =
            InProcessSurface::builder(PathBuf::from("/tmp/c65-test-get")).workflow_routing(routing_arc).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(206, "workflow/get", Some(json!({"id": "wf-stub-1"})));
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert!(response.error.is_none(), "workflow/get should succeed: {response:?}");
        let result = response.result.expect("result");
        // Summary fields land at the top level via #[serde(flatten)].
        assert_eq!(result.get("id").and_then(|v| v.as_str()), Some("wf-stub-1"));
        assert_eq!(result.get("status").and_then(|v| v.as_str()), Some("paused"));
        // Opaque detail must survive the round-trip.
        let detail = result.get("detail").expect("detail field");
        assert_eq!(detail.get("machine_state").and_then(|v| v.as_str()), Some("paused"));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// `plugin/ping` round-trips a successful response with latency_ms
/// through the wire when the routing handle is wired.
#[tokio::test]
async fn plugin_ping_routes_through_configured_routing() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (routing, _) = StubPluginRouting::new();
        let routing_arc: Arc<dyn PluginRouting> = Arc::new(routing);
        let surface = InProcessSurface::builder(PathBuf::from("/tmp/c6-test-ping")).plugin_routing(routing_arc).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(105, "plugin/ping", Some(json!({"name": "x"})));
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert!(response.error.is_none(), "plugin/ping should succeed: {response:?}");
        let result = response.result.expect("result");
        assert_eq!(result.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert!(result.get("latency_ms").and_then(|v| v.as_u64()).is_some());

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

// =====================================================================
// C6.6: queue/* routing handle wired through the surface.
// =====================================================================

use crate::control::QueueRouting;

#[derive(Default)]
struct StubQueueRouting {
    seen: Arc<Mutex<Vec<String>>>,
}

impl StubQueueRouting {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        (Self { seen: seen.clone() }, seen)
    }

    async fn record(&self, method: &str) {
        self.seen.lock().await.push(method.to_string());
    }
}

#[async_trait]
impl QueueRouting for StubQueueRouting {
    async fn queue_list(&self, _request: QueueListRequest) -> Result<QueueListResponse, ControlError> {
        self.record("queue/list").await;
        Ok(QueueListResponse {
            entries: vec![QueueEntry {
                id: "TASK-stub-1".to_string(),
                subject_id: animus_subject_protocol_wire::SubjectId::new("TASK-stub-1"),
                status: animus_control_protocol::types::QueueEntryStatus::Ready,
                priority: 2,
                enqueued_at: chrono::Utc::now(),
                hold_reason: None,
            }],
            next_cursor: None,
        })
    }

    async fn queue_enqueue(&self, _request: QueueEnqueueRequest) -> Result<QueueEntry, ControlError> {
        self.record("queue/enqueue").await;
        Ok(QueueEntry {
            id: "TASK-stub-new".to_string(),
            subject_id: animus_subject_protocol_wire::SubjectId::new("TASK-stub-new"),
            status: animus_control_protocol::types::QueueEntryStatus::Ready,
            priority: 2,
            enqueued_at: chrono::Utc::now(),
            hold_reason: None,
        })
    }

    async fn queue_drop(&self, _request: QueueDropRequest) -> Result<Unit, ControlError> {
        self.record("queue/drop").await;
        Ok(Unit::default())
    }

    async fn queue_hold(&self, _request: QueueHoldRequest) -> Result<Unit, ControlError> {
        self.record("queue/hold").await;
        Ok(Unit::default())
    }

    async fn queue_release(&self, _request: QueueReleaseRequest) -> Result<Unit, ControlError> {
        self.record("queue/release").await;
        Ok(Unit::default())
    }

    async fn queue_reorder(&self, _request: QueueReorderRequest) -> Result<Unit, ControlError> {
        self.record("queue/reorder").await;
        Ok(Unit::default())
    }

    async fn queue_stats(&self) -> Result<QueueStats, ControlError> {
        self.record("queue/stats").await;
        Ok(QueueStats { ready: 3, held: 1, in_flight: 0, done_recent: 7, dropped_recent: 2 })
    }
}

/// `queue/list` routes through the configured QueueRouting handle.
/// Without the handle the surface returns NotSupported (preserves the
/// pre-C6.6 behavior for MCP/WebAPI callers that land later); with it
/// the canned response from StubQueueRouting flows back over the wire.
#[tokio::test]
async fn queue_list_routes_through_configured_routing() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (routing, seen) = StubQueueRouting::new();
        let routing_arc: Arc<dyn QueueRouting> = Arc::new(routing);
        let surface = InProcessSurface::builder(PathBuf::from("/tmp/c66-test-list")).queue_routing(routing_arc).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(301, "queue/list", Some(json!({})));
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert!(response.error.is_none(), "queue/list should succeed when routing is wired: {response:?}");
        let result = response.result.expect("result");
        let entries = result.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get("id").and_then(|v| v.as_str()), Some("TASK-stub-1"));

        let calls = seen.lock().await.clone();
        assert_eq!(calls, vec!["queue/list".to_string()]);

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// Without a QueueRouting handle attached, `queue/list` returns the
/// NotSupported error frame (preserves pre-C6.6 behavior for tests /
/// MCP callers that haven't been wired yet).
#[tokio::test]
async fn queue_list_without_routing_returns_not_supported() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface = InProcessSurface::builder(PathBuf::from("/tmp/c66-test-noqueue")).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(302, "queue/list", Some(json!({})));
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        let error = response.error.expect("error frame");
        assert_eq!(error.code, animus_plugin_protocol::error_codes::METHOD_NOT_SUPPORTED);

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// `queue/drop`, `queue/hold`, `queue/release` round-trip through the
/// configured routing handle and the stub records the invocation order.
#[tokio::test]
async fn queue_drop_hold_release_round_trip() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (routing, seen) = StubQueueRouting::new();
        let routing_arc: Arc<dyn QueueRouting> = Arc::new(routing);
        let surface =
            InProcessSurface::builder(PathBuf::from("/tmp/c66-test-lifecycle")).queue_routing(routing_arc).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        for (id, method) in [(303, "queue/hold"), (304, "queue/release"), (305, "queue/drop")] {
            let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut reader = BufReader::new(read_half);
            let request = RpcRequest::new(id, method, Some(json!({"id": "TASK-stub-1"})));
            send_frame(&mut write_half, &request).await.unwrap();
            let value = read_frame_value(&mut reader).await.unwrap();
            let response: RpcResponse = serde_json::from_value(value).unwrap();
            assert!(response.error.is_none(), "{method} should succeed: {response:?}");
        }

        let calls = seen.lock().await.clone();
        assert_eq!(calls, vec!["queue/hold".to_string(), "queue/release".to_string(), "queue/drop".to_string()]);

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// `queue/stats` preserves the per-status counts end-to-end through the
/// wire so CLI / MCP callers see the same shape regardless of the
/// transport.
#[tokio::test]
async fn queue_stats_preserves_envelope_shape() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (routing, _) = StubQueueRouting::new();
        let routing_arc: Arc<dyn QueueRouting> = Arc::new(routing);
        let surface =
            InProcessSurface::builder(PathBuf::from("/tmp/c66-test-stats")).queue_routing(routing_arc).build();
        let surface_arc: Arc<dyn ControlSurface> = Arc::new(surface);
        let handle = ControlServer::start_with_socket(short_test_socket(), surface_arc).await.unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let request = RpcRequest::new(306, "queue/stats", None);
        send_frame(&mut write_half, &request).await.unwrap();
        let value = read_frame_value(&mut reader).await.unwrap();
        let response: RpcResponse = serde_json::from_value(value).unwrap();
        assert!(response.error.is_none(), "queue/stats should succeed: {response:?}");
        let result = response.result.expect("result");
        assert_eq!(result.get("ready").and_then(|v| v.as_u64()), Some(3));
        assert_eq!(result.get("held").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(result.get("in_flight").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(result.get("done_recent").and_then(|v| v.as_u64()), Some(7));
        assert_eq!(result.get("dropped_recent").and_then(|v| v.as_u64()), Some(2));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn workflow_events_subscription_returns_phase_started_phase_completed_for_one_workflow() {
    use crate::control::workflow_events::WorkflowEventBroadcaster;
    use animus_control_protocol::types::WorkflowEvent;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let broadcaster = WorkflowEventBroadcaster::new();
        let handle = ControlServer::start_with_socket_and_workflow_events(
            short_test_socket(),
            surface,
            Some(Arc::clone(&broadcaster)),
        )
        .await
        .unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(900, "workflow/events", Some(json!({})));
        send_frame(&mut write_half, &request).await.unwrap();

        let ack_value = read_frame_value(&mut reader).await.unwrap();
        let ack: RpcResponse = serde_json::from_value(ack_value).unwrap();
        assert!(ack.error.is_none(), "ack should succeed: {ack:?}");
        assert_eq!(ack.result, Some(json!({"watching": true})));

        // Wait until the broadcaster sees the subscription before emitting,
        // so we don't race the per-connection driver task.
        for _ in 0..50 {
            if broadcaster.subscriber_count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(broadcaster.subscriber_count(), 1);

        broadcaster.emit(WorkflowEvent {
            workflow_id: "wf-1".to_string(),
            kind: "phase_started".to_string(),
            payload: json!({"phase_id": "implement"}),
            occurred_at: chrono::Utc::now(),
        });
        broadcaster.emit(WorkflowEvent {
            workflow_id: "wf-1".to_string(),
            kind: "phase_completed".to_string(),
            payload: json!({"phase_id": "implement", "phase_status": "completed"}),
            occurred_at: chrono::Utc::now(),
        });

        let first = read_frame_value(&mut reader).await.unwrap();
        assert_eq!(first.get("method").and_then(|v| v.as_str()), Some("workflow/event"));
        let first_data = first.get("params").and_then(|p| p.get("data")).expect("data");
        assert_eq!(first_data.get("kind").and_then(|v| v.as_str()), Some("phase_started"));
        assert_eq!(first_data.get("workflow_id").and_then(|v| v.as_str()), Some("wf-1"));

        let second = read_frame_value(&mut reader).await.unwrap();
        let second_data = second.get("params").and_then(|p| p.get("data")).expect("data");
        assert_eq!(second_data.get("kind").and_then(|v| v.as_str()), Some("phase_completed"));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn workflow_events_subscription_filters_by_workflow_id() {
    use crate::control::workflow_events::WorkflowEventBroadcaster;
    use animus_control_protocol::types::WorkflowEvent;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let broadcaster = WorkflowEventBroadcaster::new();
        let handle = ControlServer::start_with_socket_and_workflow_events(
            short_test_socket(),
            surface,
            Some(Arc::clone(&broadcaster)),
        )
        .await
        .unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(901, "workflow/events", Some(json!({"workflow_id": "wf-target"})));
        send_frame(&mut write_half, &request).await.unwrap();

        let _ack = read_frame_value(&mut reader).await.unwrap();
        for _ in 0..50 {
            if broadcaster.subscriber_count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        broadcaster.emit(WorkflowEvent {
            workflow_id: "wf-other".to_string(),
            kind: "phase_started".to_string(),
            payload: json!({}),
            occurred_at: chrono::Utc::now(),
        });
        broadcaster.emit(WorkflowEvent {
            workflow_id: "wf-target".to_string(),
            kind: "phase_started".to_string(),
            payload: json!({}),
            occurred_at: chrono::Utc::now(),
        });

        let first = read_frame_value(&mut reader).await.unwrap();
        let data = first.get("params").and_then(|p| p.get("data")).expect("data");
        assert_eq!(data.get("workflow_id").and_then(|v| v.as_str()), Some("wf-target"));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn workflow_events_subscription_filters_by_kinds() {
    use crate::control::workflow_events::WorkflowEventBroadcaster;
    use animus_control_protocol::types::WorkflowEvent;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let broadcaster = WorkflowEventBroadcaster::new();
        let handle = ControlServer::start_with_socket_and_workflow_events(
            short_test_socket(),
            surface,
            Some(Arc::clone(&broadcaster)),
        )
        .await
        .unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(902, "workflow/events", Some(json!({"kinds": ["workflow_completed"]})));
        send_frame(&mut write_half, &request).await.unwrap();
        let _ack = read_frame_value(&mut reader).await.unwrap();

        for _ in 0..50 {
            if broadcaster.subscriber_count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        broadcaster.emit(WorkflowEvent {
            workflow_id: "wf-1".to_string(),
            kind: "phase_started".to_string(),
            payload: json!({}),
            occurred_at: chrono::Utc::now(),
        });
        broadcaster.emit(WorkflowEvent {
            workflow_id: "wf-1".to_string(),
            kind: "workflow_completed".to_string(),
            payload: json!({"final_status": "completed"}),
            occurred_at: chrono::Utc::now(),
        });

        let first = read_frame_value(&mut reader).await.unwrap();
        let data = first.get("params").and_then(|p| p.get("data")).expect("data");
        assert_eq!(data.get("kind").and_then(|v| v.as_str()), Some("workflow_completed"));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn workflow_events_end_to_end_via_upstream_control_client() {
    use crate::control::workflow_events::WorkflowEventBroadcaster;
    use animus_control_protocol::client::ControlClient as UpstreamControlClient;
    use animus_control_protocol::types::{WorkflowEvent, WorkflowEventsRequest};

    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let broadcaster = WorkflowEventBroadcaster::new();
        let handle = ControlServer::start_with_socket_and_workflow_events(
            short_test_socket(),
            surface,
            Some(Arc::clone(&broadcaster)),
        )
        .await
        .unwrap();

        let client = UpstreamControlClient::connect(handle.socket_path()).await.unwrap();
        let mut sub = client.workflow_events(WorkflowEventsRequest::default()).await.unwrap();

        for _ in 0..50 {
            if broadcaster.subscriber_count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let payload = WorkflowEvent {
            workflow_id: "wf-e2e".to_string(),
            kind: "phase_completed".to_string(),
            payload: json!({"phase_id": "implement", "phase_status": "completed"}),
            occurred_at: chrono::Utc::now(),
        };
        broadcaster.emit(payload.clone());

        let received = tokio::time::timeout(Duration::from_secs(5), sub.recv())
            .await
            .expect("client recv timed out")
            .expect("stream closed");
        assert_eq!(received.workflow_id, "wf-e2e");
        assert_eq!(received.kind, "phase_completed");
        assert_eq!(received.payload.get("phase_status").and_then(|v| v.as_str()), Some("completed"));

        drop(sub);
        drop(client);
        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn cancel_request_terminates_subscription_without_closing_socket() {
    use crate::control::workflow_events::WorkflowEventBroadcaster;
    use animus_control_protocol::types::WorkflowEvent;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let broadcaster = WorkflowEventBroadcaster::new();
        let handle = ControlServer::start_with_socket_and_workflow_events(
            short_test_socket(),
            surface,
            Some(Arc::clone(&broadcaster)),
        )
        .await
        .unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(710, "workflow/events", Some(json!({})));
        send_frame(&mut write_half, &request).await.unwrap();
        let _ack = read_frame_value(&mut reader).await.unwrap();

        for _ in 0..50 {
            if broadcaster.subscriber_count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(broadcaster.subscriber_count(), 1);

        broadcaster.emit(WorkflowEvent {
            workflow_id: "wf-cancel".to_string(),
            kind: "phase_started".to_string(),
            payload: json!({}),
            occurred_at: chrono::Utc::now(),
        });
        let first = read_frame_value(&mut reader).await.unwrap();
        assert_eq!(first.get("method").and_then(|v| v.as_str()), Some("workflow/event"));

        let cancel = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: "$/cancelRequest".to_string(),
            params: Some(json!({"id": 710})),
        };
        send_frame(&mut write_half, &cancel).await.unwrap();

        for _ in 0..50 {
            if broadcaster.subscriber_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(broadcaster.subscriber_count(), 0, "subscription must be torn down after $/cancelRequest");

        // Socket stays open: a follow-up unary request still works.
        let ping = RpcRequest::new(711, "daemon/status", None);
        send_frame(&mut write_half, &ping).await.unwrap();
        let ping_resp_value = tokio::time::timeout(Duration::from_secs(5), read_frame_value(&mut reader))
            .await
            .expect("daemon/status reply timed out after cancel")
            .unwrap();
        let ping_resp: RpcResponse = serde_json::from_value(ping_resp_value).unwrap();
        assert_eq!(ping_resp.id, Some(json!(711)));
        assert!(ping_resp.error.is_none(), "socket should still serve unary requests");

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn subscription_closed_notification_fires_on_workflow_completion() {
    use crate::control::workflow_events::WorkflowEventBroadcaster;
    use animus_control_protocol::types::WorkflowEvent;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let broadcaster = WorkflowEventBroadcaster::new();
        let handle = ControlServer::start_with_socket_and_workflow_events(
            short_test_socket(),
            surface,
            Some(Arc::clone(&broadcaster)),
        )
        .await
        .unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(720, "workflow/events", Some(json!({"workflow_id": "wf-done"})));
        send_frame(&mut write_half, &request).await.unwrap();
        let _ack = read_frame_value(&mut reader).await.unwrap();

        for _ in 0..50 {
            if broadcaster.subscriber_count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        broadcaster.emit(WorkflowEvent {
            workflow_id: "wf-done".to_string(),
            kind: "workflow_completed".to_string(),
            payload: json!({"final_status": "completed"}),
            occurred_at: chrono::Utc::now(),
        });

        let first = read_frame_value(&mut reader).await.unwrap();
        assert_eq!(first.get("method").and_then(|v| v.as_str()), Some("workflow/event"));
        let first_data = first.get("params").and_then(|p| p.get("data")).expect("data");
        assert_eq!(first_data.get("kind").and_then(|v| v.as_str()), Some("workflow_completed"));

        let second = read_frame_value(&mut reader).await.unwrap();
        assert_eq!(
            second.get("method").and_then(|v| v.as_str()),
            Some("subscription/closed"),
            "terminal notification must follow workflow_completed"
        );
        let close_params = second.get("params").expect("close params");
        assert_eq!(close_params.get("id"), Some(&json!(720)));

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn subscription_closed_notification_includes_reason_string() {
    use crate::control::workflow_events::WorkflowEventBroadcaster;
    use animus_control_protocol::types::WorkflowEvent;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(TestSurface::new());
        let broadcaster = WorkflowEventBroadcaster::new();
        let handle = ControlServer::start_with_socket_and_workflow_events(
            short_test_socket(),
            surface,
            Some(Arc::clone(&broadcaster)),
        )
        .await
        .unwrap();

        let stream = UnixStream::connect(handle.socket_path()).await.unwrap();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let request = RpcRequest::new(730, "workflow/events", Some(json!({"workflow_id": "wf-fail"})));
        send_frame(&mut write_half, &request).await.unwrap();
        let _ack = read_frame_value(&mut reader).await.unwrap();

        for _ in 0..50 {
            if broadcaster.subscriber_count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        broadcaster.emit(WorkflowEvent {
            workflow_id: "wf-fail".to_string(),
            kind: "workflow_failed".to_string(),
            payload: json!({"final_status": "failed", "error": "boom"}),
            occurred_at: chrono::Utc::now(),
        });

        let _event_frame = read_frame_value(&mut reader).await.unwrap();
        let close_frame = read_frame_value(&mut reader).await.unwrap();
        assert_eq!(close_frame.get("method").and_then(|v| v.as_str()), Some("subscription/closed"));
        let params = close_frame.get("params").expect("params");
        let reason = params.get("reason").and_then(|v| v.as_str()).expect("reason string");
        assert!(reason.contains("wf-fail"), "reason should mention workflow id, got {reason}");
        assert!(reason.contains("workflow_failed"), "reason should mention terminal kind, got {reason}");

        handle.shutdown().await.unwrap();
    })
    .await
    .expect("test timed out");
}
