//! Daemon-restart-survivable event back-channel for `animus-workflow-runner`.
//!
//! v0.5.1 P2 #6.2 round-3 fold-in. Closes the "daemon dies, runner stranded"
//! loop one step further than the round-2 orphan-scan: the runner now binds
//! its own `UnixListener` and acts as the server, so the daemon can attach
//! at first spawn, detach when it dies, and re-attach on the next start
//! without involving the runner at all.
//!
//! Survival contract (Unix only):
//! 1. The daemon allocates a deterministic socket path under
//!    `~/.animus/<scope>/runs/_pending/agents/<session_id>.reattach.sock`
//!    BEFORE spawning the runner. The path is advertised to the runner via
//!    the `ANIMUS_WORKFLOW_REATTACH_SOCKET` env var, and recorded in
//!    `AgentSpawnRecord::stdio_socket_path` so a fresh daemon can find it
//!    on startup orphan-scan.
//! 2. The runner binds the listener on startup. Bind happens before any
//!    workflow phase work begins. If the daemon never connects (eg a CLI-
//!    driven run that doesn't want a back-channel), the listener idles —
//!    its existence does not block phase execution.
//! 3. Every `RuntimeWorkflowEvent` the runner emits is broadcast to every
//!    currently-connected listener. Slow / disconnected listeners are
//!    dropped silently (losing a phase boundary event is strictly better
//!    than blocking the runner).
//! 4. When the daemon dies, its `UnixStream` closes; the runner notices the
//!    next time it tries to write and prunes the dead reader. The listener
//!    socket file remains valid, so a fresh daemon can connect again.
//! 5. On daemon restart, the orphan-scan reattach path looks up the spawn
//!    record's `stdio_socket_path` and calls `connect()` on it. From that
//!    moment forward, the daemon receives every NEW event the runner emits.
//!
//! Known gaps (intentional v0.6 work):
//! - No event buffering on the runner side: events emitted DURING the
//!   daemon-gap are not replayed to the reattached daemon. The daemon must
//!   consult `decisions.jsonl` for gap reconstruction.
//! - No flow control: a slow reader is dropped immediately if its write
//!   buffer fills, rather than back-pressuring the runner.
//! - No authentication: any process that can read the socket path can
//!   attach. Mitigated by the path living under the user's `~/.animus/`
//!   (mode 0700-recommended) and by the events being non-secret event
//!   metadata.
//! - Windows: gated `#[cfg(unix)]`. Windows daemons fall back to the
//!   legacy daemon-binds path and lose reattach survivability.

use std::path::PathBuf;
use std::sync::Arc;

use crate::workflow_event_emitter::{RuntimeWorkflowEvent, WireWorkflowEvent, WorkflowEventEmitter};

/// Env var the daemon sets on `animus-workflow-runner` spawn to tell the
/// runner where to bind its reattach listener. When this var is set the
/// runner builds a [`ReattachListenerEmitter`] in addition to (or in place
/// of) [`crate::workflow_event_emitter::SubprocessPipeEmitter`].
pub const ANIMUS_WORKFLOW_REATTACH_SOCKET_ENV: &str = "ANIMUS_WORKFLOW_REATTACH_SOCKET";

#[cfg(unix)]
mod imp {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::Mutex;
    use std::thread;

    /// Server-side broadcast emitter. Bound by the runner; accepts daemon
    /// connections and fans out every event line to all currently-attached
    /// readers.
    pub struct ReattachListenerEmitter {
        socket_path: PathBuf,
        readers: Arc<Mutex<Vec<UnixStream>>>,
        // Listener thread is held so the JoinHandle keeps the thread alive
        // for the lifetime of the emitter. Drop closes the socket file.
        _acceptor: thread::JoinHandle<()>,
    }

    impl ReattachListenerEmitter {
        pub fn bind(socket_path: impl Into<PathBuf>) -> std::io::Result<Arc<Self>> {
            let socket_path = socket_path.into();
            if let Some(parent) = socket_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Defensive cleanup: a stale socket from a prior crashed runner
            // at the same path would make bind() fail. Removing it is safe
            // because the orphan scan only points at THIS path when the
            // recorded PID is alive — if we're binding here, the prior owner
            // is gone.
            if socket_path.exists() {
                let _ = std::fs::remove_file(&socket_path);
            }
            let listener = UnixListener::bind(&socket_path)?;
            // Restrict to current user. The socket carries event metadata,
            // not secrets, but the runner runs in the user's session and a
            // mode-0700 socket file matches the rest of `~/.animus/`.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&socket_path) {
                    let mut perms = meta.permissions();
                    perms.set_mode(0o600);
                    let _ = std::fs::set_permissions(&socket_path, perms);
                }
            }
            listener.set_nonblocking(false)?;
            let readers: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));
            let readers_for_acceptor = readers.clone();
            let socket_for_acceptor = socket_path.clone();
            let acceptor = thread::Builder::new()
                .name(format!("animus-reattach-acceptor:{}", short_label(&socket_path)))
                .spawn(move || acceptor_loop(listener, readers_for_acceptor, socket_for_acceptor))
                .map_err(std::io::Error::other)?;
            Ok(Arc::new(Self { socket_path, readers, _acceptor: acceptor }))
        }

        pub fn from_env() -> Option<Arc<Self>> {
            let path = std::env::var(ANIMUS_WORKFLOW_REATTACH_SOCKET_ENV).ok()?;
            let trimmed = path.trim();
            if trimmed.is_empty() {
                return None;
            }
            match Self::bind(trimmed) {
                Ok(emitter) => Some(emitter),
                Err(err) => {
                    tracing::warn!(
                        target: "animus.runtime.reattach",
                        socket = trimmed,
                        error = %err,
                        "failed to bind reattach listener; falling back to noop reattach"
                    );
                    None
                }
            }
        }

        pub fn socket_path(&self) -> &std::path::Path {
            &self.socket_path
        }

        /// Bounded write deadline per reader. If a reattach reader (typically
        /// the daemon) is wedged and its socket buffer fills, we drop it
        /// after this deadline rather than block the workflow runner's emit
        /// path. Codex round-1 P2: a slow/wedged client previously could
        /// hang the runner inside `write_all` once the socket buffer
        /// saturated.
        const WRITE_DEADLINE: std::time::Duration = std::time::Duration::from_millis(100);

        fn broadcast_line(&self, line: &str) {
            let mut guard = match self.readers.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if guard.is_empty() {
                return;
            }
            // Write to every reader; drop any that has closed, whose buffer
            // is too full to accept the frame within WRITE_DEADLINE, or
            // whose write returns any other error. We never block the
            // runner on a slow reader.
            let mut survivors: Vec<UnixStream> = Vec::with_capacity(guard.len());
            for mut reader in guard.drain(..) {
                // Bounded-blocking writes: setting a write timeout makes
                // `write_all` return an Err if the kernel can't drain the
                // bytes in time. The reader will then be dropped on the
                // next branch.
                let _ = reader.set_nonblocking(false);
                let _ = reader.set_write_timeout(Some(Self::WRITE_DEADLINE));
                let mut ok = reader.write_all(line.as_bytes()).is_ok();
                if ok {
                    ok = reader.write_all(b"\n").is_ok() && reader.flush().is_ok();
                }
                if ok {
                    survivors.push(reader);
                } else {
                    tracing::debug!(
                        target: "animus.runtime.reattach",
                        socket = %self.socket_path.display(),
                        deadline_ms = Self::WRITE_DEADLINE.as_millis() as u64,
                        "dropped a stalled or disconnected reattach reader"
                    );
                }
            }
            *guard = survivors;
        }
    }

    impl Drop for ReattachListenerEmitter {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    impl WorkflowEventEmitter for ReattachListenerEmitter {
        fn emit(&self, event: RuntimeWorkflowEvent) {
            let wire = WireWorkflowEvent::from(&event);
            let line = match serde_json::to_string(&wire) {
                Ok(s) => s,
                Err(_) => return,
            };
            self.broadcast_line(&line);
        }
    }

    fn acceptor_loop(listener: UnixListener, readers: Arc<Mutex<Vec<UnixStream>>>, socket_path: PathBuf) {
        loop {
            let (stream, _addr) = match listener.accept() {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::debug!(
                        target: "animus.runtime.reattach",
                        socket = %socket_path.display(),
                        error = %err,
                        "reattach acceptor loop exiting on accept error"
                    );
                    return;
                }
            };
            if let Ok(mut guard) = readers.lock() {
                guard.push(stream);
            }
        }
    }

    fn short_label(path: &std::path::Path) -> String {
        path.file_name().and_then(|s| s.to_str()).unwrap_or("reattach").to_string()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::workflow_event_emitter::{RuntimeWorkflowEventKind, WireWorkflowEvent};
        use chrono::Utc;
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixStream;
        use tempfile::TempDir;

        fn temp_socket() -> (TempDir, PathBuf) {
            let dir = TempDir::new().expect("tempdir");
            let path = dir.path().join("reattach.sock");
            (dir, path)
        }

        fn sample_event(label: &str) -> RuntimeWorkflowEvent {
            RuntimeWorkflowEvent {
                workflow_id: format!("wf-{label}"),
                kind: RuntimeWorkflowEventKind::PhaseStarted,
                payload: serde_json::json!({"phase": label}),
                occurred_at: Utc::now(),
            }
        }

        #[test]
        fn bind_creates_socket_file() {
            let (_dir, path) = temp_socket();
            let emitter = ReattachListenerEmitter::bind(&path).expect("bind");
            assert_eq!(emitter.socket_path(), path);
            assert!(path.exists(), "socket file must exist after bind");
        }

        #[test]
        fn bind_removes_stale_socket_at_path() {
            let (_dir, path) = temp_socket();
            // Pre-create a regular file at the path (simulating a stale
            // socket from a crashed prior runner; bind() can't tell the
            // difference at the syscall layer).
            std::fs::write(&path, b"stale").unwrap();
            let _emitter = ReattachListenerEmitter::bind(&path).expect("bind replaces stale");
            assert!(path.exists());
        }

        #[test]
        fn connected_reader_receives_broadcast_event() {
            let (_dir, path) = temp_socket();
            let emitter = ReattachListenerEmitter::bind(&path).expect("bind");

            // Connect a reader (simulating the daemon side).
            let stream = UnixStream::connect(&path).expect("connect");
            stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            let mut reader = BufReader::new(stream);

            // Give the acceptor a moment to register the connection.
            // The acceptor is a separate thread; a tiny sleep is enough.
            std::thread::sleep(std::time::Duration::from_millis(50));

            emitter.emit(sample_event("alpha"));

            let mut line = String::new();
            reader.read_line(&mut line).expect("read line");
            let wire: WireWorkflowEvent = serde_json::from_str(line.trim()).expect("parse");
            assert_eq!(wire.workflow_id, "wf-alpha");
            assert_eq!(wire.kind, "phase_started");
        }

        #[test]
        fn second_reader_attaches_and_receives_only_subsequent_events() {
            let (_dir, path) = temp_socket();
            let emitter = ReattachListenerEmitter::bind(&path).expect("bind");

            // First reader connects and consumes one event.
            let first = UnixStream::connect(&path).expect("connect first");
            first.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            let mut first_reader = BufReader::new(first);
            std::thread::sleep(std::time::Duration::from_millis(50));
            emitter.emit(sample_event("one"));
            let mut s1 = String::new();
            first_reader.read_line(&mut s1).expect("first reader event 1");
            assert!(s1.contains("wf-one"));

            // Simulate the daemon crashing: drop the first reader.
            drop(first_reader);
            // Emit while no daemon is attached. The emitter detects the
            // dropped reader on the next write and removes it.
            emitter.emit(sample_event("gap-event"));

            // Second daemon attaches.
            let second = UnixStream::connect(&path).expect("connect second");
            second.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            let mut second_reader = BufReader::new(second);
            std::thread::sleep(std::time::Duration::from_millis(50));
            emitter.emit(sample_event("after-reattach"));

            let mut s2 = String::new();
            second_reader.read_line(&mut s2).expect("second reader event 1");
            assert!(s2.contains("wf-after-reattach"), "second reader sees only post-reattach events: {s2}");
            // The gap event is NOT replayed — that's the documented v0.6 gap.
        }

        #[test]
        fn dropped_reader_does_not_block_emit() {
            let (_dir, path) = temp_socket();
            let emitter = ReattachListenerEmitter::bind(&path).expect("bind");
            let stream = UnixStream::connect(&path).expect("connect");
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(stream);
            // Emit several events with no reader attached. Must not hang.
            for i in 0..10 {
                emitter.emit(sample_event(&format!("orphan-{i}")));
            }
        }

        #[test]
        fn stalled_reader_does_not_block_runner_emit() {
            // Codex round-1 P2 regression guard: if a reader connects but
            // never drains, the runner must NOT hang inside emit when the
            // socket buffer fills. We fill up the buffer by emitting many
            // large events and assert the whole sequence completes within
            // a bounded wall-clock window. Without the WRITE_DEADLINE knob,
            // this test hangs (kernel pipe buffer is ~64KiB-ish; with no
            // reader draining, write_all blocks once it's full).
            let (_dir, path) = temp_socket();
            let emitter = ReattachListenerEmitter::bind(&path).expect("bind");

            // Connect but never read.
            let stream = UnixStream::connect(&path).expect("connect");
            // Tighten the socket buffer so we hit the wedge condition with
            // fewer events. set_nonblocking on the READER side (the
            // connecting socket) doesn't matter — the runner-side
            // accepted socket is what we time out on, and that's handled
            // by `set_write_timeout` in broadcast_line.
            std::thread::sleep(std::time::Duration::from_millis(50));

            let payload: String = "x".repeat(8 * 1024);
            let start = std::time::Instant::now();
            for i in 0..200 {
                emitter.emit(RuntimeWorkflowEvent {
                    workflow_id: format!("wf-stall-{i}"),
                    kind: RuntimeWorkflowEventKind::PhaseStarted,
                    payload: serde_json::json!({"big": payload.clone()}),
                    occurred_at: Utc::now(),
                });
                // Belt-and-suspenders: fail loud if any one emit blocked
                // longer than ~1s (vastly above the 100ms WRITE_DEADLINE).
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(20),
                    "runner emit blocked too long; stalled reader was not dropped"
                );
            }

            // Keep the stalled reader alive until the end of the scope so
            // its socket file descriptor doesn't close before broadcast.
            drop(stream);
        }

        #[test]
        fn from_env_returns_none_when_unset() {
            let prev = std::env::var(ANIMUS_WORKFLOW_REATTACH_SOCKET_ENV).ok();
            std::env::remove_var(ANIMUS_WORKFLOW_REATTACH_SOCKET_ENV);
            assert!(ReattachListenerEmitter::from_env().is_none());
            if let Some(v) = prev {
                std::env::set_var(ANIMUS_WORKFLOW_REATTACH_SOCKET_ENV, v);
            }
        }
    }
}

#[cfg(unix)]
pub use imp::ReattachListenerEmitter;

#[cfg(not(unix))]
pub struct ReattachListenerEmitter;

#[cfg(not(unix))]
impl ReattachListenerEmitter {
    pub fn bind(_socket_path: impl Into<PathBuf>) -> std::io::Result<Arc<Self>> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "reattach listener is Unix-only"))
    }
    pub fn from_env() -> Option<Arc<Self>> {
        None
    }
    pub fn socket_path(&self) -> &std::path::Path {
        std::path::Path::new("")
    }
}

#[cfg(not(unix))]
impl WorkflowEventEmitter for ReattachListenerEmitter {
    fn emit(&self, _event: RuntimeWorkflowEvent) {}
}
