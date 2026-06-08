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

    /// Like [`Self::start_with_workflow_events`] but additionally wires a
    /// [`PluginStatusRegistry`] for the `plugin/status` RPC method.
    pub async fn start_with_observability(
        project_root: &Path,
        surface: Arc<dyn ControlSurface>,
        broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
        status_registry: Option<Arc<PluginStatusRegistry>>,
    ) -> Result<ControlServerHandle, ControlError> {
        let socket_path = control_socket_path(project_root);
        Self::start_with_socket_and_observability(socket_path, surface, broadcaster, status_registry).await
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
        Self::start_with_socket_and_observability(socket_path, surface, broadcaster, None).await
    }

    #[cfg(unix)]
    pub async fn start_with_socket_and_observability(
        socket_path: PathBuf,
        surface: Arc<dyn ControlSurface>,
        broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
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
    pub async fn start_with_socket_and_observability(
        _socket_path: PathBuf,
        _surface: Arc<dyn ControlSurface>,
        _broadcaster: Option<Arc<WorkflowEventBroadcaster>>,
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
                        let status_registry_clone = status_registry.clone();
                        tokio::spawn(async move {
                            let mut connection = ControlConnection::new(stream, surface);
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
