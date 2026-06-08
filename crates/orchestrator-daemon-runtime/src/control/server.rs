//! [`ControlServer`] — the Unix-socket front door.
//!
//! Auto-starts at daemon launch (unless the operator sets
//! [`CONTROL_SERVER_DISABLE_ENV`]). Binds
//! `~/.animus/<repo-scope>/control.sock`, sets mode 0700, and accepts
//! newline-delimited JSON-RPC 2.0 connections. Each connection is handed
//! to [`super::ControlConnection`] which runs the per-client dispatch
//! loop.
//!
//! Anti-deadlock rules:
//!
//! - Server state is set once on `start` and never mutated. The
//!   shutdown signaler is a [`tokio::sync::broadcast::Sender`], also set
//!   once.
//! - No `Drop` impl holds a lock or awaits.
//! - The accept loop never holds any lock across `.await`.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use animus_control_protocol::ControlSurface;
use thiserror::Error;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

#[cfg(unix)]
use super::connection::ControlConnection;
use super::policy::{ConnectionPrincipal, PolicyState};
use super::workflow_events::WorkflowEventBroadcaster;
use orchestrator_plugin_host::PluginStatusRegistry;

/// Environment variable that disables the control server entirely when
/// set to a truthy value.
///
/// Honored at daemon startup. Useful for testing the in-process fallback
/// path in CLI/MCP while the v0.4.0 control-protocol migration is in
/// flight, and as a fast circuit-breaker if a buggy connection handler
/// ever ships.
pub const CONTROL_SERVER_DISABLE_ENV: &str = "ANIMUS_DAEMON_DISABLE_CONTROL_SERVER";

/// Returns `true` when [`CONTROL_SERVER_DISABLE_ENV`] is set to a truthy
/// value.
///
/// Mirrors the truthy parse used by the subject / log-storage / trigger
/// disable knobs: empty / `"0"` / `"false"` / `"no"` / `"off"` are false;
/// anything else is true.
pub fn control_server_disable_env_set() -> bool {
    match std::env::var(CONTROL_SERVER_DISABLE_ENV) {
        Ok(value) => {
            let trimmed = value.trim().to_ascii_lowercase();
            !trimmed.is_empty() && trimmed != "0" && trimmed != "false" && trimmed != "no" && trimmed != "off"
        }
        Err(_) => false,
    }
}

/// Errors surfaced by [`ControlServer`] lifecycle calls.
#[derive(Debug, Error)]
pub enum ControlError {
    /// Could not bind the listener socket.
    #[error("control server: failed to bind {path}: {source}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Could not create the parent directory for the socket.
    #[error("control server: failed to create socket dir {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Could not set permissions on the socket file.
    #[error("control server: failed to chmod {path} to 0700: {source}")]
    Chmod {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Could not remove a stale socket file at the target path.
    #[error("control server: failed to remove stale socket {path}: {source}")]
    RemoveStale {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Project root could not be resolved to a scoped state dir and no
    /// fallback `.animus` directory was reachable.
    #[error("control server: could not resolve socket path for project root {project_root}")]
    ResolveSocketPath { project_root: PathBuf },

    /// The control server is not supported on this platform (the
    /// daemon's wire surface is currently Unix-socket only). Callers
    /// should fall back to the in-process service path.
    #[error("control server: {0}")]
    Unavailable(&'static str),
}

/// Compute the Unix-socket path for `project_root`.
///
/// Prefers the scoped state root `~/.animus/<repo-scope>/control.sock`.
/// Falls back to the project-local `.animus/control.sock` when the
/// scoped root cannot be resolved (e.g. `$HOME` is unavailable in a
/// sandboxed test).
pub fn control_socket_path(project_root: &Path) -> PathBuf {
    protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".animus")).join("control.sock")
}

/// Background-task handle for a running [`ControlServer`].
///
/// Dropping this aborts the accept loop without sending the graceful
/// shutdown signal — prefer [`ControlServerHandle::shutdown`] instead.
pub struct ControlServerHandle {
    socket_path: PathBuf,
    accept_task: Option<JoinHandle<()>>,
    shutdown_tx: broadcast::Sender<()>,
}

impl ControlServerHandle {
    /// The bound socket path. Useful for emitting
    /// [`crate::DaemonRunEvent::ControlServerResolved`].
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Signal the accept loop to stop, wait for in-flight connections to
    /// finish, then remove the socket file.
    pub async fn shutdown(mut self) -> Result<(), ControlError> {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.accept_task.take() {
            handle.abort();
            let _ = handle.await;
        }
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)
                .map_err(|e| ControlError::RemoveStale { path: self.socket_path.clone(), source: e })?;
        }
        Ok(())
    }
}

impl Drop for ControlServerHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.accept_task.take() {
            handle.abort();
        }
        // Best-effort cleanup; ignore errors so panics during drop are
        // impossible.
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

/// The daemon-side control RPC server.
///
/// Construct via [`ControlServer::start`]. The returned
/// [`ControlServerHandle`] owns the accept-loop task; call
/// [`ControlServerHandle::shutdown`] at daemon shutdown.
pub struct ControlServer;

impl ControlServer {
    /// Bind the socket at `control_socket_path(project_root)`, spawn the
    /// accept loop, and return a [`ControlServerHandle`].
    ///
    /// Removes any pre-existing socket file at the target path before
    /// binding (e.g. left over from a crashed daemon). Sets mode 0700
    /// on the bound socket so only the owning UID can connect.
    pub async fn start(
        project_root: &Path,
        surface: Arc<dyn ControlSurface>,
    ) -> Result<ControlServerHandle, ControlError> {
        let socket_path = control_socket_path(project_root);
        Self::start_with_socket(socket_path, surface).await
    }

    /// Like [`Self::start`] but also wires a [`WorkflowEventBroadcaster`]
    /// for the `workflow/events` subscription method. Without this hook
    /// `workflow/events` returns `internal_error` on subscribe (the
    /// in-tree v0.1.10 `ControlSurface` does not yet implement it).
    pub async fn start_with_workflow_events(
        project_root: &Path,
        surface: Arc<dyn ControlSurface>,
        broadcaster: Arc<WorkflowEventBroadcaster>,
    ) -> Result<ControlServerHandle, ControlError> {
        let socket_path = control_socket_path(project_root);
        Self::start_with_socket_and_workflow_events(socket_path, surface, Some(broadcaster)).await
    }

    /// v0.5.8: wire the RBAC policy (chokepoint #1) alongside the
    /// workflow-event broadcaster. The accept loop derives a
    /// [`ConnectionPrincipal`] from peer credentials per connection
    /// and threads it through the dispatch hook.
    pub async fn start_with_policy(
        project_root: &Path,
        surface: Arc<dyn ControlSurface>,
        broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
        policy: PolicyState,
    ) -> Result<ControlServerHandle, ControlError> {
        Self::start_with_policy_and_observability(project_root, surface, broadcaster, policy, None).await
    }

    /// Like [`Self::start_with_workflow_events`] but additionally wires a
    /// [`PluginStatusRegistry`] for the `plugin/status` RPC method.
    pub async fn start_with_observability(
        project_root: &Path,
        surface: Arc<dyn ControlSurface>,
        broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
        status_registry: Option<Arc<PluginStatusRegistry>>,
    ) -> Result<ControlServerHandle, ControlError> {
        Self::start_with_policy_and_observability(
            project_root,
            surface,
            broadcaster,
            PolicyState::single_user(),
            status_registry,
        )
        .await
    }

    /// v0.5.8 canonical full-featured constructor: RBAC policy + workflow
    /// event broadcaster + plugin status registry, all wired through the
    /// accept loop and each spawned [`ControlConnection`].
    pub async fn start_with_policy_and_observability(
        project_root: &Path,
        surface: Arc<dyn ControlSurface>,
        broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
        policy: PolicyState,
        status_registry: Option<Arc<PluginStatusRegistry>>,
    ) -> Result<ControlServerHandle, ControlError> {
        let socket_path = control_socket_path(project_root);
        Self::start_with_socket_full(socket_path, surface, broadcaster, policy, status_registry).await
    }

    /// Bind at an explicit socket path. Used by tests where the
    /// scoped-state-root resolution would produce a path too long for
    /// `SUN_LEN`, and as the underlying primitive for [`Self::start`].
    ///
    /// On non-Unix targets this returns [`ControlError::Unavailable`];
    /// the daemon treats that as "no control server, warn and continue"
    /// so the in-process service path keeps working.
    #[cfg(unix)]
    pub async fn start_with_socket(
        socket_path: PathBuf,
        surface: Arc<dyn ControlSurface>,
    ) -> Result<ControlServerHandle, ControlError> {
        Self::start_with_socket_and_workflow_events(socket_path, surface, None).await
    }

    #[cfg(unix)]
    pub async fn start_with_socket_and_workflow_events(
        socket_path: PathBuf,
        surface: Arc<dyn ControlSurface>,
        broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
    ) -> Result<ControlServerHandle, ControlError> {
        Self::start_with_socket_full(socket_path, surface, broadcaster, PolicyState::single_user(), None).await
    }

    /// Bind, set perms, and run the accept loop with an explicit RBAC
    /// [`PolicyState`] propagated to each connection.
    #[cfg(unix)]
    pub async fn start_with_socket_policy_and_workflow_events(
        socket_path: PathBuf,
        surface: Arc<dyn ControlSurface>,
        broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
        policy: PolicyState,
    ) -> Result<ControlServerHandle, ControlError> {
        Self::start_with_socket_full(socket_path, surface, broadcaster, policy, None).await
    }

    #[cfg(unix)]
    pub async fn start_with_socket_and_observability(
        socket_path: PathBuf,
        surface: Arc<dyn ControlSurface>,
        broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
        status_registry: Option<Arc<PluginStatusRegistry>>,
    ) -> Result<ControlServerHandle, ControlError> {
        Self::start_with_socket_full(socket_path, surface, broadcaster, PolicyState::single_user(), status_registry)
            .await
    }

    /// v0.5.8 canonical full-featured socket-level constructor.
    #[cfg(unix)]
    pub async fn start_with_socket_full(
        socket_path: PathBuf,
        surface: Arc<dyn ControlSurface>,
        broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
        policy: PolicyState,
        status_registry: Option<Arc<PluginStatusRegistry>>,
    ) -> Result<ControlServerHandle, ControlError> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ControlError::CreateDir { path: parent.to_path_buf(), source: e })?;
        }
        if socket_path.exists() {
            std::fs::remove_file(&socket_path)
                .map_err(|e| ControlError::RemoveStale { path: socket_path.clone(), source: e })?;
        }
        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| ControlError::Bind { path: socket_path.clone(), source: e })?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| ControlError::Chmod { path: socket_path.clone(), source: e })?;

        let (shutdown_tx, _shutdown_rx) = broadcast::channel::<()>(8);
        let accept_task = tokio::spawn(accept_loop(
            listener,
            surface,
            shutdown_tx.subscribe(),
            socket_path.clone(),
            broadcaster,
            policy,
            status_registry,
        ));

        Ok(ControlServerHandle { socket_path, accept_task: Some(accept_task), shutdown_tx })
    }

    /// Non-Unix stub. The control server is Unix-domain-socket only;
    /// Windows callers receive [`ControlError::Unavailable`] and the
    /// daemon falls back to in-process service dispatch.
    #[cfg(not(unix))]
    pub async fn start_with_socket(
        _socket_path: PathBuf,
        _surface: Arc<dyn ControlSurface>,
    ) -> Result<ControlServerHandle, ControlError> {
        Err(ControlError::Unavailable("control server not supported on this platform"))
    }

    #[cfg(not(unix))]
    pub async fn start_with_socket_and_workflow_events(
        _socket_path: PathBuf,
        _surface: Arc<dyn ControlSurface>,
        _broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
    ) -> Result<ControlServerHandle, ControlError> {
        Err(ControlError::Unavailable("control server not supported on this platform"))
    }

    #[cfg(not(unix))]
    pub async fn start_with_socket_policy_and_workflow_events(
        _socket_path: PathBuf,
        _surface: Arc<dyn ControlSurface>,
        _broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
        _policy: PolicyState,
    ) -> Result<ControlServerHandle, ControlError> {
        Err(ControlError::Unavailable("control server not supported on this platform"))
    }

    #[cfg(not(unix))]
    pub async fn start_with_socket_and_observability(
        _socket_path: PathBuf,
        _surface: Arc<dyn ControlSurface>,
        _broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
        _status_registry: Option<Arc<PluginStatusRegistry>>,
    ) -> Result<ControlServerHandle, ControlError> {
        Err(ControlError::Unavailable("control server not supported on this platform"))
    }

    #[cfg(not(unix))]
    pub async fn start_with_socket_full(
        _socket_path: PathBuf,
        _surface: Arc<dyn ControlSurface>,
        _broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
        _policy: PolicyState,
        _status_registry: Option<Arc<PluginStatusRegistry>>,
    ) -> Result<ControlServerHandle, ControlError> {
        Err(ControlError::Unavailable("control server not supported on this platform"))
    }
}

/// Background task: accept connections until the shutdown signal fires.
///
/// Each accepted connection is moved into a fresh [`tokio::spawn`] task
/// running [`ControlConnection::serve`]. The accept loop never blocks
/// on a single connection; connection-handler errors are logged via
/// `tracing` (not yet wired through the daemon's structured event hook —
/// that's part of the C5 ↔ daemon hook plumbing).
#[cfg(unix)]
async fn accept_loop(
    listener: UnixListener,
    surface: Arc<dyn ControlSurface>,
    mut shutdown_rx: broadcast::Receiver<()>,
    socket_path: PathBuf,
    broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
    policy: PolicyState,
    status_registry: Option<Arc<PluginStatusRegistry>>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                tracing::debug!(
                    target: "animus.control.server",
                    path = %socket_path.display(),
                    "control server shutdown signal received; stopping accept loop"
                );
                return;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _peer)) => {
                        let surface = Arc::clone(&surface);
                        let broadcaster_clone = broadcaster.clone();
                        let policy_clone = policy.clone();
                        let peer_os_user = resolve_peer_os_user(&stream);
                        let principal = match peer_os_user {
                            Some(user) => Arc::new(ConnectionPrincipal::from_peer_os_user(user, &policy_clone)),
                            None => Arc::new(ConnectionPrincipal::anonymous()),
                        };
                        let status_registry_clone = status_registry.clone();
                        tokio::spawn(async move {
                            let mut connection = ControlConnection::new(stream, surface)
                                .with_policy(policy_clone)
                                .with_principal(principal);
                            if let Some(b) = broadcaster_clone {
                                connection = connection.with_workflow_event_broadcaster(b);
                            }
                            if let Some(r) = status_registry_clone {
                                connection = connection.with_plugin_status_registry(r);
                            }
                            if let Err(err) = connection.serve().await {
                                tracing::debug!(
                                    target: "animus.control.server",
                                    error = %err,
                                    "control connection ended with error"
                                );
                            }
                        });
                    }
                    Err(err) => {
                        tracing::warn!(
                            target: "animus.control.server",
                            error = %err,
                            "control server accept failed; continuing"
                        );
                    }
                }
            }
        }
    }
}

/// Resolve the peer credential of an accepted Unix-socket connection
/// to a username via `getpeereid(2)` (BSD / macOS) or `SO_PEERCRED`
/// (Linux) plus the password database.
///
/// Returns `None` when the lookup fails (uid 0 with no passwd entry,
/// musl static builds where `getpwuid` is not linked, etc.). The
/// caller then falls back to an anonymous [`ConnectionPrincipal`] —
/// under `RbacMode::Enforce` that yields a permission_denied response
/// on the first dispatch, which is exactly the fail-closed shape the
/// design doc calls for.
#[cfg(unix)]
fn resolve_peer_os_user(stream: &tokio::net::UnixStream) -> Option<String> {
    let uid = peer_uid_for_stream(stream)?;
    lookup_username_for_uid(uid)
}

#[cfg(all(
    unix,
    any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        target_os = "ios"
    )
))]
#[allow(unsafe_code)]
fn peer_uid_for_stream(stream: &tokio::net::UnixStream) -> Option<libc::uid_t> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: `fd` is a valid file descriptor for as long as `stream`
    // is alive (we hold a borrow). `getpeereid` writes through valid
    // pointers to stack-local uid/gid.
    let rc = unsafe { libc::getpeereid(fd, &raw mut uid, &raw mut gid) };
    if rc != 0 {
        tracing::debug!(
            target: "animus.control.server",
            errno = std::io::Error::last_os_error().raw_os_error(),
            "getpeereid failed; falling back to anonymous principal"
        );
        return None;
    }
    Some(uid)
}

#[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
#[allow(unsafe_code)]
fn peer_uid_for_stream(stream: &tokio::net::UnixStream) -> Option<libc::uid_t> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut ucred: libc::ucred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len: libc::socklen_t = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `fd` is a valid descriptor; `&raw mut ucred` is a valid
    // out-pointer; `&raw mut len` is a valid in/out length pointer.
    let rc = unsafe {
        libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut ucred).cast::<libc::c_void>(), &raw mut len)
    };
    if rc != 0 {
        tracing::debug!(
            target: "animus.control.server",
            errno = std::io::Error::last_os_error().raw_os_error(),
            "SO_PEERCRED failed; falling back to anonymous principal"
        );
        return None;
    }
    Some(ucred.uid)
}

#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        target_os = "ios",
        target_os = "linux",
        target_os = "android",
    ))
))]
fn peer_uid_for_stream(_stream: &tokio::net::UnixStream) -> Option<libc::uid_t> {
    // Other Unix targets (Solaris, AIX, etc.) have neither getpeereid
    // nor SO_PEERCRED in the shape libc exposes; fall back to anonymous.
    tracing::debug!(
        target: "animus.control.server",
        "peer credential lookup unsupported on this Unix target; falling back to anonymous principal"
    );
    None
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn lookup_username_for_uid(uid: libc::uid_t) -> Option<String> {
    // SAFETY: `getpwuid` returns a pointer to a static struct or null.
    // We immediately copy the `pw_name` C string into an owned String,
    // never holding the pointer past the call.
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return None;
        }
        let name_ptr = (*pw).pw_name;
        if name_ptr.is_null() {
            return None;
        }
        let cstr = std::ffi::CStr::from_ptr(name_ptr);
        cstr.to_str().ok().map(str::to_string)
    }
}
