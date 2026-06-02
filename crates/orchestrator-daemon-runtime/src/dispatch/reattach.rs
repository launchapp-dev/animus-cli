//! Daemon-side reattach client for v0.5.1 P2 #6.2 round-3.
//!
//! Pairs with [`animus_runtime_shared::reattach::ReattachListenerEmitter`]: the
//! runner binds a `UnixListener` at a deterministic path advertised via the
//! spawn record's `stdio_socket_path`. On daemon startup, after the orphan
//! scan reports each live orphan, this module opens a `UnixStream` to that
//! path, spawns a reader task that translates newline-JSON
//! [`animus_runtime_shared::WireWorkflowEvent`] frames into wire
//! [`animus_control_protocol::types::WorkflowEvent`] and forwards them into
//! the daemon's [`crate::control::WorkflowEventBroadcaster`].
//!
//! What round-3-fold-in closes (round-3 originally left this as v0.6):
//! - [`replay_decision_log_gap`]: reconstruct events that the runner wrote
//!   during the daemon gap by tailing `decisions.jsonl` and translating
//!   selected [`agent_runner::recording::DecisionEvent`] kinds into
//!   `WorkflowEvent`. Race-safe: the reader uses the writer-tolerant
//!   [`agent_runner::recording::tail::DecisionTailReader`].
//!
//! Scope honesty for this fold-in:
//! - The reader task lives until the runner closes the socket OR the
//!   daemon shuts down. There is no built-in cancellation hook today; if a
//!   reattached orphan completes mid-stream, the JoinHandle exits cleanly
//!   on EOF.
//! - No throttling, no retry. A failed connect emits
//!   [`crate::DaemonRunEvent::OrphanAgentReattachFailed`] and the orphan
//!   record stays on disk for the operator to inspect or for the next
//!   daemon start to retry.
//! - Gap reconstruction is offered as a primitive; the orphan-scan path
//!   does not yet auto-discover per-agent decision-log files because the
//!   workflow-runner spawn record does not (yet) record decision-log paths
//!   per child agent run. Callers that know the path can drive the
//!   primitive directly; auto-discovery is the v0.6 follow-up.
//! - Windows: gated `#[cfg(unix)]`. Windows daemons skip reattach entirely
//!   and the orphan record stays on disk.

#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use tokio::task::JoinHandle;

#[cfg(unix)]
use crate::control::WorkflowEventBroadcaster;

#[cfg(unix)]
#[allow(dead_code)]
pub struct ReattachConnection {
    socket_path: std::path::PathBuf,
    reader_task: JoinHandle<()>,
}

#[cfg(unix)]
impl ReattachConnection {
    #[allow(dead_code)]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

/// Try to connect to a runner's reattach listener and start forwarding its
/// events into `broadcaster`. Returns `Ok(connection)` on a successful
/// connect; the caller is responsible for retaining the
/// [`ReattachConnection`] so the reader task isn't dropped (dropping the
/// JoinHandle merely detaches the task on tokio, but we keep the handle
/// for testability and future graceful-shutdown hooks).
#[cfg(unix)]
pub fn try_reattach(
    socket_path: &Path,
    broadcaster: Arc<WorkflowEventBroadcaster>,
) -> std::io::Result<ReattachConnection> {
    use tokio::net::UnixStream;
    // Connect synchronously using std to get a clean, blocking error if the
    // runner isn't listening yet (eg the daemon restarted faster than the
    // runner bound its listener). Promote to a tokio stream for the reader
    // task so we don't block a runtime worker.
    let std_stream = std::os::unix::net::UnixStream::connect(socket_path)?;
    std_stream.set_nonblocking(true)?;
    let stream = UnixStream::from_std(std_stream)?;
    let socket_owned = socket_path.to_path_buf();
    let reader_socket = socket_owned.clone();
    let reader_task = tokio::spawn(reader_loop(stream, broadcaster, reader_socket));
    Ok(ReattachConnection { socket_path: socket_owned, reader_task })
}

#[cfg(unix)]
async fn reader_loop(
    stream: tokio::net::UnixStream,
    broadcaster: Arc<WorkflowEventBroadcaster>,
    socket: std::path::PathBuf,
) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => forward_line(&broadcaster, &line, &socket),
            Ok(None) => {
                tracing::debug!(
                    target: "animus.runtime.reattach",
                    socket = %socket.display(),
                    "reattach reader EOF (runner closed)"
                );
                return;
            }
            Err(error) => {
                tracing::debug!(
                    target: "animus.runtime.reattach",
                    socket = %socket.display(),
                    %error,
                    "reattach reader stream error; exiting"
                );
                return;
            }
        }
    }
}

/// Outcome of a single [`replay_decision_log_gap`] sweep.
#[derive(Debug, Clone)]
pub struct GapReplayReport {
    /// How many `WorkflowEvent`s were emitted into the broadcaster.
    pub emitted: usize,
    /// The byte offset the reader now sits at; pass this in on a follow-up
    /// call to read only newer events.
    pub next_offset: u64,
    /// `true` when the tail reader observed a writer-in-progress partial
    /// line at the end of the file. Pure hint — a subsequent call will
    /// either yield more events or report the same offset.
    pub partial_tail: bool,
}

/// Reconstruct the daemon's view of `decisions.jsonl` events that landed
/// during a daemon-restart gap. Reads from `start_offset` to the current
/// end-of-file (race-safe; partial trailing lines are held back) and
/// emits a synthetic [`animus_control_protocol::types::WorkflowEvent`]
/// for each recorded [`agent_runner::recording::DecisionEvent`] kind that
/// can be lifted into the workflow-event surface.
///
/// Per-agent decision events do NOT map 1:1 to workflow-terminal events
/// because a single workflow may run many agents (one per phase); a phase
/// finish must not auto-close workflow subscribers. The primitive lifts
/// agent-level events into namespaced, NON-terminal workflow-event kinds:
/// - `Error` → `agent_error` with the error message in payload
/// - `Finished { exit_code: Some(0) }` → `agent_finished` with exit_code=0
/// - `Finished { exit_code: Some(nonzero) }` / `None` → `agent_error`
///   with the exit code in payload (still NOT a terminal workflow event;
///   the workflow runner emits the true `workflow_failed` on phase exit)
///
/// Callers that need to map gap-replayed agent terminals into actual
/// workflow-terminal events must do so with phase + workflow context the
/// daemon already tracks. The primitive does not try to second-guess that.
///
/// Other recording events (Prompt, ResponseChunk, ToolCall, ToolResult,
/// Metadata) are logged to the decision log but NOT promoted to the
/// workflow-event channel; subscribers consume them via the recording
/// surface directly when needed.
pub fn replay_decision_log_gap(
    decisions_path: &std::path::Path,
    workflow_id: &str,
    start_offset: u64,
    broadcaster: &dyn WorkflowEventBroadcasterLike,
) -> anyhow::Result<GapReplayReport> {
    use agent_runner::recording::tail::DecisionTailReader;
    use agent_runner::recording::DecisionEvent;
    let mut reader = DecisionTailReader::open(decisions_path, start_offset);
    let batch = reader.read_new()?;
    let mut emitted = 0usize;
    for event in &batch.events {
        // Codex round-2 P2: use non-terminal agent_* kinds. The
        // WorkflowEventBroadcaster treats workflow_completed / workflow_failed
        // as terminal frames that auto-close subscribers; promoting a single
        // agent's finish to a workflow-terminal event would prematurely
        // close subscribers for multi-phase workflows. The TRUE workflow
        // terminal event arrives from the workflow runner's emitter.
        let lifted = match event {
            DecisionEvent::Error { message, .. } => Some(animus_control_protocol::types::WorkflowEvent {
                workflow_id: workflow_id.to_string(),
                kind: "agent_error".to_string(),
                payload: serde_json::json!({"error": message, "source": "decision_log_gap"}),
                occurred_at: chrono::Utc::now(),
            }),
            DecisionEvent::Finished { exit_code, .. } => {
                let kind = if matches!(exit_code, Some(0)) { "agent_finished" } else { "agent_error" };
                Some(animus_control_protocol::types::WorkflowEvent {
                    workflow_id: workflow_id.to_string(),
                    kind: kind.to_string(),
                    payload: serde_json::json!({"exit_code": exit_code, "source": "decision_log_gap"}),
                    occurred_at: chrono::Utc::now(),
                })
            }
            _ => None,
        };
        if let Some(wf_event) = lifted {
            broadcaster.emit(wf_event);
            emitted += 1;
        }
    }
    Ok(GapReplayReport { emitted, next_offset: batch.offset, partial_tail: batch.partial_tail })
}

/// Trait-erased emitter so [`replay_decision_log_gap`] can be exercised
/// against a test double. Production callers pass a
/// [`WorkflowEventBroadcaster`] wrapped in [`BroadcasterEmitter`].
#[cfg(unix)]
pub trait WorkflowEventBroadcasterLike: Send + Sync {
    fn emit(&self, event: animus_control_protocol::types::WorkflowEvent);
}

#[cfg(not(unix))]
pub trait WorkflowEventBroadcasterLike {
    fn emit(&self, event: animus_control_protocol::types::WorkflowEvent);
}

#[cfg(unix)]
pub struct BroadcasterEmitter {
    pub inner: Arc<WorkflowEventBroadcaster>,
}

#[cfg(unix)]
impl WorkflowEventBroadcasterLike for BroadcasterEmitter {
    fn emit(&self, event: animus_control_protocol::types::WorkflowEvent) {
        self.inner.emit(event);
    }
}

#[cfg(unix)]
fn forward_line(broadcaster: &WorkflowEventBroadcaster, line: &str, socket: &Path) {
    let wire: animus_runtime_shared::workflow_event_emitter::WireWorkflowEvent = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                target: "animus.runtime.reattach",
                socket = %socket.display(),
                %error,
                "discarding malformed reattach event frame"
            );
            return;
        }
    };
    let event = animus_control_protocol::types::WorkflowEvent {
        workflow_id: wire.workflow_id,
        kind: wire.kind,
        payload: wire.payload,
        occurred_at: wire.occurred_at,
    };
    broadcaster.emit(event);
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::control::WorkflowEventBroadcaster;
    use std::io::Write;
    use std::os::unix::net::UnixListener as StdListener;
    use std::path::PathBuf;
    use std::thread;
    use tempfile::TempDir;

    fn pair() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reattach.sock");
        (dir, path)
    }

    #[tokio::test]
    async fn try_reattach_fails_on_missing_socket() {
        let (_dir, path) = pair();
        let broadcaster = WorkflowEventBroadcaster::new();
        let err = try_reattach(&path, broadcaster).err().expect("connect must fail when socket absent");
        assert!(err.kind() == std::io::ErrorKind::NotFound || err.kind() == std::io::ErrorKind::ConnectionRefused);
    }

    #[tokio::test]
    async fn forwarded_event_reaches_broadcaster_subscriber() {
        use crate::control::WorkflowEventFilter;
        let (_dir, path) = pair();
        let listener = StdListener::bind(&path).expect("bind listener");
        let broadcaster = WorkflowEventBroadcaster::new();
        let (_id, mut rx) = broadcaster.subscribe(WorkflowEventFilter::default());

        // Accept the daemon's reattach connect on a background thread so
        // try_reattach can complete.
        let writer_handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            // Send one wire event frame.
            let frame = serde_json::json!({
                "workflow_id": "wf-reattach-1",
                "kind": "phase_started",
                "payload": {"phase": "implementation"},
                "occurred_at": chrono::Utc::now()
            });
            let mut line = serde_json::to_string(&frame).unwrap();
            line.push('\n');
            stream.write_all(line.as_bytes()).unwrap();
            stream.flush().unwrap();
            // Keep the stream alive briefly so the reader can drain.
            std::thread::sleep(std::time::Duration::from_millis(100));
        });

        let _conn = try_reattach(&path, broadcaster.clone()).expect("reattach connect");

        let item = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("subscriber timeout")
            .expect("subscriber channel closed");
        match item {
            crate::control::SubscriberItem::Event(event) => {
                assert_eq!(event.workflow_id, "wf-reattach-1");
                assert_eq!(event.kind, "phase_started");
            }
            crate::control::SubscriberItem::Closed { reason } => {
                panic!("unexpected close item: {reason}");
            }
        }

        writer_handle.join().unwrap();
    }

    #[test]
    fn replay_decision_log_gap_lifts_finished_and_error_only() {
        use agent_runner::recording::{DecisionEvent, Durability, Recorder};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        let recorder = Recorder::create_with_durability(&path, Durability::FsyncPerEvent).expect("recorder");
        recorder.record(&DecisionEvent::prompt("m", "p", None)).unwrap();
        recorder.record(&DecisionEvent::response_chunk("stdout", "noise")).unwrap();
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(recorder);

        struct Collector {
            events: std::sync::Mutex<Vec<animus_control_protocol::types::WorkflowEvent>>,
        }
        impl WorkflowEventBroadcasterLike for Collector {
            fn emit(&self, event: animus_control_protocol::types::WorkflowEvent) {
                self.events.lock().unwrap().push(event);
            }
        }
        let collector = Collector { events: std::sync::Mutex::new(Vec::new()) };
        let report = replay_decision_log_gap(&path, "wf-gap-1", 0, &collector).expect("gap replay");
        assert_eq!(report.emitted, 1, "only finished/error lift to workflow_event");
        assert!(!report.partial_tail);
        let events = collector.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].workflow_id, "wf-gap-1");
        assert_eq!(events[0].kind, "agent_finished");
        assert_eq!(events[0].payload["source"], "decision_log_gap");
    }

    #[test]
    fn replay_decision_log_gap_resumes_from_offset() {
        use agent_runner::recording::{DecisionEvent, Durability, Recorder};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        let recorder = Recorder::create_with_durability(&path, Durability::FsyncPerEvent).expect("recorder");
        recorder.record(&DecisionEvent::prompt("m", "first", None)).unwrap();
        recorder.record(&DecisionEvent::error("first-error")).unwrap();
        drop(recorder);

        struct Collector(std::sync::Mutex<Vec<animus_control_protocol::types::WorkflowEvent>>);
        impl WorkflowEventBroadcasterLike for Collector {
            fn emit(&self, event: animus_control_protocol::types::WorkflowEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let collector = Collector(std::sync::Mutex::new(Vec::new()));
        let r1 = replay_decision_log_gap(&path, "wf-gap-2", 0, &collector).expect("first sweep");
        assert_eq!(r1.emitted, 1);

        // Runner appends MORE events during a simulated gap. The daemon
        // resumes from `r1.next_offset` and sees only post-gap events.
        let recorder2 = Recorder::create_with_durability(&path, Durability::FsyncPerEvent).expect("recorder 2");
        recorder2.record(&DecisionEvent::response_chunk("stdout", "ignored")).unwrap();
        recorder2.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(recorder2);
        let r2 = replay_decision_log_gap(&path, "wf-gap-2", r1.next_offset, &collector).expect("second sweep");
        assert_eq!(r2.emitted, 1, "only the new finished event");
        let all = collector.0.lock().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].kind, "agent_error");
        assert_eq!(all[1].kind, "agent_finished");
        assert_eq!(all[1].payload["exit_code"], 0);
    }

    #[test]
    fn replay_decision_log_gap_promotes_nonzero_exit_to_agent_error() {
        use agent_runner::recording::{DecisionEvent, Durability, Recorder};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        let recorder = Recorder::create_with_durability(&path, Durability::FsyncPerEvent).expect("recorder");
        recorder.record(&DecisionEvent::finished(Some(2))).unwrap();
        drop(recorder);
        struct Collector(std::sync::Mutex<Vec<animus_control_protocol::types::WorkflowEvent>>);
        impl WorkflowEventBroadcasterLike for Collector {
            fn emit(&self, event: animus_control_protocol::types::WorkflowEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let collector = Collector(std::sync::Mutex::new(Vec::new()));
        let r = replay_decision_log_gap(&path, "wf-nonzero", 0, &collector).expect("sweep");
        assert_eq!(r.emitted, 1);
        let events = collector.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "agent_error");
        assert_eq!(events[0].payload["exit_code"], 2);
    }

    #[test]
    fn replay_decision_log_gap_promotes_missing_exit_to_agent_error() {
        use agent_runner::recording::{DecisionEvent, Durability, Recorder};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        let recorder = Recorder::create_with_durability(&path, Durability::FsyncPerEvent).expect("recorder");
        recorder.record(&DecisionEvent::finished(None)).unwrap();
        drop(recorder);
        struct Collector(std::sync::Mutex<Vec<animus_control_protocol::types::WorkflowEvent>>);
        impl WorkflowEventBroadcasterLike for Collector {
            fn emit(&self, event: animus_control_protocol::types::WorkflowEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let collector = Collector(std::sync::Mutex::new(Vec::new()));
        let r = replay_decision_log_gap(&path, "wf-missing", 0, &collector).expect("sweep");
        assert_eq!(r.emitted, 1);
        let events = collector.0.lock().unwrap();
        assert_eq!(events[0].kind, "agent_error");
    }

    #[test]
    fn replay_decision_log_gap_writer_reader_race_yields_partial_tail_hint() {
        use agent_runner::recording::{DecisionEvent, Durability, Recorder};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("decisions.jsonl");
        let recorder = Recorder::create_with_durability(&path, Durability::FsyncPerEvent).expect("recorder");
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        // Drop drains the writer; then we manually append a partial bytes
        // string with no terminating newline (simulating the writer being
        // mid-append when the daemon reads).
        drop(recorder);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(br#"{"kind":"finished","timestamp_ms":2,"exit_code":1"#).unwrap();
        }
        struct NoopEmitter;
        impl WorkflowEventBroadcasterLike for NoopEmitter {
            fn emit(&self, _event: animus_control_protocol::types::WorkflowEvent) {}
        }
        let report = replay_decision_log_gap(&path, "wf-race", 0, &NoopEmitter).expect("race sweep");
        assert!(report.partial_tail, "partial-tail must be reported as a hint");
        assert_eq!(report.emitted, 1, "the complete finished event was lifted; partial line held back");
    }
}
