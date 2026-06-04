//! Daemon-side reattach client.
//!
//! Pairs with [`animus_runtime_shared::reattach::ReattachListenerEmitter`]: the
//! runner binds a [`interprocess::local_socket::Listener`] at a deterministic
//! identifier advertised via the spawn record's `stdio_socket_path`. On
//! daemon startup, after the orphan scan reports each live orphan, this
//! module opens an [`interprocess::local_socket::tokio::Stream`] to that
//! identifier, spawns a reader task that translates newline-JSON
//! [`animus_runtime_shared::WireWorkflowEvent`] frames into wire
//! [`animus_control_protocol::types::WorkflowEvent`] and forwards them into
//! the daemon's [`crate::control::WorkflowEventBroadcaster`].
//!
//! v0.5.1 BETA fold-in (item 7): the previous `cfg(unix)`-only path was
//! collapsed into a cross-platform implementation on top of
//! [`interprocess::local_socket`]. Windows daemons now reattach via named
//! pipes; Unix daemons continue to use Unix domain sockets — both go
//! through the SAME `try_reattach` entrypoint and the SAME wire format.
//!
//! What round-3-fold-in closed (round-3 originally left this as v0.6):
//! - [`replay_decision_log_gap`]: reconstruct events that the runner wrote
//!   during the daemon gap by tailing `decisions.jsonl` and translating
//!   selected [`animus_runtime_shared::recording::DecisionEvent`] kinds into
//!   `WorkflowEvent`. Race-safe: the reader uses the writer-tolerant
//!   [`animus_runtime_shared::recording::tail::DecisionTailReader`].
//!
//! v0.5.1 BETA fold-in (item 2):
//! - [`replay_gap_from_spawn_record`]: drives the gap-replay primitive
//!   straight off an [`super::agent_record::AgentSpawnRecord`] —
//!   `decisions_jsonl_path` field plus `last_consumed_offset` persisted on
//!   the record so a daemon restart never double-emits an event it
//!   already broadcast.
//!
//! Scope honesty:
//! - The reader task lives until the runner closes the socket OR the
//!   daemon shuts down. There is no built-in cancellation hook today; if a
//!   reattached orphan completes mid-stream, the JoinHandle exits cleanly
//!   on EOF.
//! - No throttling, no retry. A failed connect emits
//!   [`crate::DaemonRunEvent::OrphanAgentReattachFailed`] and the orphan
//!   record stays on disk for the operator to inspect or for the next
//!   daemon start to retry.

use std::sync::Arc;
use std::thread;

use crate::control::WorkflowEventBroadcaster;

#[allow(dead_code)]
pub struct ReattachConnection {
    socket_path: String,
    reader_thread: Option<thread::JoinHandle<()>>,
}

impl ReattachConnection {
    #[allow(dead_code)]
    pub fn socket_path(&self) -> &str {
        &self.socket_path
    }
}

/// Try to connect to a runner's reattach listener and start forwarding its
/// events into `broadcaster`. Returns `Ok(connection)` on a successful
/// connect; the caller is responsible for retaining the
/// [`ReattachConnection`] so the reader thread isn't dropped (dropping
/// the JoinHandle merely detaches the thread, but we keep the handle for
/// testability and future graceful-shutdown hooks).
///
/// `socket_path` is interpreted identically to the runner's
/// [`animus_runtime_shared::reattach::local_socket_name_for`]: filesystem
/// path on Unix, namespaced pipe name on Windows. Callers should pass the
/// exact string stored in the spawn record's `stdio_socket_path` field.
///
/// Synchronous on purpose: the daemon-startup orphan-reattach path is
/// synchronous (it runs before the main tick loop starts), and the
/// blocking `interprocess::local_socket::Stream::connect()` returns a
/// clean `io::Error` on a non-existent listener so the call site can
/// surface a precise error without bridging through tokio.
pub fn try_reattach(
    socket_path: &str,
    broadcaster: Arc<WorkflowEventBroadcaster>,
) -> std::io::Result<ReattachConnection> {
    use animus_runtime_shared::reattach::local_socket_name_for;
    use interprocess::local_socket::traits::Stream as _;
    use interprocess::local_socket::Stream;

    let name = local_socket_name_for(socket_path)?;
    let stream = Stream::connect(name)?;
    let socket_owned = socket_path.to_string();
    let reader_socket = socket_owned.clone();
    let reader_thread = thread::Builder::new()
        .name(format!("animus-reattach-reader:{}", short_label(&socket_owned)))
        .spawn(move || reader_loop(stream, broadcaster, reader_socket))
        .map_err(std::io::Error::other)?;
    Ok(ReattachConnection { socket_path: socket_owned, reader_thread: Some(reader_thread) })
}

fn reader_loop(stream: interprocess::local_socket::Stream, broadcaster: Arc<WorkflowEventBroadcaster>, socket: String) {
    use std::io::{BufRead, BufReader};
    let reader = BufReader::new(stream);
    for next in reader.lines() {
        match next {
            Ok(line) => forward_line(&broadcaster, &line, &socket),
            Err(error) => {
                tracing::debug!(
                    target: "animus.runtime.reattach",
                    socket = %socket,
                    %error,
                    "reattach reader stream error; exiting"
                );
                return;
            }
        }
    }
    tracing::debug!(
        target: "animus.runtime.reattach",
        socket = %socket,
        "reattach reader EOF (runner closed)"
    );
}

fn short_label(path: &str) -> String {
    std::path::Path::new(path).file_name().and_then(|s| s.to_str()).map(|s| s.to_string()).unwrap_or_else(|| {
        let trimmed = path.trim();
        if trimmed.len() > 16 {
            trimmed[trimmed.len() - 16..].to_string()
        } else {
            trimmed.to_string()
        }
    })
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
/// for each recorded [`animus_runtime_shared::recording::DecisionEvent`] kind that
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
    use animus_runtime_shared::recording::tail::DecisionTailReader;
    use animus_runtime_shared::recording::DecisionEvent;
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
pub trait WorkflowEventBroadcasterLike: Send + Sync {
    fn emit(&self, event: animus_control_protocol::types::WorkflowEvent);
}

/// v0.5.1 fold-in (item 2): convenience that derives the `decisions.jsonl`
/// path directly from an [`super::agent_record::AgentSpawnRecord`] and replays
/// any events that landed past `record.last_consumed_offset`.
///
/// Path-resolution order:
/// 1. The explicit `record.decisions_jsonl_path` field if present (post
///    fold-in records).
/// 2. The canonical
///    `~/.animus/<scope>/runs/<agent_session_id>/decisions.jsonl` derived
///    from `project_root` + `record.agent_session_id` (pre fold-in records
///    and as a safe fallback).
///
/// Returns `Ok(None)` when no decisions.jsonl could be located OR the file
/// does not yet exist on disk — both are valid: the runner may not have
/// produced any decisions yet, or older runners may not honor
/// `ANIMUS_AGENT_RUN_ID`. In neither case is this an error condition.
///
/// On success, the workflow-id used for the synthetic events is taken from
/// the spawn record (`agent_session_id::<workflow_ref>` form so subscribers
/// can filter by workflow_ref). Callers that care about a different
/// workflow_id should use [`replay_decision_log_gap`] directly.
pub fn replay_gap_from_spawn_record(
    project_root: &std::path::Path,
    record: &super::agent_record::AgentSpawnRecord,
    broadcaster: &dyn WorkflowEventBroadcasterLike,
) -> anyhow::Result<Option<GapReplayReport>> {
    let path = match resolve_decisions_path(project_root, record) {
        Some(p) => p,
        None => return Ok(None),
    };
    if !path.exists() {
        return Ok(None);
    }
    let workflow_id = synthetic_workflow_id_for(record);
    let report = replay_decision_log_gap(&path, &workflow_id, record.last_consumed_offset, broadcaster)?;
    Ok(Some(report))
}

/// Resolve the on-disk `decisions.jsonl` path for an agent spawn record.
/// Public for callers that want to inspect or compare paths without driving
/// a full replay.
pub fn resolve_decisions_path(
    project_root: &std::path::Path,
    record: &super::agent_record::AgentSpawnRecord,
) -> Option<std::path::PathBuf> {
    if let Some(explicit) = record.decisions_jsonl_path.as_ref() {
        let path = std::path::PathBuf::from(explicit);
        if path.is_absolute() {
            return Some(path);
        }
    }
    let project_root_str = project_root.to_string_lossy();
    animus_runtime_shared::recording::decision_log_path(&project_root_str, &record.agent_session_id)
}

fn synthetic_workflow_id_for(record: &super::agent_record::AgentSpawnRecord) -> String {
    if record.workflow_ref.is_empty() {
        record.agent_session_id.clone()
    } else {
        format!("{}::{}", record.agent_session_id, record.workflow_ref)
    }
}

pub struct BroadcasterEmitter {
    pub inner: Arc<WorkflowEventBroadcaster>,
}

impl WorkflowEventBroadcasterLike for BroadcasterEmitter {
    fn emit(&self, event: animus_control_protocol::types::WorkflowEvent) {
        self.inner.emit(event);
    }
}

fn forward_line(broadcaster: &WorkflowEventBroadcaster, line: &str, socket: &str) {
    let wire: animus_runtime_shared::workflow_event_emitter::WireWorkflowEvent = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                target: "animus.runtime.reattach",
                socket = %socket,
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
mod tests {
    use super::*;
    use crate::control::WorkflowEventBroadcaster;
    use protocol::SubjectDispatchExt;
    use std::io::Write;
    use std::path::PathBuf;
    use std::thread;
    use tempfile::TempDir;

    #[cfg(unix)]
    fn pair() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reattach.sock");
        (dir, path)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn try_reattach_fails_on_missing_socket() {
        let (_dir, path) = pair();
        let broadcaster = WorkflowEventBroadcaster::new();
        let err = try_reattach(path.to_string_lossy().as_ref(), broadcaster)
            .err()
            .expect("connect must fail when socket absent");
        assert!(err.kind() == std::io::ErrorKind::NotFound || err.kind() == std::io::ErrorKind::ConnectionRefused);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn forwarded_event_reaches_broadcaster_subscriber() {
        use crate::control::WorkflowEventFilter;
        use std::os::unix::net::UnixListener as StdListener;
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

        let _conn = try_reattach(path.to_string_lossy().as_ref(), broadcaster.clone()).expect("reattach connect");

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
        use animus_runtime_shared::recording::{DecisionEvent, Durability, Recorder};
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
        use animus_runtime_shared::recording::{DecisionEvent, Durability, Recorder};
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
        use animus_runtime_shared::recording::{DecisionEvent, Durability, Recorder};
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
        use animus_runtime_shared::recording::{DecisionEvent, Durability, Recorder};
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
    fn replay_gap_from_spawn_record_uses_explicit_path_when_present() {
        use animus_runtime_shared::recording::{DecisionEvent, Durability, Recorder};
        let dir = TempDir::new().unwrap();
        let decisions = dir.path().join("decisions.jsonl");
        let recorder = Recorder::create_with_durability(&decisions, Durability::FsyncPerEvent).expect("recorder");
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        drop(recorder);

        let dispatch = protocol::SubjectDispatch::for_task("TASK-GAP", "standard");
        let record = crate::dispatch::agent_record::build_record_with_decisions(
            "agent-explicit".to_string(),
            12345,
            &dispatch,
            vec!["/bin/echo".into()],
            None,
            Some(decisions.display().to_string()),
        );
        struct Collector(std::sync::Mutex<Vec<animus_control_protocol::types::WorkflowEvent>>);
        impl WorkflowEventBroadcasterLike for Collector {
            fn emit(&self, event: animus_control_protocol::types::WorkflowEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let collector = Collector(std::sync::Mutex::new(Vec::new()));
        let report =
            replay_gap_from_spawn_record(dir.path(), &record, &collector).expect("replay").expect("report present");
        assert_eq!(report.emitted, 1);
        let events = collector.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].workflow_id, "agent-explicit::standard");
        assert_eq!(events[0].kind, "agent_finished");
    }

    #[test]
    fn replay_gap_from_spawn_record_returns_none_when_decisions_missing() {
        let dir = TempDir::new().unwrap();
        let dispatch = protocol::SubjectDispatch::for_task("TASK-NOFILE", "standard");
        let record = crate::dispatch::agent_record::build_record_with_decisions(
            "agent-missing".to_string(),
            12345,
            &dispatch,
            vec!["/bin/echo".into()],
            None,
            Some(dir.path().join("does-not-exist.jsonl").display().to_string()),
        );
        struct NoopEmitter;
        impl WorkflowEventBroadcasterLike for NoopEmitter {
            fn emit(&self, _event: animus_control_protocol::types::WorkflowEvent) {}
        }
        let outcome = replay_gap_from_spawn_record(dir.path(), &record, &NoopEmitter).expect("ok");
        assert!(outcome.is_none(), "missing decisions.jsonl must be Ok(None), not an error");
    }

    #[test]
    fn replay_gap_from_spawn_record_resumes_from_persisted_offset() {
        use animus_runtime_shared::recording::{DecisionEvent, Durability, Recorder};
        let dir = TempDir::new().unwrap();
        let decisions = dir.path().join("decisions.jsonl");
        let recorder = Recorder::create_with_durability(&decisions, Durability::FsyncPerEvent).expect("recorder");
        recorder.record(&DecisionEvent::finished(Some(0))).unwrap();
        recorder.record(&DecisionEvent::error("after-restart")).unwrap();
        drop(recorder);

        let dispatch = protocol::SubjectDispatch::for_task("TASK-RESUME", "standard");
        let mut record = crate::dispatch::agent_record::build_record_with_decisions(
            "agent-resume".to_string(),
            555,
            &dispatch,
            vec!["/bin/echo".into()],
            None,
            Some(decisions.display().to_string()),
        );
        // First sweep starts at 0, then we persist the next_offset on the
        // record to simulate the daemon writing the offset back to disk.
        struct Collector(std::sync::Mutex<Vec<animus_control_protocol::types::WorkflowEvent>>);
        impl WorkflowEventBroadcasterLike for Collector {
            fn emit(&self, event: animus_control_protocol::types::WorkflowEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let collector = Collector(std::sync::Mutex::new(Vec::new()));
        let r1 = replay_gap_from_spawn_record(dir.path(), &record, &collector).unwrap().unwrap();
        record.last_consumed_offset = r1.next_offset;
        let collector2 = Collector(std::sync::Mutex::new(Vec::new()));
        let r2 = replay_gap_from_spawn_record(dir.path(), &record, &collector2).unwrap().unwrap();
        assert_eq!(r2.emitted, 0, "subsequent sweep with persisted offset must replay nothing");
    }

    #[cfg(windows)]
    #[test]
    fn try_reattach_fails_on_missing_named_pipe_on_windows() {
        let unique = format!(
            "animus-test-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        );
        let broadcaster = WorkflowEventBroadcaster::new();
        let err = try_reattach(&unique, broadcaster).err().expect("connect must fail when pipe absent");
        assert!(
            err.kind() == std::io::ErrorKind::NotFound || err.kind() == std::io::ErrorKind::ConnectionRefused,
            "unexpected error kind on missing named pipe: {err:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn try_reattach_forwards_named_pipe_events_to_broadcaster_on_windows() {
        use crate::control::WorkflowEventFilter;
        use animus_runtime_shared::reattach::ReattachListenerEmitter;
        use animus_runtime_shared::workflow_event_emitter::{
            RuntimeWorkflowEvent, RuntimeWorkflowEventKind, WorkflowEventEmitter,
        };
        let unique = format!(
            "animus-test-roundtrip-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        );
        let emitter = ReattachListenerEmitter::bind(&unique).expect("bind named pipe");
        let broadcaster = WorkflowEventBroadcaster::new();
        let (_id, mut rx) = broadcaster.subscribe(WorkflowEventFilter::default());

        // Allow the acceptor thread to come up.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let _conn = try_reattach(&unique, broadcaster.clone()).expect("connect named pipe");
        std::thread::sleep(std::time::Duration::from_millis(100));

        emitter.emit(RuntimeWorkflowEvent {
            workflow_id: "wf-win-reattach".to_string(),
            kind: RuntimeWorkflowEventKind::PhaseStarted,
            payload: serde_json::json!({"phase": "windows"}),
            occurred_at: chrono::Utc::now(),
        });

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let item = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("recv timeout")
                .expect("channel closed")
        });
        match item {
            crate::control::SubscriberItem::Event(event) => {
                assert_eq!(event.workflow_id, "wf-win-reattach");
                assert_eq!(event.kind, "phase_started");
            }
            crate::control::SubscriberItem::Closed { reason } => panic!("unexpected close: {reason}"),
        }
    }

    #[test]
    fn replay_decision_log_gap_writer_reader_race_yields_partial_tail_hint() {
        use animus_runtime_shared::recording::{DecisionEvent, Durability, Recorder};
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
