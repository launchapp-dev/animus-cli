//! Per-spawn Unix-domain-socket back-channel for forwarding
//! `workflow_events` from a runner subprocess into the daemon's in-process
//! [`WorkflowEventBroadcaster`].
//!
//! Lifecycle (Unix only — Windows pending):
//!
//! 1. Before [`crate::ProcessManager::spawn_workflow_runner`] forks the
//!    runner, the caller asks for a [`SubprocessEventPipe`] via
//!    [`SubprocessEventPipe::bind`].
//! 2. The pipe binds a freshly-named `UnixListener` under the project's
//!    `~/.animus/<scope>/runs/<workflow-or-subject>/events.pipe` directory
//!    (falls back to `~/.animus/runs/.../events.pipe` when scoped state is
//!    unavailable, e.g. tests with no git context).
//! 3. The path is exposed via the env var
//!    [`animus_runtime_shared::workflow_event_emitter::ANIMUS_WORKFLOW_EVENT_PIPE_ENV`]
//!    on the spawn command; the runner reads it on startup and builds a
//!    [`animus_runtime_shared::SubprocessPipeEmitter`].
//! 4. A reader task per-spawn accepts the (single) inbound connection,
//!    reads newline-delimited JSON frames, deserializes to
//!    [`WireWorkflowEvent`], and forwards each into the broadcaster as a
//!    wire [`animus_control_protocol::types::WorkflowEvent`].
//! 5. On subprocess exit / pipe close the reader task ends and
//!    [`SubprocessEventPipe::cleanup`] removes the socket file.
//!
//! Windows: this module is gated `#[cfg(unix)]`. On Windows the
//! constructor returns `None` and the daemon dispatches without a
//! back-channel — workflow_events from subprocess runs are silently
//! dropped until a Windows transport (named pipes) is wired.

#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use animus_runtime_shared::workflow_event_emitter::{WireWorkflowEvent, ANIMUS_WORKFLOW_EVENT_PIPE_ENV};
#[cfg(unix)]
use tokio::task::JoinHandle;

#[cfg(unix)]
use crate::control::WorkflowEventBroadcaster;

/// A bound back-channel for a single workflow runner spawn. Carries the
/// path the subprocess will connect to plus the reader-task handle.
///
/// The pipe must outlive the spawned subprocess; drop it after the
/// subprocess has been reaped (and you've drained its stderr). Dropping
/// aborts the reader task and removes the socket file.
#[cfg(unix)]
pub struct SubprocessEventPipe {
    socket_path: PathBuf,
    reader_task: Option<JoinHandle<()>>,
    /// Signals the reader loop to stop accepting new connections and finish
    /// draining the current one. Held in an `Option` so `shutdown` can
    /// `take()` it (firing the notify exactly once) without consuming the
    /// surrounding pipe value, which we still want for the socket cleanup
    /// path on Drop.
    shutdown_signal: Option<Arc<tokio::sync::Notify>>,
}

/// Bounded deadline for the graceful drain on `shutdown`. The reader has
/// this long to finish draining any in-flight stream after the shutdown
/// notify fires; if it doesn't return in time we fall back to `abort` so
/// daemon shutdown never blocks on a misbehaving plugin.
#[cfg(unix)]
const SHUTDOWN_DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_millis(250);

#[cfg(unix)]
#[allow(dead_code)]
impl SubprocessEventPipe {
    /// Bind a fresh per-spawn back-channel under `pipe_root` and start a
    /// reader task that forwards each received event into `broadcaster`.
    ///
    /// `pipe_root` should be the directory we'd like the socket file to
    /// live in (the caller decides scope-vs-project layout); the directory
    /// is created if missing. `subject_label` is a short human-friendly id
    /// included in the file name to make socket files self-identifying when
    /// an operator tails `~/.animus/.../runs/`.
    pub async fn bind(
        pipe_root: &Path,
        subject_label: &str,
        broadcaster: Arc<WorkflowEventBroadcaster>,
    ) -> std::io::Result<Self> {
        Self::bind_sync(pipe_root, subject_label, broadcaster)
    }

    /// Synchronous bind variant. Performs the socket bind on the calling
    /// thread (a couple of syscalls) and spawns the reader task on the
    /// current Tokio runtime. Returns an error if no runtime is current or
    /// if bind fails. Used by the sync subprocess-spawn path so we don't
    /// have to block on a channel waiting for an async task to call
    /// `bind`.
    pub fn bind_sync(
        pipe_root: &Path,
        subject_label: &str,
        broadcaster: Arc<WorkflowEventBroadcaster>,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(pipe_root)?;
        // Unix domain socket paths are capped (SUN_LEN — ~104 bytes on
        // macOS, 108 on Linux). Pick a socket directory whose path fits
        // even when pipe_root is deep — fall back to $TMPDIR for the
        // socket itself while keeping the per-run dir under pipe_root for
        // operator visibility.
        let socket_name = short_socket_name(subject_label);
        let mut socket_path = pipe_root.join(&socket_name);
        if socket_path.as_os_str().len() > MAX_UNIX_SOCKET_PATH_BYTES {
            let fallback_root = std::env::temp_dir().join("animus-event-pipes");
            std::fs::create_dir_all(&fallback_root)?;
            socket_path = fallback_root.join(&socket_name);
        }
        // Defensive: if a stale socket from a crashed prior run is sitting
        // at the same path, remove it so bind() can succeed.
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        // Bind synchronously via std, then promote to a Tokio listener. We
        // require a current Tokio runtime so the reader task has somewhere
        // to live; the caller (subprocess spawn site) always runs inside
        // the daemon runtime.
        let std_listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
        std_listener.set_nonblocking(true)?;
        let listener = tokio::net::UnixListener::from_std(std_listener)?;

        let socket_path_clone = socket_path.clone();
        let shutdown_signal = Arc::new(tokio::sync::Notify::new());
        let reader_signal = shutdown_signal.clone();
        let reader_task = tokio::spawn(async move {
            reader_loop(listener, broadcaster, socket_path_clone, reader_signal).await;
        });

        Ok(Self { socket_path, reader_task: Some(reader_task), shutdown_signal: Some(shutdown_signal) })
    }

    /// Path the runner subprocess should connect to. Pass this via the
    /// [`ANIMUS_WORKFLOW_EVENT_PIPE_ENV`] env var on the spawn command.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Env var name the runner subprocess reads. Exposed for the spawn
    /// site so it can `cmd.env(SubprocessEventPipe::env_var(), pipe.socket_path())`.
    pub fn env_var() -> &'static str {
        ANIMUS_WORKFLOW_EVENT_PIPE_ENV
    }

    /// Explicitly tear down the reader task and remove the socket file.
    /// Idempotent.
    ///
    /// Performs a GRACEFUL drain before abort: signals the reader to stop
    /// accepting new connections and finish reading any in-flight stream,
    /// then awaits the task up to [`SHUTDOWN_DRAIN_DEADLINE`]. If the drain
    /// completes in time, no events are lost. If it exceeds the deadline
    /// (eg a misbehaving plugin that never closes its end), we abort so
    /// daemon shutdown is not stalled indefinitely.
    ///
    /// Previously this method aborted unconditionally, which could drop
    /// the final batch of `workflow_events` a runner emitted immediately
    /// before exiting: the reader was mid-drain of the socket buffer when
    /// the abort fired.
    pub async fn shutdown(mut self) {
        if let Some(signal) = self.shutdown_signal.take() {
            signal.notify_one();
        }
        if let Some(mut task) = self.reader_task.take() {
            // Borrow `&mut task` into the timeout future so the JoinHandle
            // survives a timeout — dropping a JoinHandle merely detaches
            // the task rather than aborting it, so without the `&mut` we
            // would leak a blocked reader past the documented fallback.
            match tokio::time::timeout(SHUTDOWN_DRAIN_DEADLINE, &mut task).await {
                Ok(_join_result) => {
                    // Graceful drain completed (whether the task returned
                    // Ok or panicked is logged inside the reader_loop and
                    // doesn't affect cleanup here).
                }
                Err(_elapsed) => {
                    tracing::debug!(
                        target: "animus.runtime.event_pipe",
                        socket = %self.socket_path.display(),
                        deadline_ms = SHUTDOWN_DRAIN_DEADLINE.as_millis() as u64,
                        "event pipe reader did not finish draining within deadline; falling back to abort"
                    );
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
impl Drop for SubprocessEventPipe {
    fn drop(&mut self) {
        // Best-effort: if the explicit `shutdown` path wasn't taken (Drop
        // happens on panic or when the caller forgets to await), fire the
        // signal so the reader loop exits its accept() and any in-flight
        // stream drains naturally. We can't await here (drop is sync), so
        // we fall back to abort if the task is still pending — the
        // graceful path requires the caller to use `shutdown`.
        if let Some(signal) = self.shutdown_signal.take() {
            signal.notify_one();
        }
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
async fn reader_loop(
    listener: tokio::net::UnixListener,
    broadcaster: Arc<WorkflowEventBroadcaster>,
    socket_path: PathBuf,
    shutdown: Arc<tokio::sync::Notify>,
) {
    // We only ever expect one client per spawn (the runner subprocess we
    // just forked). Loop accepts so that if the runner reconnects after a
    // transient write failure we pick the new connection up.
    //
    // Shutdown semantics:
    // - When shutdown fires while we're at the accept point, we still try
    //   to drain any pending connection in the OS accept queue (via a
    //   non-blocking accept pass) so events the runner wrote and then
    //   exited on are not lost. This matters because the writer task can
    //   finish before the reader has even reached its first accept yield —
    //   shutdown then notifies, and a naive select would skip the drain.
    // - Once we have a stream we drain it to EOF unconditionally — we do
    //   NOT race the drain against `shutdown` because that would re-introduce
    //   the bug we're fixing (final events still buffered when the abort
    //   fires). The outer `SHUTDOWN_DRAIN_DEADLINE` bound in
    //   `SubprocessEventPipe::shutdown` is what caps total time spent
    //   here so daemon shutdown can't hang forever on a misbehaving plugin.
    let mut shutdown_pending = false;
    loop {
        let stream = if shutdown_pending {
            // Drain pass: bounded-wait accept to pick up any connection
            // the kernel buffered before the writer exited. If none
            // materializes within DRAIN_ACCEPT_DEADLINE we're done — the
            // outer shutdown deadline still caps total time. The wait is
            // short because tokio needs a poll cycle to observe a queued
            // connection that was enqueued before this future was created.
            const DRAIN_ACCEPT_DEADLINE: std::time::Duration = std::time::Duration::from_millis(50);
            match tokio::time::timeout(DRAIN_ACCEPT_DEADLINE, listener.accept()).await {
                Ok(Ok((stream, _peer))) => stream,
                Ok(Err(error)) => {
                    tracing::debug!(
                        target: "animus.runtime.event_pipe",
                        socket = %socket_path.display(),
                        %error,
                        "event pipe listener errored during shutdown drain; exiting reader loop"
                    );
                    return;
                }
                Err(_elapsed) => {
                    tracing::debug!(
                        target: "animus.runtime.event_pipe",
                        socket = %socket_path.display(),
                        "event pipe reader found no buffered connection after shutdown; exiting reader loop"
                    );
                    return;
                }
            }
        } else {
            let accept_or_shutdown = tokio::select! {
                res = listener.accept() => Some(res),
                _ = shutdown.notified() => None,
            };
            match accept_or_shutdown {
                None => {
                    // Shutdown fired while idle. Take one more pass through
                    // the loop in drain mode to pick up any kernel-buffered
                    // connection the runner left behind.
                    shutdown_pending = true;
                    continue;
                }
                Some(Ok((stream, _peer))) => stream,
                Some(Err(error)) => {
                    tracing::debug!(
                        target: "animus.runtime.event_pipe",
                        socket = %socket_path.display(),
                        %error,
                        "event pipe listener errored; exiting reader loop"
                    );
                    return;
                }
            }
        };

        use tokio::io::{AsyncBufReadExt, BufReader};
        let reader = BufReader::new(stream);
        let mut lines = reader.lines();
        loop {
            // No tokio::select on shutdown here: we want every buffered
            // line drained before we exit. The outer deadline in
            // SubprocessEventPipe::shutdown bounds total time so a runaway
            // writer can't stall shutdown indefinitely.
            match lines.next_line().await {
                Ok(Some(line)) => {
                    forward_line(&broadcaster, &line);
                }
                Ok(None) => break, // EOF; subprocess closed.
                Err(error) => {
                    tracing::debug!(
                        target: "animus.runtime.event_pipe",
                        %error,
                        "event pipe stream errored mid-stream; rotating to accept loop"
                    );
                    break;
                }
            }
        }
        // After draining one stream during the shutdown drain pass, exit
        // — we only ever expect one client per spawn and the outer
        // deadline bounds total time.
        if shutdown_pending {
            tracing::debug!(
                target: "animus.runtime.event_pipe",
                socket = %socket_path.display(),
                "event pipe reader drained one stream after shutdown; exiting reader loop"
            );
            return;
        }
    }
}

#[cfg(unix)]
fn forward_line(broadcaster: &WorkflowEventBroadcaster, line: &str) {
    let wire: WireWorkflowEvent = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                target: "animus.runtime.event_pipe",
                %error,
                "discarding malformed workflow event frame"
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

/// Conservative cap on the Unix domain socket path length. POSIX guarantees
/// only ~104 bytes for SUN_LEN on macOS / 108 on Linux. Use 100 to give
/// headroom for trailing nulls and platform quirks.
#[cfg(unix)]
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 100;

/// Build a short, collision-resistant socket file name. We never embed the
/// full subject_label because most subject ids are too long to leave room
/// for a meaningful unique suffix inside `MAX_UNIX_SOCKET_PATH_BYTES`.
#[cfg(unix)]
fn short_socket_name(subject_label: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    // 12 chars of sanitized subject label is enough to make the file
    // self-identifying without blowing the path budget.
    let short_label: String =
        subject_label.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(12).collect();
    let short_label = if short_label.is_empty() { "wf".to_string() } else { short_label };
    format!("{}-{:x}.sock", short_label, nanos & 0xffff_ffff)
}

#[cfg(not(unix))]
pub struct SubprocessEventPipe;

#[cfg(not(unix))]
impl SubprocessEventPipe {
    pub fn env_var() -> &'static str {
        "ANIMUS_WORKFLOW_EVENT_PIPE"
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::control::{SubscriberItem, WorkflowEventBroadcaster, WorkflowEventFilter};
    use animus_runtime_shared::workflow_event_emitter::WireWorkflowEvent;
    use chrono::Utc;
    use serde_json::json;
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use tempfile::tempdir;

    fn unwrap_event(item: SubscriberItem) -> animus_control_protocol::types::WorkflowEvent {
        match item {
            SubscriberItem::Event(e) => e,
            SubscriberItem::Closed { reason } => panic!("expected event, got Closed({reason})"),
        }
    }

    #[tokio::test]
    async fn subprocess_workflow_emits_events_via_pipe_to_daemon_broadcaster() {
        let temp = tempdir().expect("tempdir");
        let bus = WorkflowEventBroadcaster::new();
        let (_id, mut rx) = bus.subscribe(WorkflowEventFilter::default());

        let pipe = SubprocessEventPipe::bind(temp.path(), "wf-pipe-test", bus.clone()).await.expect("bind pipe");
        let socket_path = pipe.socket_path().to_path_buf();

        // Simulate the subprocess writing two newline-delimited frames into the
        // socket the daemon prebound. This is exactly what
        // SubprocessPipeEmitter does inside the runner process.
        let writer_path = socket_path.clone();
        let writer = tokio::task::spawn_blocking(move || {
            let mut stream = UnixStream::connect(&writer_path).expect("subprocess connect");
            let evt1 = WireWorkflowEvent {
                workflow_id: "wf-subprocess-1".to_string(),
                kind: "phase_started".to_string(),
                payload: json!({"phase_id": "impl"}),
                occurred_at: Utc::now(),
            };
            let evt2 = WireWorkflowEvent {
                workflow_id: "wf-subprocess-1".to_string(),
                kind: "phase_completed".to_string(),
                payload: json!({"phase_id": "impl", "phase_status": "completed"}),
                occurred_at: Utc::now(),
            };
            for evt in [&evt1, &evt2] {
                let mut line = serde_json::to_string(evt).expect("serialize");
                line.push('\n');
                stream.write_all(line.as_bytes()).expect("write line");
            }
            // Drop stream to signal EOF.
        });
        writer.await.expect("writer task");

        let first = unwrap_event(rx.recv().await.expect("event 1 must arrive"));
        let second = unwrap_event(rx.recv().await.expect("event 2 must arrive"));
        assert_eq!(first.workflow_id, "wf-subprocess-1");
        assert_eq!(first.kind, "phase_started");
        assert_eq!(second.kind, "phase_completed");

        pipe.shutdown().await;
        assert!(!socket_path.exists(), "socket file should be removed on shutdown");
    }

    #[tokio::test]
    async fn bind_sync_does_not_block_runtime_and_forwards_events() {
        // Regression: previously the subprocess-spawn site spawned an
        // async bind task and waited on a `std::sync::mpsc::Receiver`,
        // which could deadlock a current-thread runtime and stall a
        // worker on a multi-thread runtime. The `bind_sync` path binds
        // on the calling thread (no waiting on another task) and just
        // spawns the reader. This test asserts the sync entry point
        // works end-to-end without any rx.recv() hop.
        let temp = tempdir().expect("tempdir");
        let bus = WorkflowEventBroadcaster::new();
        let (_id, mut rx) = bus.subscribe(WorkflowEventFilter::default());

        let pipe = SubprocessEventPipe::bind_sync(temp.path(), "wf-sync-bind", bus.clone()).expect("bind_sync");
        let socket_path = pipe.socket_path().to_path_buf();

        let writer_path = socket_path.clone();
        let writer = tokio::task::spawn_blocking(move || {
            let mut stream = UnixStream::connect(&writer_path).expect("connect");
            let evt = WireWorkflowEvent {
                workflow_id: "wf-sync".to_string(),
                kind: "phase_started".to_string(),
                payload: json!({}),
                occurred_at: Utc::now(),
            };
            let mut line = serde_json::to_string(&evt).expect("serialize");
            line.push('\n');
            stream.write_all(line.as_bytes()).expect("write");
        });
        writer.await.expect("writer task");

        let event = unwrap_event(rx.recv().await.expect("event must arrive"));
        assert_eq!(event.workflow_id, "wf-sync");
        assert_eq!(event.kind, "phase_started");
        pipe.shutdown().await;
    }

    // Fix 3 (durability hardening): shutdown must drain buffered events
    // before tearing the reader down. Pre-fix, shutdown unconditionally
    // aborted the reader task, which could discard the final batch of
    // events the runner subprocess emitted immediately before exiting
    // (the writer flushed bytes into the socket buffer; the reader had
    // not yet consumed them; abort fired before the next next_line()).
    //
    // Repro: write N frames, close the writer to signal EOF, IMMEDIATELY
    // call shutdown (giving the reader no scheduling time to drain).
    // The graceful-drain path must still observe all N events on the
    // broadcaster.
    #[tokio::test]
    async fn shutdown_drains_buffered_events_before_aborting() {
        let temp = tempdir().expect("tempdir");
        let bus = WorkflowEventBroadcaster::new();
        let (_id, mut rx) = bus.subscribe(WorkflowEventFilter::default());

        let pipe = SubprocessEventPipe::bind(temp.path(), "wf-drain", bus.clone()).await.expect("bind pipe");
        let socket_path = pipe.socket_path().to_path_buf();

        // Synchronously write 3 frames into the socket buffer, then close
        // the writer. spawn_blocking keeps this off the runtime so the
        // reader task can't make progress until the await below yields.
        let writer_path = socket_path.clone();
        tokio::task::spawn_blocking(move || {
            let mut stream = UnixStream::connect(&writer_path).expect("connect");
            for i in 0..3 {
                let evt = WireWorkflowEvent {
                    workflow_id: format!("wf-final-batch-{i}"),
                    kind: "phase_completed".to_string(),
                    payload: json!({ "phase_id": "impl", "phase_status": "completed", "i": i }),
                    occurred_at: Utc::now(),
                };
                let mut line = serde_json::to_string(&evt).expect("serialize");
                line.push('\n');
                stream.write_all(line.as_bytes()).expect("write");
            }
            stream.flush().expect("flush");
            // Drop the stream to signal EOF; the reader will see Ok(None)
            // after draining buffered lines.
        })
        .await
        .expect("writer task");

        // IMMEDIATELY shutdown without yielding to the reader. The
        // graceful-drain path must still surface all 3 events.
        pipe.shutdown().await;

        let mut received: Vec<String> = Vec::new();
        // Drain everything that landed in the broadcaster within a short
        // window. Use try_recv-style polling with a small timeout so the
        // test doesn't hang if the fix regresses (it would simply yield
        // fewer than 3 events instead of all 3).
        for _ in 0..3 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                Ok(Some(item)) => {
                    let evt = unwrap_event(item);
                    received.push(evt.workflow_id);
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }

        assert_eq!(
            received.len(),
            3,
            "graceful-drain shutdown must surface ALL events buffered before EOF; got: {received:?}"
        );
        for i in 0..3 {
            assert!(
                received.contains(&format!("wf-final-batch-{i}")),
                "missing event wf-final-batch-{i} in {received:?}"
            );
        }
        assert!(!socket_path.exists(), "socket file should be removed on shutdown");
    }

    #[tokio::test]
    async fn malformed_line_is_discarded_without_killing_reader() {
        let temp = tempdir().expect("tempdir");
        let bus = WorkflowEventBroadcaster::new();
        let (_id, mut rx) = bus.subscribe(WorkflowEventFilter::default());

        let pipe = SubprocessEventPipe::bind(temp.path(), "wf-malformed", bus.clone()).await.expect("bind pipe");
        let socket_path = pipe.socket_path().to_path_buf();

        let writer = tokio::task::spawn_blocking(move || {
            let mut stream = UnixStream::connect(&socket_path).expect("connect");
            stream.write_all(b"this is not json\n").expect("write garbage");
            let valid = WireWorkflowEvent {
                workflow_id: "wf-after-garbage".to_string(),
                kind: "phase_started".to_string(),
                payload: json!({}),
                occurred_at: Utc::now(),
            };
            let mut line = serde_json::to_string(&valid).expect("serialize");
            line.push('\n');
            stream.write_all(line.as_bytes()).expect("write valid");
        });
        writer.await.expect("writer task");

        let valid_event = unwrap_event(rx.recv().await.expect("post-garbage event must still arrive"));
        assert_eq!(valid_event.workflow_id, "wf-after-garbage");
        pipe.shutdown().await;
    }
}
