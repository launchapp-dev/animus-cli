//! Daemon-restart-survivable event back-channel for `animus-workflow-runner`.
//!
//! v0.5.1 P2 #6.2 round-3 fold-in seeded the Unix-only path; the v0.5.1
//! BETA fold-in (item 7) collapsed Unix + Windows into ONE wire-protocol
//! implementation on top of [`interprocess::local_socket`]. The runner is
//! the SERVER (binds a listener); the daemon is the CLIENT (connects on
//! every startup). One `interprocess` listener type covers Unix domain
//! sockets and Windows named pipes transparently.
//!
//! Survival contract (both platforms):
//! 1. The daemon allocates a deterministic socket name BEFORE spawning the
//!    runner. The name is advertised to the runner via the
//!    `ANIMUS_WORKFLOW_REATTACH_SOCKET` env var and recorded in
//!    `AgentSpawnRecord::stdio_socket_path` so a fresh daemon can find it
//!    on startup orphan-scan. The string is platform-specific:
//!     - Unix:  filesystem path under `~/.animus/.../agents/<id>.r.sock`
//!     - Windows: namespaced pipe name `animus-reattach-<pid>-<id>`
//!
//!    Callers should treat the string as opaque and round-trip it via the
//!    [`local_socket_name_for`] helper rather than parsing it.
//! 2. The runner binds the listener on startup. Bind happens before any
//!    workflow phase work begins. If the daemon never connects (eg a CLI-
//!    driven run that doesn't want a back-channel), the listener idles —
//!    its existence does not block phase execution.
//! 3. Every `RuntimeWorkflowEvent` the runner emits is dispatched to every
//!    currently-connected reader via a per-reader writer thread fed by a
//!    bounded mpsc queue (capacity `BROADCAST_QUEUE_DEPTH`). If a reader
//!    stalls and its queue fills, the runner drops events for that one
//!    reader rather than blocking the emit path. Other readers continue
//!    receiving events unaffected.
//! 4. When the daemon dies, its stream closes; the per-reader writer
//!    thread notices on the next write and exits. The listener survives,
//!    so a fresh daemon can connect again.
//! 5. On daemon restart, the orphan-scan reattach path looks up the spawn
//!    record's `stdio_socket_path` and connects to it. From that moment
//!    forward, the daemon receives every NEW event the runner emits.
//!
//! Known gaps (still v0.6):
//! - No event buffering on the runner side: events emitted DURING the
//!   daemon-gap are not replayed via this socket. The daemon must consult
//!   `decisions.jsonl` for gap reconstruction — see
//!   `orchestrator_daemon_runtime::dispatch::reattach::replay_gap_from_spawn_record`.

use std::io::Write;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use interprocess::local_socket::{prelude::*, GenericFilePath, GenericNamespaced, ListenerOptions, Name, Stream};

use crate::workflow_event_emitter::{RuntimeWorkflowEvent, WireWorkflowEvent, WorkflowEventEmitter};

/// Env var the daemon sets on `animus-workflow-runner` spawn to tell the
/// runner where to bind its reattach listener. The value is interpreted as:
/// - A filesystem path when it contains a path separator, OR
/// - A namespaced pipe name otherwise.
///
/// Use [`local_socket_name_for`] to round-trip the value back into an
/// `interprocess` `Name` regardless of platform.
pub const ANIMUS_WORKFLOW_REATTACH_SOCKET_ENV: &str = "ANIMUS_WORKFLOW_REATTACH_SOCKET";

/// Per-reader broadcast queue depth. A slow reader whose writer thread
/// cannot keep up will start dropping events once this many are pending.
/// Sized generously to absorb a phase-start/finish burst without dropping
/// in the common case, but small enough that a wedged reader doesn't
/// pin meaningful memory.
const BROADCAST_QUEUE_DEPTH: usize = 256;

/// v0.5.1 fold-in (item 7): resolve a stored socket-identifier string into
/// a platform-appropriate [`Name`]. The daemon writes either an absolute
/// path (Unix) or a namespace identifier (Windows) into the spawn record;
/// both sides of the reattach pair use this helper so the encoding stays
/// consistent.
pub fn local_socket_name_for(value: &str) -> std::io::Result<Name<'_>> {
    if looks_like_filesystem(value) {
        value.to_fs_name::<GenericFilePath>()
    } else {
        value.to_ns_name::<GenericNamespaced>()
    }
}

fn looks_like_filesystem(value: &str) -> bool {
    value.contains(std::path::MAIN_SEPARATOR) || value.contains('/')
}

/// Handle for a single attached reader: its per-reader queue sender, used
/// by the broadcast path to push event lines. Dropping the sender shuts
/// down the writer thread (which exits on the channel close).
struct ReaderHandle {
    sender: mpsc::SyncSender<Vec<u8>>,
}

/// Server-side broadcast emitter. Bound by the runner; accepts daemon
/// connections and fans out every event line to all currently-attached
/// readers. Works identically on Unix (domain sockets) and Windows
/// (named pipes) via [`interprocess::local_socket`].
pub struct ReattachListenerEmitter {
    socket_path: String,
    readers: Arc<Mutex<Vec<ReaderHandle>>>,
    _acceptor: Option<thread::JoinHandle<()>>,
    on_drop_path: Option<std::path::PathBuf>,
}

impl ReattachListenerEmitter {
    pub fn bind(socket_path: impl AsRef<str>) -> std::io::Result<Arc<Self>> {
        Self::bind_inner(socket_path.as_ref().to_string())
    }

    pub fn bind_path(socket_path: impl AsRef<Path>) -> std::io::Result<Arc<Self>> {
        Self::bind(&*socket_path.as_ref().to_string_lossy())
    }

    fn bind_inner(socket_path: String) -> std::io::Result<Arc<Self>> {
        let (name, on_drop_path) = Self::resolve_name(&socket_path)?;

        if let Some(ref path) = on_drop_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
        }

        let listener = ListenerOptions::new().name(name).create_sync()?;

        #[cfg(unix)]
        if let Some(ref path) = on_drop_path {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(path, perms);
            }
        }

        let readers: Arc<Mutex<Vec<ReaderHandle>>> = Arc::new(Mutex::new(Vec::new()));
        let readers_for_acceptor = readers.clone();
        let socket_label = socket_path.clone();
        let acceptor = thread::Builder::new()
            .name(format!("animus-reattach-acceptor:{}", short_label(&socket_path)))
            .spawn(move || acceptor_loop(listener, readers_for_acceptor, socket_label))
            .map_err(std::io::Error::other)?;

        Ok(Arc::new(Self { socket_path, readers, _acceptor: Some(acceptor), on_drop_path }))
    }

    fn resolve_name(socket_path: &str) -> std::io::Result<(Name<'_>, Option<std::path::PathBuf>)> {
        if looks_like_filesystem(socket_path) {
            let name = socket_path.to_fs_name::<GenericFilePath>()?;
            Ok((name, Some(std::path::PathBuf::from(socket_path))))
        } else {
            let name = socket_path.to_ns_name::<GenericNamespaced>()?;
            Ok((name, None))
        }
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

    pub fn socket_path(&self) -> &str {
        &self.socket_path
    }

    fn broadcast_line(&self, line: Vec<u8>) {
        let mut guard = match self.readers.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if guard.is_empty() {
            return;
        }
        let mut survivors: Vec<ReaderHandle> = Vec::with_capacity(guard.len());
        for handle in guard.drain(..) {
            match handle.sender.try_send(line.clone()) {
                Ok(()) => survivors.push(handle),
                Err(mpsc::TrySendError::Full(_)) => {
                    tracing::debug!(
                        target: "animus.runtime.reattach",
                        socket = %self.socket_path,
                        depth = BROADCAST_QUEUE_DEPTH,
                        "dropping stalled reattach reader (per-reader queue full)"
                    );
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    tracing::debug!(
                        target: "animus.runtime.reattach",
                        socket = %self.socket_path,
                        "dropping closed reattach reader"
                    );
                }
            }
        }
        *guard = survivors;
    }
}

impl Drop for ReattachListenerEmitter {
    fn drop(&mut self) {
        if let Some(path) = self.on_drop_path.as_ref() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl WorkflowEventEmitter for ReattachListenerEmitter {
    fn emit(&self, event: RuntimeWorkflowEvent) {
        let wire = WireWorkflowEvent::from(&event);
        let mut bytes = match serde_json::to_vec(&wire) {
            Ok(s) => s,
            Err(_) => return,
        };
        bytes.push(b'\n');
        self.broadcast_line(bytes);
    }
}

fn acceptor_loop(
    listener: interprocess::local_socket::Listener,
    readers: Arc<Mutex<Vec<ReaderHandle>>>,
    socket_label: String,
) {
    use interprocess::local_socket::traits::Listener as _;
    loop {
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(err) => {
                tracing::debug!(
                    target: "animus.runtime.reattach",
                    socket = %socket_label,
                    error = %err,
                    "reattach acceptor loop exiting on accept error"
                );
                return;
            }
        };
        let handle = spawn_writer(stream, &socket_label);
        if let Ok(mut guard) = readers.lock() {
            guard.push(handle);
        }
    }
}

fn spawn_writer(mut stream: Stream, socket_label: &str) -> ReaderHandle {
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(BROADCAST_QUEUE_DEPTH);
    let label = socket_label.to_string();
    let _ =
        thread::Builder::new().name(format!("animus-reattach-writer:{}", short_label(socket_label))).spawn(move || {
            for bytes in rx.iter() {
                if stream.write_all(&bytes).is_err() {
                    tracing::debug!(
                        target: "animus.runtime.reattach",
                        socket = %label,
                        "reattach writer exiting on write error (reader closed)"
                    );
                    return;
                }
                if stream.flush().is_err() {
                    tracing::debug!(
                        target: "animus.runtime.reattach",
                        socket = %label,
                        "reattach writer exiting on flush error"
                    );
                    return;
                }
            }
        });
    ReaderHandle { sender: tx }
}

fn short_label(path: &str) -> String {
    Path::new(path).file_name().and_then(|s| s.to_str()).map(|s| s.to_string()).unwrap_or_else(|| {
        let trimmed = path.trim();
        if trimmed.len() > 16 {
            trimmed[trimmed.len() - 16..].to_string()
        } else {
            trimmed.to_string()
        }
    })
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_event_emitter::{RuntimeWorkflowEventKind, WireWorkflowEvent};
    use chrono::Utc;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream as StdUnixStream;
    use std::path::PathBuf;
    use std::time::Duration;
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
    fn bind_creates_socket_file_for_filesystem_path() {
        let (_dir, path) = temp_socket();
        let emitter = ReattachListenerEmitter::bind_path(&path).expect("bind");
        assert_eq!(emitter.socket_path(), path.to_string_lossy());
        assert!(path.exists(), "socket file must exist after bind");
    }

    #[test]
    fn bind_removes_stale_socket_at_path() {
        let (_dir, path) = temp_socket();
        std::fs::write(&path, b"stale").unwrap();
        let _emitter = ReattachListenerEmitter::bind_path(&path).expect("bind replaces stale");
        assert!(path.exists());
    }

    #[test]
    fn connected_reader_receives_broadcast_event() {
        let (_dir, path) = temp_socket();
        let emitter = ReattachListenerEmitter::bind_path(&path).expect("bind");

        let stream = StdUnixStream::connect(&path).expect("connect");
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut reader = BufReader::new(stream);

        std::thread::sleep(Duration::from_millis(50));

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
        let emitter = ReattachListenerEmitter::bind_path(&path).expect("bind");

        let first = StdUnixStream::connect(&path).expect("connect first");
        first.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut first_reader = BufReader::new(first);
        std::thread::sleep(Duration::from_millis(50));
        emitter.emit(sample_event("one"));
        let mut s1 = String::new();
        first_reader.read_line(&mut s1).expect("first reader event 1");
        assert!(s1.contains("wf-one"));

        drop(first_reader);
        emitter.emit(sample_event("gap-event"));

        let second = StdUnixStream::connect(&path).expect("connect second");
        second.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut second_reader = BufReader::new(second);
        std::thread::sleep(Duration::from_millis(50));
        emitter.emit(sample_event("after-reattach"));

        let mut s2 = String::new();
        second_reader.read_line(&mut s2).expect("second reader event 1");
        assert!(s2.contains("wf-after-reattach"), "second reader sees only post-reattach events: {s2}");
    }

    #[test]
    fn dropped_reader_does_not_block_emit() {
        let (_dir, path) = temp_socket();
        let emitter = ReattachListenerEmitter::bind_path(&path).expect("bind");
        let stream = StdUnixStream::connect(&path).expect("connect");
        std::thread::sleep(Duration::from_millis(50));
        drop(stream);
        for i in 0..10 {
            emitter.emit(sample_event(&format!("orphan-{i}")));
        }
    }

    #[test]
    fn stalled_reader_does_not_block_runner_emit() {
        // Codex round-1 P2 regression guard: if a reader connects but
        // never drains, the runner must NOT hang inside emit. With the
        // per-reader queue + writer-thread model, broadcast_line never
        // waits for the kernel pipe — once the per-reader queue fills,
        // the reader is dropped on the next try_send.
        let (_dir, path) = temp_socket();
        let emitter = ReattachListenerEmitter::bind_path(&path).expect("bind");

        let stream = StdUnixStream::connect(&path).expect("connect");
        std::thread::sleep(Duration::from_millis(50));

        let payload: String = "x".repeat(8 * 1024);
        let start = std::time::Instant::now();
        for i in 0..(BROADCAST_QUEUE_DEPTH * 4) {
            emitter.emit(RuntimeWorkflowEvent {
                workflow_id: format!("wf-stall-{i}"),
                kind: RuntimeWorkflowEventKind::PhaseStarted,
                payload: serde_json::json!({"big": payload.clone()}),
                occurred_at: Utc::now(),
            });
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "runner emit blocked too long; stalled reader was not dropped"
            );
        }

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

    #[test]
    fn local_socket_name_for_filesystem_and_namespaced_both_parse() {
        let _fs = local_socket_name_for("/tmp/animus-test.sock").expect("fs name");
        let _ns = local_socket_name_for("animus-test-pipe").expect("ns name");
    }
}

#[cfg(windows)]
#[cfg(test)]
mod windows_tests {
    use super::*;
    use crate::workflow_event_emitter::{RuntimeWorkflowEventKind, WireWorkflowEvent};
    use chrono::Utc;
    use interprocess::local_socket::traits::Stream as _;
    use std::io::{BufRead, BufReader};
    use std::sync::atomic::{AtomicU64, Ordering};

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    fn unique_pipe_name(label: &str) -> String {
        let id = UNIQUE.fetch_add(1, Ordering::SeqCst);
        format!("animus-reattach-test-{}-{}-{}", std::process::id(), id, label)
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
    fn bind_succeeds_on_namespaced_pipe_name() {
        let name = unique_pipe_name("bind");
        let emitter = ReattachListenerEmitter::bind(&name).expect("bind named pipe");
        assert_eq!(emitter.socket_path(), name);
    }

    #[test]
    fn connected_reader_receives_broadcast_event_via_named_pipe() {
        let name = unique_pipe_name("broadcast");
        let emitter = ReattachListenerEmitter::bind(&name).expect("bind");

        let resolved = local_socket_name_for(&name).expect("name");
        let stream = Stream::connect(resolved).expect("connect");
        let mut reader = BufReader::new(stream);

        std::thread::sleep(std::time::Duration::from_millis(50));

        emitter.emit(sample_event("win-alpha"));

        let mut line = String::new();
        reader.read_line(&mut line).expect("read line");
        let wire: WireWorkflowEvent = serde_json::from_str(line.trim()).expect("parse");
        assert_eq!(wire.workflow_id, "wf-win-alpha");
    }

    #[test]
    fn dropped_reader_does_not_block_emit_on_windows() {
        let name = unique_pipe_name("drop");
        let emitter = ReattachListenerEmitter::bind(&name).expect("bind");
        let resolved = local_socket_name_for(&name).expect("name");
        let stream = Stream::connect(resolved).expect("connect");
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(stream);
        for i in 0..10 {
            emitter.emit(sample_event(&format!("orphan-{i}")));
        }
    }

    #[test]
    fn from_env_returns_none_when_unset_on_windows() {
        let prev = std::env::var(ANIMUS_WORKFLOW_REATTACH_SOCKET_ENV).ok();
        std::env::remove_var(ANIMUS_WORKFLOW_REATTACH_SOCKET_ENV);
        assert!(ReattachListenerEmitter::from_env().is_none());
        if let Some(v) = prev {
            std::env::set_var(ANIMUS_WORKFLOW_REATTACH_SOCKET_ENV, v);
        }
    }
}
