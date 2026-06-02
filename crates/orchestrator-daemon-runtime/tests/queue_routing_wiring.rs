//! Integration test: when a `QueueRouting` handle is wired into the
//! daemon's `InProcessSurface`, `queue/list` over the control socket
//! returns the plugin's response instead of `ControlError::NotSupported`.
//!
//! This pins the v0.5.1 round-3 fold-in for the daemon control socket
//! plugin routing for queue verbs (Task B). The mock `QueueRouting`
//! stands in for the installed queue plugin — it captures each method
//! invocation and emits a canned `QueueListResponse`. The wire round-trip
//! goes through the real `ControlServer` + Unix socket so we exercise
//! the same dispatch path production callers see.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use animus_control_protocol::client::ControlClient;
use animus_control_protocol::types::{
    QueueDropRequest, QueueEnqueueRequest, QueueEntry, QueueEntryStatus, QueueHoldRequest, QueueListRequest,
    QueueListResponse, QueueReleaseRequest, QueueReorderRequest, QueueStats, Unit,
};
use animus_control_protocol::ControlError;
use animus_subject_protocol_wire::SubjectId;
use async_trait::async_trait;
use chrono::Utc;
use orchestrator_daemon_runtime::control::{ControlServer, InProcessSurface, QueueRouting};
use tokio::sync::Mutex;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn short_test_socket() -> PathBuf {
    let unique = format!("aq-{}-{}.sock", std::process::id(), &uuid::Uuid::new_v4().simple().to_string()[..8]);
    PathBuf::from("/tmp").join(unique)
}

#[derive(Default)]
struct MockQueuePlugin {
    seen_methods: Arc<Mutex<Vec<String>>>,
}

impl MockQueuePlugin {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        (Self { seen_methods: seen.clone() }, seen)
    }

    async fn record(&self, method: &str) {
        self.seen_methods.lock().await.push(method.to_string());
    }
}

#[async_trait]
impl QueueRouting for MockQueuePlugin {
    async fn queue_list(&self, _request: QueueListRequest) -> Result<QueueListResponse, ControlError> {
        self.record("queue/list").await;
        Ok(QueueListResponse {
            entries: vec![QueueEntry {
                id: "ent_mock_1".to_string(),
                subject_id: SubjectId::new("TASK-MOCK-1"),
                status: QueueEntryStatus::Ready,
                priority: 3,
                enqueued_at: Utc::now(),
                hold_reason: None,
            }],
            next_cursor: None,
        })
    }

    async fn queue_enqueue(&self, _request: QueueEnqueueRequest) -> Result<QueueEntry, ControlError> {
        self.record("queue/enqueue").await;
        Ok(QueueEntry {
            id: "ent_mock_new".to_string(),
            subject_id: SubjectId::new("TASK-MOCK-NEW"),
            status: QueueEntryStatus::Ready,
            priority: 2,
            enqueued_at: Utc::now(),
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
        Ok(QueueStats { ready: 4, held: 1, in_flight: 2, done_recent: 0, dropped_recent: 0 })
    }
}

#[tokio::test]
async fn queue_list_over_control_socket_routes_through_plugin() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (plugin, seen) = MockQueuePlugin::new();
        let routing: Arc<dyn QueueRouting> = Arc::new(plugin);
        let surface =
            InProcessSurface::builder(PathBuf::from("/tmp/queue-routing-wiring")).queue_routing(routing).build();
        let surface_arc: Arc<dyn animus_control_protocol::control_trait::ControlSurface> = Arc::new(surface);
        let socket_path = short_test_socket();
        let handle =
            ControlServer::start_with_socket(socket_path.clone(), surface_arc).await.expect("start control server");

        let client = ControlClient::connect(&socket_path).await.expect("client connect");
        let response = client.queue_list(QueueListRequest::default()).await.expect("queue/list call");

        assert_eq!(response.entries.len(), 1, "mock plugin response should round-trip through the wire");
        let entry = &response.entries[0];
        assert_eq!(entry.id, "ent_mock_1");
        assert_eq!(entry.priority, 3);
        assert!(matches!(entry.status, QueueEntryStatus::Ready));

        let calls = seen.lock().await.clone();
        assert_eq!(calls, vec!["queue/list".to_string()], "queue plugin should be invoked exactly once");

        handle.shutdown().await.expect("shutdown");
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn queue_list_without_routing_returns_not_supported() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface = InProcessSurface::builder(PathBuf::from("/tmp/queue-routing-wiring-missing")).build();
        let surface_arc: Arc<dyn animus_control_protocol::control_trait::ControlSurface> = Arc::new(surface);
        let socket_path = short_test_socket();
        let handle =
            ControlServer::start_with_socket(socket_path.clone(), surface_arc).await.expect("start control server");

        let client = ControlClient::connect(&socket_path).await.expect("client connect");
        let result = client.queue_list(QueueListRequest::default()).await;

        let err = result.expect_err("queue/list should fail when no routing is wired");
        let message = format!("{err:#}");
        assert!(
            message.to_lowercase().contains("not")
                && (message.to_lowercase().contains("supported") || message.to_lowercase().contains("configured")),
            "expected NotSupported-like error when routing is absent, got: {message}"
        );

        handle.shutdown().await.expect("shutdown");
    })
    .await
    .expect("test timed out");
}
