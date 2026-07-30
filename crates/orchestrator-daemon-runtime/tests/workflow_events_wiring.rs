//! End-to-end integration: the daemon-runtime broadcaster +
//! [`BroadcastWorkflowEventEmitter`] + control-server stack delivers
//! workflow lifecycle events from an in-process [`WorkflowEventEmitter`]
//! call site through to a `workflow/events` subscriber on the control
//! socket.
//!
//! This is the "Part A" production-readiness gap test: it pins the wiring
//! that `run_daemon` constructs at startup. It does NOT spin up a full
//! daemon (no scheduler, no plugin host, no real workflow) — that would
//! require disk fixtures and a real workflow YAML. Instead it stands up
//! the same wire path (ControlServer + broadcaster + emitter adapter) the
//! daemon installs and exercises every event kind a workflow run emits at
//! phase boundaries.

#![cfg(unix)]

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use animus_control_protocol::client::ControlClient as UpstreamControlClient;
use animus_control_protocol::control_trait::{ControlSurface, DaemonEventStream, DaemonLogStream, SubjectWatchStream};
use animus_control_protocol::types::{
    AgentCancelRequest, AgentRunRequest, AgentRunResult, AgentStatus, AgentStatusRequest, CodingFleetStatusResponse,
    DaemonAgentsResponse, DaemonEventsRequest, DaemonHealthResponse, DaemonHealthStatus, DaemonLogsRequest,
    DaemonStatusResponse, PluginBrowseRequest, PluginCallRequest, PluginCallResponse, PluginInfo, PluginInfoRequest,
    PluginInstallRequest, PluginInstallResponse, PluginListRequest, PluginListResponse, PluginPingRequest,
    PluginPingResponse, PluginSearchRequest, PluginSearchResponse, PluginUninstallRequest, PluginUpdateRequest,
    PluginUpdateResponse, ProjectInfo, ProjectInitRequest, ProjectSetupRequest, ProjectStatusResponse,
    QueueDropRequest, QueueEnqueueRequest, QueueEntry, QueueHoldRequest, QueueListRequest, QueueListResponse,
    QueueReleaseRequest, QueueReorderRequest, QueueStats, SubjectCreateRequest, SubjectGetRequest, SubjectListRequest,
    SubjectListResponse, SubjectNextRequest, SubjectNextResponse, SubjectStatusRequest, SubjectUpdateRequest,
    SubjectWatchRequest, Unit, WorkflowCancelRequest, WorkflowEventsRequest, WorkflowExecuteRequest,
    WorkflowGetRequest, WorkflowListRequest, WorkflowListResponse, WorkflowPauseRequest, WorkflowResumeRequest,
    WorkflowRun, WorkflowRunRequest, WorkflowRunStart,
};
use animus_control_protocol::ControlError;
use animus_runtime_shared::workflow_event_emitter::{
    RuntimeWorkflowEvent, RuntimeWorkflowEventKind, WorkflowEventEmitter,
};
use animus_subject_protocol_wire::{Subject, SubjectChangedEvent};
use async_trait::async_trait;
use chrono::Utc;
use futures_util::stream;
use orchestrator_daemon_runtime::control::{BroadcastWorkflowEventEmitter, ControlServer, WorkflowEventBroadcaster};
use serde_json::json;

const TEST_TIMEOUT: Duration = Duration::from_secs(20);

fn short_test_socket() -> PathBuf {
    let unique = format!("aw-{}-{}.sock", std::process::id(), &uuid::Uuid::new_v4().simple().to_string()[..8]);
    PathBuf::from("/tmp").join(unique)
}

#[derive(Default)]
struct StubSurface;

#[async_trait]
impl ControlSurface for StubSurface {
    async fn subject_list(&self, _request: SubjectListRequest) -> Result<SubjectListResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn subject_get(&self, _request: SubjectGetRequest) -> Result<Subject, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn subject_create(&self, _request: SubjectCreateRequest) -> Result<Subject, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn subject_update(&self, _request: SubjectUpdateRequest) -> Result<Subject, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn subject_next(&self, _request: SubjectNextRequest) -> Result<SubjectNextResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn subject_status(&self, _request: SubjectStatusRequest) -> Result<Subject, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn subject_watch(&self, _request: SubjectWatchRequest) -> Result<SubjectWatchStream, ControlError> {
        Ok(Box::pin(stream::empty::<SubjectChangedEvent>())
            as Pin<Box<dyn futures_core::Stream<Item = SubjectChangedEvent> + Send>>)
    }
    async fn plugin_list(&self, _request: PluginListRequest) -> Result<PluginListResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn plugin_info(&self, _request: PluginInfoRequest) -> Result<PluginInfo, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn plugin_install(&self, _request: PluginInstallRequest) -> Result<PluginInstallResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn plugin_uninstall(&self, _request: PluginUninstallRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn plugin_ping(&self, _request: PluginPingRequest) -> Result<PluginPingResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn plugin_call(&self, _request: PluginCallRequest) -> Result<PluginCallResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn plugin_search(&self, _request: PluginSearchRequest) -> Result<PluginSearchResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn plugin_browse(&self, _request: PluginBrowseRequest) -> Result<PluginSearchResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn plugin_update(&self, _request: PluginUpdateRequest) -> Result<PluginUpdateResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn daemon_status(&self) -> Result<DaemonStatusResponse, ControlError> {
        Ok(DaemonStatusResponse {
            running: true,
            pid: Some(std::process::id()),
            uptime_seconds: Some(0),
            version: Some("test".into()),
            project_root: Some(PathBuf::from("/tmp")),
            log_path: None,
        })
    }
    async fn daemon_health(&self) -> Result<DaemonHealthResponse, ControlError> {
        Ok(DaemonHealthResponse { status: DaemonHealthStatus::Healthy, plugins: vec![], last_error: None })
    }
    async fn daemon_start(&self) -> Result<Unit, ControlError> {
        Ok(Unit::default())
    }
    async fn daemon_stop(&self) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn daemon_restart(&self) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn daemon_agents(&self) -> Result<DaemonAgentsResponse, ControlError> {
        Ok(DaemonAgentsResponse { agents: vec![] })
    }
    async fn fleet_status(&self) -> Result<CodingFleetStatusResponse, ControlError> {
        Ok(CodingFleetStatusResponse {
            capacity: 5,
            active: 0,
            available: 5,
            owner_id: "stub".to_string(),
            reservations: vec![],
        })
    }
    async fn daemon_events(&self, _request: DaemonEventsRequest) -> Result<DaemonEventStream, ControlError> {
        Ok(Box::pin(stream::empty()))
    }
    async fn daemon_logs(&self, _request: DaemonLogsRequest) -> Result<DaemonLogStream, ControlError> {
        Ok(Box::pin(stream::empty()))
    }
    async fn workflow_list(&self, _request: WorkflowListRequest) -> Result<WorkflowListResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn workflow_get(&self, _request: WorkflowGetRequest) -> Result<WorkflowRun, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn workflow_run(&self, _request: WorkflowRunRequest) -> Result<WorkflowRunStart, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn workflow_execute(&self, _request: WorkflowExecuteRequest) -> Result<WorkflowRunStart, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn workflow_pause(&self, _request: WorkflowPauseRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn workflow_resume(&self, _request: WorkflowResumeRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn workflow_cancel(&self, _request: WorkflowCancelRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn agent_run(&self, _request: AgentRunRequest) -> Result<AgentRunResult, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn agent_status(&self, _request: AgentStatusRequest) -> Result<AgentStatus, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn agent_cancel(&self, _request: AgentCancelRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn queue_list(&self, _request: QueueListRequest) -> Result<QueueListResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn queue_enqueue(&self, _request: QueueEnqueueRequest) -> Result<QueueEntry, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn queue_drop(&self, _request: QueueDropRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn queue_hold(&self, _request: QueueHoldRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn queue_release(&self, _request: QueueReleaseRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn queue_reorder(&self, _request: QueueReorderRequest) -> Result<Unit, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn queue_stats(&self) -> Result<QueueStats, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn project_init(&self, _request: ProjectInitRequest) -> Result<ProjectInfo, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn project_setup(&self, _request: ProjectSetupRequest) -> Result<ProjectInfo, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
    async fn project_status(&self) -> Result<ProjectStatusResponse, ControlError> {
        Err(ControlError::NotSupported("stub".into()))
    }
}

/// Drive a sequence of [`RuntimeWorkflowEvent`]s through the
/// [`BroadcastWorkflowEventEmitter`] adapter and verify each one arrives at
/// an upstream [`UpstreamControlClient::workflow_events`] subscription with
/// the workflow_id, kind, and payload preserved.
#[tokio::test]
async fn broadcast_emitter_delivers_runtime_events_to_control_client() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let surface: Arc<dyn ControlSurface> = Arc::new(StubSurface);
        let broadcaster = WorkflowEventBroadcaster::new();
        let handle = ControlServer::start_with_socket_and_workflow_events(
            short_test_socket(),
            surface,
            Some(Arc::clone(&broadcaster)),
        )
        .await
        .expect("control server start");

        let emitter = BroadcastWorkflowEventEmitter::new(Arc::clone(&broadcaster));

        let client = UpstreamControlClient::connect(handle.socket_path()).await.expect("client connect");
        let mut sub = client
            .workflow_events(WorkflowEventsRequest { workflow_id: Some("wf-emit".into()), kinds: None })
            .await
            .expect("subscribe");

        for _ in 0..50 {
            if broadcaster.subscriber_count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(broadcaster.subscriber_count(), 1);

        let kinds = [
            RuntimeWorkflowEventKind::PhaseStarted,
            RuntimeWorkflowEventKind::PhaseCompleted,
            RuntimeWorkflowEventKind::PhaseStarted,
            RuntimeWorkflowEventKind::PhaseCompleted,
            RuntimeWorkflowEventKind::WorkflowCompleted,
        ];
        for (idx, kind) in kinds.iter().enumerate() {
            emitter.emit(RuntimeWorkflowEvent {
                workflow_id: "wf-emit".to_string(),
                kind: *kind,
                payload: json!({"step": idx}),
                occurred_at: Utc::now(),
            });
        }

        let mut received: Vec<(String, serde_json::Value)> = Vec::new();
        for _ in 0..kinds.len() {
            let event = tokio::time::timeout(Duration::from_secs(5), sub.recv())
                .await
                .expect("recv timed out")
                .expect("stream closed early");
            received.push((event.kind, event.payload));
        }

        assert_eq!(
            received.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["phase_started", "phase_completed", "phase_started", "phase_completed", "workflow_completed"]
        );
        for (idx, (_, payload)) in received.iter().enumerate() {
            assert_eq!(payload.get("step").and_then(|v| v.as_i64()), Some(idx as i64));
        }

        drop(sub);
        drop(client);
        handle.shutdown().await.expect("shutdown");
    })
    .await
    .expect("test timed out");
}

/// Verify the daemon-runtime [`MetricsObserver`] writes
/// `plugin_request_duration_seconds` samples through the global metrics
/// registry. This is the Part B production-readiness gap test: it confirms
/// the histogram surface is wired correctly without standing up a real
/// `PluginSessionBackend` (which would require a real plugin binary).
#[test]
fn metrics_observer_records_plugin_request_duration_samples() {
    use orchestrator_daemon_runtime::metrics::{labeled, metrics_dispatch_observer, snapshot};

    let observer = metrics_dispatch_observer();
    let key = labeled("plugin_request_duration_seconds", &[("plugin", "wiring-test-plugin"), ("method", "agent/run")]);

    let before = snapshot().histograms.get(&key).map(|h| h.count).unwrap_or(0);

    for elapsed_ms in [3u64, 12, 250] {
        observer.observe_duration("wiring-test-plugin", "agent/run", Duration::from_millis(elapsed_ms));
    }

    let after = snapshot().histograms.get(&key).cloned().expect("histogram registered");
    assert_eq!(after.count, before + 3);
    assert!(after.sum_seconds > 0.0);
}
