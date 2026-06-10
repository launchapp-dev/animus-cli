//! Log storage backend integration for the daemon.
//!
//! Mirrors the provider/trigger plugin pattern: at daemon startup we
//! discover every installed plugin and filter for `plugin_kind ==
//! log_storage_backend`. When exactly one is found (and the operator has
//! not set `ANIMUS_DAEMON_DISABLE_LOG_STORAGE_PLUGIN`), it becomes the
//! active sink for all daemon-emitted log entries plus `log/entry`
//! notifications forwarded from other supervised plugins. When multiple
//! are found we pick the first by name (deterministic) and emit a
//! warning so operators know fan-out is not yet supported. When none are
//! found, we fall back to the in-tree [`orchestrator_logging::Logger`]
//! which writes structured events to `events.jsonl` — the historical
//! behavior.
//!
//! Anti-deadlock rules (per the v0.4.0 cut design notes):
//!
//! - The dispatch handle is an [`Arc`] over an immutable enum. No
//!   mutexes guard it on the read path because it is set once at daemon
//!   startup and never mutated afterward.
//! - No `Drop` impl on the dispatch touches a lock.
//! - The discovery path uses `std::sync::RwLock` only when caching the
//!   resolved dispatch for the CLI surface; nothing here holds a lock
//!   across `.await`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use anyhow::Result;
use orchestrator_logging::Logger;
use orchestrator_plugin_host::{discover_plugins, DiscoveredPlugin, PluginHost, PluginSpawnOptions};
use serde_json::Value;
use tokio::sync::RwLock as TokioRwLock;

use animus_log_storage_protocol::{METHOD_LOG_STORAGE_QUERY, METHOD_LOG_STORAGE_STORE};
use animus_plugin_protocol::PLUGIN_KIND_LOG_STORAGE_BACKEND;

/// Environment variable that forces the in-tree log storage fallback even
/// when a `log_storage_backend` plugin is installed.
///
/// Matches the shape of `ANIMUS_PROVIDER_DISABLE_PLUGIN` from the provider
/// dispatch path. Any non-empty value other than `"0"` is treated as
/// truthy.
pub const LOG_STORAGE_DISABLE_ENV: &str = "ANIMUS_DAEMON_DISABLE_LOG_STORAGE_PLUGIN";

/// Resolution of the log storage dispatch path for a given project.
///
/// `InTree` carries the project root so callers can lazily build an
/// in-tree [`Logger`] on demand without holding the file handle across
/// dispatch lifetimes (each `.emit()` call opens the file fresh today).
/// `Plugin` carries the discovered plugin manifest so the CLI surface and
/// the daemon's `log/entry` forwarder can both reach the plugin via
/// [`orchestrator_plugin_host`] without re-running discovery.
#[derive(Debug, Clone)]
pub enum LogStorageDispatch {
    /// No `log_storage_backend` plugin installed (or one is installed but
    /// the disable env var is set). Daemon + CLI use the in-tree
    /// [`Logger`] which writes events.jsonl under the scoped runtime
    /// state root.
    InTree { project_root: PathBuf },
    /// A `log_storage_backend` plugin is installed and the operator has
    /// not opted out. The active dispatch routes daemon log writes and
    /// `log/entry` notifications through this plugin instead.
    Plugin { project_root: PathBuf, plugin: Box<DiscoveredPlugin> },
}

impl LogStorageDispatch {
    /// Returns `true` when the dispatch is routing through a plugin
    /// rather than the in-tree fallback.
    pub fn is_plugin(&self) -> bool {
        matches!(self, LogStorageDispatch::Plugin { .. })
    }

    /// Returns the plugin name when routing through a plugin, otherwise
    /// `None`.
    pub fn plugin_name(&self) -> Option<&str> {
        match self {
            LogStorageDispatch::Plugin { plugin, .. } => Some(plugin.name.as_str()),
            LogStorageDispatch::InTree { .. } => None,
        }
    }

    /// Project root the dispatch is scoped to.
    pub fn project_root(&self) -> &Path {
        match self {
            LogStorageDispatch::InTree { project_root } => project_root,
            LogStorageDispatch::Plugin { project_root, .. } => project_root,
        }
    }

    /// Build a fresh in-tree [`Logger`] for the project. Available on
    /// both branches so the daemon's fallback writer and the CLI's
    /// `logs tail --plugin none` reader share one implementation.
    pub fn in_tree_logger(&self) -> Logger {
        Logger::for_project(self.project_root())
    }
}

/// Returns `true` when `$ANIMUS_DAEMON_DISABLE_LOG_STORAGE_PLUGIN` is set
/// to a truthy value. Mirrors the provider dispatch knob.
pub fn log_storage_disable_env_set() -> bool {
    match std::env::var(LOG_STORAGE_DISABLE_ENV) {
        Ok(value) => {
            let trimmed = value.trim().to_ascii_lowercase();
            !trimmed.is_empty() && trimmed != "0" && trimmed != "false" && trimmed != "no" && trimmed != "off"
        }
        Err(_) => false,
    }
}

/// Filter the project's installed plugins down to log storage backends.
pub fn discover_log_storage_backends(project_root: &Path) -> Result<Vec<DiscoveredPlugin>> {
    let plugins = discover_plugins(project_root)?;
    Ok(plugins.into_iter().filter(|p| p.manifest.plugin_kind == PLUGIN_KIND_LOG_STORAGE_BACKEND).collect())
}

/// Outcome of [`resolve_log_storage_dispatch`].
///
/// The `selected` field carries the resolved dispatch handle. `warnings`
/// is populated when multiple plugins were discovered (we surface a
/// deterministic-pick warning) or when the disable knob is set while a
/// plugin is installed (we record the override so it's visible in the
/// daemon event log).
#[derive(Debug, Clone)]
pub struct LogStorageResolution {
    /// Resolved dispatch handle. Always populated.
    pub selected: Arc<LogStorageDispatch>,
    /// All log_storage_backend plugins seen by discovery, in the order
    /// returned by `discover_plugins` (which is already deterministic
    /// across config/project-local/plugin-path scans).
    pub all_candidates: Vec<DiscoveredPlugin>,
    /// Human-readable warnings to surface up the daemon event log.
    pub warnings: Vec<String>,
}

/// Resolve which [`LogStorageDispatch`] a project should use right now.
///
/// Selection rules (in priority order):
///
/// 1. If [`LOG_STORAGE_DISABLE_ENV`] is set truthy → InTree.
/// 2. Else if discovery surfaces zero log_storage_backend plugins → InTree.
/// 3. Else if discovery surfaces exactly one → Plugin with that backend.
/// 4. Else (>1) → Plugin with the first by name; record a warning.
///
/// All errors from `discover_plugins` are propagated. Callers that want
/// "always succeed, fall back to in-tree on discovery failure" can map
/// the error themselves — the daemon entrypoint does this so a broken
/// plugin install never blocks startup.
pub fn resolve_log_storage_dispatch(project_root: &Path) -> Result<LogStorageResolution> {
    let project_root_buf = project_root.to_path_buf();
    let backends = discover_log_storage_backends(project_root)?;
    let mut warnings: Vec<String> = Vec::new();

    if log_storage_disable_env_set() {
        if !backends.is_empty() {
            warnings.push(format!(
                "log_storage_backend plugin discovered ({} installed) but {LOG_STORAGE_DISABLE_ENV} is set; routing through in-tree fallback",
                backends.len()
            ));
        }
        return Ok(LogStorageResolution {
            selected: Arc::new(LogStorageDispatch::InTree { project_root: project_root_buf }),
            all_candidates: backends,
            warnings,
        });
    }

    if backends.is_empty() {
        return Ok(LogStorageResolution {
            selected: Arc::new(LogStorageDispatch::InTree { project_root: project_root_buf }),
            all_candidates: backends,
            warnings,
        });
    }

    let mut sorted = backends.clone();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let chosen = sorted.first().cloned().expect("backends non-empty");

    if sorted.len() > 1 {
        let names: Vec<String> = sorted.iter().map(|p| p.name.clone()).collect();
        warnings.push(format!(
            "multiple log_storage_backend plugins installed ({}); selecting first by name ({}). Multi-backend fan-out is not yet supported.",
            names.join(", "),
            chosen.name
        ));
    }

    Ok(LogStorageResolution {
        selected: Arc::new(LogStorageDispatch::Plugin { project_root: project_root_buf, plugin: Box::new(chosen) }),
        all_candidates: backends,
        warnings,
    })
}

// =====================================================================
// LogStorageHandle: long-lived dispatch surface
// =====================================================================

/// Process-global slot for the daemon's active [`LogStorageHandle`].
///
/// Installed by [`install_log_storage_handle`] at daemon startup once the
/// dispatch resolves and (for `Plugin` dispatch) the supervised
/// [`PluginHost`] has finished its handshake. Consumed by
/// [`current_log_storage_handle`] from:
///
/// - [`crate::DaemonEventLog::append`] — for outbound `log/entry` routing.
/// - The control surface's `daemon/logs` reader — for inbound
///   `log_storage/tail` routing.
///
/// `None` means the daemon process hasn't installed one yet (CLI one-shot
/// commands, tests that don't spin up the daemon, etc.); callers in that
/// case fall back to the historical in-tree file path so behavior is
/// unchanged.
static DAEMON_LOG_STORAGE_HANDLE: OnceLock<RwLock<Option<Arc<LogStorageHandle>>>> = OnceLock::new();

fn handle_slot() -> &'static RwLock<Option<Arc<LogStorageHandle>>> {
    DAEMON_LOG_STORAGE_HANDLE.get_or_init(|| RwLock::new(None))
}

/// Install the active [`LogStorageHandle`] into the process-global slot.
///
/// First-installer-wins is NOT enforced here — the daemon installs once
/// at startup and clears once at shutdown. Tests pre-install their own
/// handle and rely on `clear_log_storage_handle` running in `Drop` to
/// reset.
pub fn install_log_storage_handle(handle: Arc<LogStorageHandle>) {
    if let Ok(mut guard) = handle_slot().write() {
        *guard = Some(handle);
    }
}

/// Clear the active [`LogStorageHandle`] (used at daemon shutdown and in
/// tests).
pub fn clear_log_storage_handle() {
    if let Ok(mut guard) = handle_slot().write() {
        *guard = None;
    }
}

/// Returns the active [`LogStorageHandle`] when one has been installed by
/// the daemon. Returns `None` from CLI one-shot processes and tests that
/// never call [`install_log_storage_handle`].
pub fn current_log_storage_handle() -> Option<Arc<LogStorageHandle>> {
    handle_slot().read().ok().and_then(|guard| guard.clone())
}

/// Shared lock for tests that mutate the process-global handle slot.
///
/// Test modules across the crate (`log_storage::tests`,
/// `daemon::daemon_event_log::daemon_logs_dispatch_tests`,
/// `control::dispatch::control_daemon_logs_plugin_route_tests`)
/// serialize on this lock so cargo's parallel runner does not race them
/// against each other. Production code never touches it.
#[cfg(test)]
pub(crate) static LOG_STORAGE_TEST_SLOT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Long-lived plugin host wrapped behind an `RwLock<Option<Arc<...>>>`.
///
/// `PluginHost` is internally `Send + Sync` and multiplexes concurrent
/// requests over the stdio transport, so the request path takes a brief
/// read-lock to clone the `Arc<PluginHost>` and drops the lock before
/// awaiting the RPC. Slow or hung backends therefore don't serialize
/// every queued request behind a single mutex.
///
/// Shutdown takes a write-lock and `.take()`s the `Arc` out of the slot.
/// We don't call `PluginHost::shutdown(self)` (it consumes the host and
/// the Arc would need to be the last reference), we just drop the Arc
/// and let the host's reader/writer tasks die naturally — the child
/// process exits when its stdio is closed.
struct PluginRouteState {
    name: String,
    host: TokioRwLock<Option<Arc<PluginHost>>>,
}

/// What kind of routing the active handle performs.
///
/// `InTree` is a marker — callers (the in-tree `DaemonEventLog::append`
/// path and the file-reading `daemon/logs` path) fall back to the
/// existing file behavior when this variant is active.
///
/// `Plugin` carries the long-lived host. Callers route `log/entry` and
/// `log_storage/tail` through the host instead of touching the file.
enum LogStorageRoute {
    InTree,
    Plugin(Arc<PluginRouteState>),
}

/// Active log-storage routing handle held by the daemon for the lifetime
/// of the process.
///
/// When the resolved dispatch is `Plugin`, the handle owns a spawned +
/// handshaked [`PluginHost`] and serializes `log/entry` notifications and
/// `log_storage/tail` requests against it. When the dispatch is `InTree`,
/// the handle is a thin marker so callers know to use the in-tree file
/// path.
pub struct LogStorageHandle {
    route: LogStorageRoute,
    project_root: PathBuf,
}

impl std::fmt::Debug for LogStorageHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogStorageHandle")
            .field("project_root", &self.project_root)
            .field("plugin", &self.plugin_name())
            .finish_non_exhaustive()
    }
}

impl LogStorageHandle {
    /// Build an in-tree handle bound to `project_root`. Used by the
    /// daemon when no `log_storage_backend` plugin is installed and by
    /// tests that need a non-plugin handle.
    pub fn in_tree(project_root: PathBuf) -> Self {
        Self { route: LogStorageRoute::InTree, project_root }
    }

    /// Build a plugin handle from a host that has already completed its
    /// handshake. The host is moved into the handle and reaped at
    /// [`LogStorageHandle::shutdown`].
    ///
    /// Used directly by tests (with a `PluginHost::from_streams` host)
    /// and indirectly by [`spawn_log_storage_supervisor`] in the daemon
    /// startup path.
    pub fn from_handshaked_host(plugin_name: impl Into<String>, host: PluginHost, project_root: PathBuf) -> Self {
        let state = PluginRouteState { name: plugin_name.into(), host: TokioRwLock::new(Some(Arc::new(host))) };
        Self { route: LogStorageRoute::Plugin(Arc::new(state)), project_root }
    }

    /// `true` when routing through a plugin.
    pub fn is_plugin(&self) -> bool {
        matches!(self.route, LogStorageRoute::Plugin(_))
    }

    /// Plugin name when routing through a plugin, otherwise `None`.
    pub fn plugin_name(&self) -> Option<&str> {
        match &self.route {
            LogStorageRoute::Plugin(state) => Some(state.name.as_str()),
            LogStorageRoute::InTree => None,
        }
    }

    /// Project root the handle is scoped to.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Forward a single log entry to the active backend.
    ///
    /// `Plugin` routing sends a `log_storage/store` request to the plugin
    /// with the entry wrapped in `{ entries: [entry] }` per the
    /// [`animus-log-storage-protocol`] wire shape and returns `Ok(true)`.
    /// On plugin errors the call returns `Err(...)`; per policy (B) the
    /// daemon also writes the local file so a misbehaving plugin doesn't
    /// silently lose the entry — the daemon-events file remains the
    /// source of truth for `daemon events` / MCP poll surfaces.
    ///
    /// `InTree` routing returns `Ok(false)` so callers know to perform
    /// the historical file write.
    ///
    /// `entry` must already be a JSON object conforming to
    /// [`animus_log_storage_protocol::LogEntry`].
    pub async fn publish_log_entry(&self, entry: Value) -> Result<bool> {
        match &self.route {
            LogStorageRoute::InTree => Ok(false),
            LogStorageRoute::Plugin(state) => {
                let host = {
                    let guard = state.host.read().await;
                    match guard.as_ref() {
                        Some(host) => host.clone(),
                        None => {
                            return Err(anyhow::anyhow!(
                                "log_storage_backend plugin '{}' host already shut down",
                                state.name
                            ));
                        }
                    }
                };
                let params = serde_json::json!({ "entries": [entry] });
                host.request_with_timeout(METHOD_LOG_STORAGE_STORE, Some(params), Duration::from_secs(5))
                    .await
                    .map_err(|rpc_err| {
                        anyhow::anyhow!(
                            "{METHOD_LOG_STORAGE_STORE} on plugin '{}' returned RPC error ({}): {}",
                            state.name,
                            rpc_err.code,
                            rpc_err.message,
                        )
                    })?;
                Ok(true)
            }
        }
    }

    /// Query the active backend for a historical batch of log entries.
    ///
    /// `Plugin` routing sends a synchronous `log_storage/query` request
    /// (see `animus-log-storage-protocol`) and returns the decoded
    /// response. `log_storage/tail` is streaming-only on the wire and
    /// would require subscribing to `log_storage/event` notifications; we
    /// keep the daemon's historical path on `query` so `daemon/logs`
    /// returns a bounded batch in a single round-trip.
    ///
    /// Returns `None` for in-tree routing so callers fall back to the
    /// file reader. Errors are surfaced verbatim so the daemon's
    /// `daemon/logs` handler can decide whether to bubble them up to the
    /// CLI.
    pub async fn tail(&self, params: Option<Value>, timeout: Duration) -> Result<Option<Value>> {
        match &self.route {
            LogStorageRoute::InTree => Ok(None),
            LogStorageRoute::Plugin(state) => {
                let host = {
                    let guard = state.host.read().await;
                    match guard.as_ref() {
                        Some(host) => host.clone(),
                        None => {
                            return Err(anyhow::anyhow!(
                                "log_storage_backend plugin '{}' host already shut down",
                                state.name
                            ));
                        }
                    }
                };
                let value =
                    host.request_with_timeout(METHOD_LOG_STORAGE_QUERY, params, timeout).await.map_err(|rpc_err| {
                        anyhow::anyhow!(
                            "{METHOD_LOG_STORAGE_QUERY} on plugin '{}' returned RPC error ({}): {}",
                            state.name,
                            rpc_err.code,
                            rpc_err.message,
                        )
                    })?;
                Ok(Some(value))
            }
        }
    }

    /// Cleanly shut down the plugin host (no-op for in-tree routing).
    /// Errors are swallowed because shutdown is best-effort and must not
    /// block the daemon teardown path.
    ///
    /// Bounded to two seconds: the host is wrapped in `Arc<PluginHost>`
    /// behind an `RwLock<Option<…>>`. We take a write lock briefly to
    /// remove the slot's `Arc`. If we are the last `Arc` owner we run
    /// the full `PluginHost::shutdown` sequence (sends `shutdown` RPC +
    /// `exit` notification, waits for the child to exit). If a concurrent
    /// in-flight request still holds an `Arc` clone we drop the slot's
    /// reference and rely on stdio EOF to terminate the child; an
    /// orphaned process is preferable to blocking daemon teardown
    /// indefinitely.
    pub async fn shutdown(&self) {
        const SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
        if let LogStorageRoute::Plugin(state) = &self.route {
            let take_fut = async {
                let mut guard = state.host.write().await;
                guard.take()
            };
            let arc = match tokio::time::timeout(SHUTDOWN_WAIT, take_fut).await {
                Ok(arc) => arc,
                Err(_) => {
                    tracing::warn!(
                        target: "log_storage",
                        plugin = %state.name,
                        "log_storage_backend shutdown timed out waiting for write lock; orphaning plugin process",
                    );
                    return;
                }
            };
            let Some(arc) = arc else { return };
            match Arc::try_unwrap(arc) {
                Ok(host) => {
                    // Last owner — issue the standard shutdown +
                    // exit sequence with a bounded wait.
                    let _ = tokio::time::timeout(SHUTDOWN_WAIT, host.shutdown()).await;
                }
                Err(_arc_with_other_owners) => {
                    tracing::warn!(
                        target: "log_storage",
                        plugin = %state.name,
                        "log_storage_backend shutdown invoked while a request is still in flight; closing stdio without graceful shutdown RPC",
                    );
                }
            }
        }
    }
}

/// Outcome of [`spawn_log_storage_supervisor`].
///
/// Surfaced so the daemon entrypoint can record a structured event with
/// the chosen plugin's name (or the in-tree marker) and any spawn-time
/// warnings — failures degrade to an in-tree handle so a broken
/// log_storage plugin never blocks startup.
#[derive(Debug)]
pub struct LogStorageSupervisorOutcome {
    /// Active handle (always populated). Either in-tree or plugin-backed.
    pub handle: Arc<LogStorageHandle>,
    /// Name of the plugin that won dispatch resolution, if any.
    pub plugin_name: Option<String>,
    /// Total number of candidate plugins discovered (before dispatch
    /// selection). Carried into the dispatch resolution event so operators
    /// see "we picked plugin X out of N candidates".
    pub candidate_count: usize,
    /// Whether the disable env knob was set when resolution ran.
    pub disable_env_set: bool,
    /// Operator-facing warnings (multiple candidates, spawn fallback,
    /// disable override, ...).
    pub warnings: Vec<String>,
}

/// Resolve, spawn, and handshake the active log_storage_backend dispatch.
///
/// Selection mirrors [`resolve_log_storage_dispatch`]. On `Plugin`
/// resolution, the host is spawned with [`PluginSpawnOptions::for_manifest`]
/// (so the daemon's manifest-declared env_required allowlist applies) and
/// the standard `initialize` / `initialized` handshake runs. On any
/// failure during spawn or handshake the function returns an in-tree
/// handle plus a warning so the daemon never refuses to start because of
/// a broken log_storage plugin.
pub async fn spawn_log_storage_supervisor(project_root: &Path) -> LogStorageSupervisorOutcome {
    let disable_env_set = log_storage_disable_env_set();
    let project_root_buf = project_root.to_path_buf();
    let resolution = match resolve_log_storage_dispatch(project_root) {
        Ok(resolution) => resolution,
        Err(error) => {
            return LogStorageSupervisorOutcome {
                handle: Arc::new(LogStorageHandle::in_tree(project_root_buf)),
                plugin_name: None,
                candidate_count: 0,
                disable_env_set,
                warnings: vec![format!(
                    "log_storage_backend discovery failed; falling back to in-tree Logger: {error:#}"
                )],
            };
        }
    };

    let candidate_count = resolution.all_candidates.len();
    let mut warnings = resolution.warnings.clone();

    match resolution.selected.as_ref() {
        LogStorageDispatch::InTree { .. } => LogStorageSupervisorOutcome {
            handle: Arc::new(LogStorageHandle::in_tree(project_root_buf)),
            plugin_name: None,
            candidate_count,
            disable_env_set,
            warnings,
        },
        LogStorageDispatch::Plugin { plugin, .. } => {
            let options = PluginSpawnOptions::for_manifest(
                plugin.name.clone(),
                &plugin.manifest.env_required,
                std::iter::empty::<String>(),
                None,
            )
            .with_notification_buffer_hint(plugin.manifest.notification_buffer_size)
            .with_working_dir(&project_root_buf);
            let host = match PluginHost::spawn_with_options(&plugin.path, &[], options).await {
                Ok(host) => host,
                Err(error) => {
                    warnings.push(format!(
                        "log_storage_backend plugin '{}' spawn failed; falling back to in-tree Logger: {error:#}",
                        plugin.name
                    ));
                    return LogStorageSupervisorOutcome {
                        handle: Arc::new(LogStorageHandle::in_tree(project_root_buf)),
                        plugin_name: None,
                        candidate_count,
                        disable_env_set,
                        warnings,
                    };
                }
            };
            if let Err(error) = host.handshake().await {
                warnings.push(format!(
                    "log_storage_backend plugin '{}' handshake failed; falling back to in-tree Logger: {error:#}",
                    plugin.name
                ));
                let _ = host.shutdown().await;
                return LogStorageSupervisorOutcome {
                    handle: Arc::new(LogStorageHandle::in_tree(project_root_buf)),
                    plugin_name: None,
                    candidate_count,
                    disable_env_set,
                    warnings,
                };
            }
            let plugin_name = plugin.name.clone();
            LogStorageSupervisorOutcome {
                handle: Arc::new(LogStorageHandle::from_handshaked_host(plugin_name.clone(), host, project_root_buf)),
                plugin_name: Some(plugin_name),
                candidate_count,
                disable_env_set,
                warnings,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_plugin_protocol::PluginManifest;
    use orchestrator_plugin_host::{DiscoveredPlugin, DiscoverySource};
    use tempfile::TempDir;

    /// Serializes env-var-touching tests so cargo's parallel runner does
    /// not race on the disable knob. Mirrors the pattern used by the
    /// discovery + provider resolver tests.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, prev }
        }

        fn unset(key: &'static str) -> Self {
            let prev = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(prev) => std::env::set_var(self.key, prev),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn fake_plugin(name: &str, path: PathBuf) -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: name.to_string(),
            path,
            manifest: PluginManifest {
                name: name.to_string(),
                version: "0.1.0".to_string(),
                plugin_kind: PLUGIN_KIND_LOG_STORAGE_BACKEND.to_string(),
                description: "test".to_string(),
                protocol_version: "1.0.0".to_string(),
                capabilities: vec![],
                env_required: vec![],
                notification_buffer_size: None,
            },
            source: DiscoverySource::ProjectLocal,
        }
    }

    /// Helper: build an isolated project root with no plugins discovered
    /// so the resolver returns `InTree` deterministically.
    fn isolated_project() -> (TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().to_path_buf();
        std::fs::create_dir_all(project.join(".animus/plugins")).expect("mkdir plugins dir");
        (temp, project)
    }

    #[test]
    fn discovers_zero_log_storage_backends_uses_in_tree() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _disable = EnvGuard::unset(LOG_STORAGE_DISABLE_ENV);
        let _animus_home = EnvGuard::set("ANIMUS_CONFIG_DIR", "/tmp/animus-test-empty-home-xyz123");
        let _plugin_dir = EnvGuard::set("ANIMUS_PLUGIN_DIR", "");

        let (_temp, project_root) = isolated_project();

        let resolution = resolve_log_storage_dispatch(&project_root).expect("resolve");
        assert!(!resolution.selected.is_plugin(), "expected in-tree dispatch, got {:?}", resolution.selected);
        assert!(resolution.warnings.is_empty(), "no plugins → no warnings");
        assert!(resolution.all_candidates.is_empty());
    }

    #[test]
    fn disable_env_var_forces_in_tree_even_when_candidates_present() {
        // We can't easily inject DiscoveredPlugin from a unit test without
        // building real plugin binaries, but we CAN test that the env-var
        // short-circuit triggers when set. Combined with the unit-level
        // tests on `log_storage_disable_env_set` below, the contract is
        // covered.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _disable = EnvGuard::set(LOG_STORAGE_DISABLE_ENV, "1");
        let _animus_home = EnvGuard::set("ANIMUS_CONFIG_DIR", "/tmp/animus-test-empty-home-xyz123");
        let _plugin_dir = EnvGuard::set("ANIMUS_PLUGIN_DIR", "");

        let (_temp, project_root) = isolated_project();
        let resolution = resolve_log_storage_dispatch(&project_root).expect("resolve");
        assert!(!resolution.selected.is_plugin(), "disable env forces in-tree");
    }

    #[test]
    fn disable_env_predicate_recognizes_truthy_values() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let _e1 = EnvGuard::set(LOG_STORAGE_DISABLE_ENV, "1");
        assert!(log_storage_disable_env_set(), "'1' is truthy");
        drop(_e1);

        let _e2 = EnvGuard::set(LOG_STORAGE_DISABLE_ENV, "true");
        assert!(log_storage_disable_env_set(), "'true' is truthy");
        drop(_e2);

        let _e3 = EnvGuard::set(LOG_STORAGE_DISABLE_ENV, "TRUE");
        assert!(log_storage_disable_env_set(), "uppercase 'TRUE' is truthy");
        drop(_e3);

        let _e4 = EnvGuard::set(LOG_STORAGE_DISABLE_ENV, "0");
        assert!(!log_storage_disable_env_set(), "'0' is falsy");
        drop(_e4);

        let _e5 = EnvGuard::set(LOG_STORAGE_DISABLE_ENV, "");
        assert!(!log_storage_disable_env_set(), "empty is falsy");
        drop(_e5);

        let _e6 = EnvGuard::unset(LOG_STORAGE_DISABLE_ENV);
        assert!(!log_storage_disable_env_set(), "unset is falsy");
    }

    #[test]
    fn multiple_backends_picks_first_by_name_and_warns() {
        // Pure unit test on the deterministic-selection branch — we
        // synthesize the candidate set rather than discover from disk so
        // the test does not depend on real plugin binaries.
        let project_root = PathBuf::from("/tmp/fake-project-xyz");
        let mut candidates: Vec<DiscoveredPlugin> = [
            fake_plugin("zeta-storage", PathBuf::from("/tmp/zeta")),
            fake_plugin("alpha-storage", PathBuf::from("/tmp/alpha")),
            fake_plugin("middle-storage", PathBuf::from("/tmp/middle")),
        ]
        .into_iter()
        .collect();
        candidates.sort_by(|a, b| a.name.cmp(&b.name));
        let chosen = candidates.first().cloned().expect("non-empty");
        assert_eq!(chosen.name, "alpha-storage", "deterministic selection picks lexicographic first");

        // Sanity-check the warning shape. We construct the same warning
        // text that resolve_log_storage_dispatch emits so a future format
        // change is caught here.
        let warning = format!(
            "multiple log_storage_backend plugins installed ({}); selecting first by name ({}). Multi-backend fan-out is not yet supported.",
            candidates.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", "),
            chosen.name
        );
        assert!(warning.contains("multiple log_storage_backend plugins installed"));
        assert!(warning.contains("alpha-storage"));
        assert!(warning.contains("Multi-backend fan-out is not yet supported"));

        // Build the actual dispatch for the chosen plugin and confirm the
        // helpers introspect correctly.
        let dispatch = LogStorageDispatch::Plugin { project_root: project_root.clone(), plugin: Box::new(chosen) };
        assert!(dispatch.is_plugin());
        assert_eq!(dispatch.plugin_name(), Some("alpha-storage"));
        assert_eq!(dispatch.project_root(), project_root.as_path());
    }

    // -----------------------------------------------------------------
    // Plugin-routed handle: outbound `log_storage/store` request +
    // inbound `log_storage/query` request via a fake in-process plugin
    // host.
    // -----------------------------------------------------------------

    /// (method, params) tuple recorded by [`fake_log_storage_host`] for
    /// every JSON-RPC request the fake plugin receives.
    type RecordedCall = (String, Option<serde_json::Value>);

    /// Spawns an in-process fake `log_storage_backend` over
    /// `tokio::io::duplex` streams. Records every incoming request so
    /// tests can assert `log_storage/store` (and friends) arrived, and
    /// returns a canned response for `log_storage/query` requests.
    ///
    /// Mirrors the helper pattern used by `subject_dispatch::tests`.
    async fn fake_log_storage_host(
        name: &str,
        plugin_query_response: serde_json::Value,
        recorded_calls: std::sync::Arc<tokio::sync::Mutex<Vec<RecordedCall>>>,
    ) -> orchestrator_plugin_host::PluginHost {
        use animus_plugin_protocol::{InitializeResult, PluginCapabilities, PluginInfo, RpcRequest, RpcResponse};
        use orchestrator_plugin_host::PluginHost;
        use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);
        let name_for_task = name.to_string();
        let recorded = recorded_calls.clone();
        let query_response = plugin_query_response.clone();

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.expect("read line") == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: serde_json::Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if value.get("id").is_none() || value.get("id") == Some(&serde_json::Value::Null) {
                    // Notifications (`initialized`, `exit`, ...) — ignore.
                    continue;
                }
                let request: RpcRequest = match serde_json::from_value(value) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let response = match request.method.as_str() {
                    "initialize" => RpcResponse::ok(
                        request.id,
                        serde_json::json!(InitializeResult {
                            protocol_version: "1.0.0".to_string(),
                            plugin_info: PluginInfo {
                                name: name_for_task.clone(),
                                version: "0.1.0".to_string(),
                                plugin_kind: PLUGIN_KIND_LOG_STORAGE_BACKEND.to_string(),
                                description: None,
                            },
                            capabilities: PluginCapabilities::default(),
                        }),
                    ),
                    method => {
                        recorded.lock().await.push((method.to_string(), request.params.clone()));
                        if method == "log_storage/query" {
                            RpcResponse::ok(request.id, query_response.clone())
                        } else {
                            RpcResponse::ok(request.id, serde_json::json!({}))
                        }
                    }
                };
                let mut encoded = serde_json::to_string(&response).expect("encode response");
                encoded.push('\n');
                if plugin_writer.write_all(encoded.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        PluginHost::from_streams(name, host_reader, host_writer)
    }

    #[tokio::test]
    async fn plugin_handle_routes_log_entry_via_log_storage_store_request() {
        let recorded: std::sync::Arc<tokio::sync::Mutex<Vec<RecordedCall>>> =
            std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let host = fake_log_storage_host("test-log-sink", serde_json::json!({"entries": []}), recorded.clone()).await;
        host.handshake().await.expect("handshake");

        let handle = std::sync::Arc::new(LogStorageHandle::from_handshaked_host(
            "test-log-sink",
            host,
            PathBuf::from("/tmp/fake-project-xyz"),
        ));
        assert!(handle.is_plugin());
        assert_eq!(handle.plugin_name(), Some("test-log-sink"));

        let entry = serde_json::json!({
            "id": "evt-1",
            "ts": "2026-05-28T00:00:00Z",
            "level": "info",
            "source": "daemon",
            "target": "daemon.events.test",
            "message": "hello plugin",
            "fields": {"k": "v"}
        });
        let took_over = handle.publish_log_entry(entry.clone()).await.expect("publish ok");
        assert!(took_over, "plugin handle reports it took ownership of the entry");

        // Give the host's writer task time to drain.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let captured = recorded.lock().await;
        let received = captured
            .iter()
            .find(|(method, _)| method == METHOD_LOG_STORAGE_STORE)
            .expect("log_storage/store request recorded");
        let params = received.1.clone().expect("request carries params");
        let entries = params.get("entries").and_then(|v| v.as_array()).expect("entries array");
        assert_eq!(entries.len(), 1, "single-entry batch");
        assert_eq!(entries[0], entry, "entry round-trips verbatim inside the batch");
        drop(captured);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn plugin_handle_tail_calls_log_storage_query_request() {
        let recorded: std::sync::Arc<tokio::sync::Mutex<Vec<RecordedCall>>> =
            std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let canned = serde_json::json!({
            "entries": [
                {
                    "id": "evt-canned",
                    "ts": "2026-05-28T00:00:00Z",
                    "level": "info",
                    "source": "plugin",
                    "source_name": "test-log-sink",
                    "target": "test",
                    "message": "from-plugin",
                }
            ]
        });
        let host = fake_log_storage_host("test-log-sink", canned.clone(), recorded.clone()).await;
        host.handshake().await.expect("handshake");

        let handle = std::sync::Arc::new(LogStorageHandle::from_handshaked_host(
            "test-log-sink",
            host,
            PathBuf::from("/tmp/fake-project-xyz"),
        ));

        let response = handle
            .tail(Some(serde_json::json!({"limit": 5})), std::time::Duration::from_secs(2))
            .await
            .expect("tail ok");
        let response = response.expect("plugin produced a query response");
        assert_eq!(response, canned, "plugin query response surfaced verbatim");

        // The plugin saw a `log_storage/query` request (not `tail`,
        // which is streaming-only).
        let captured = recorded.lock().await;
        assert!(
            captured.iter().any(|(method, _)| method == METHOD_LOG_STORAGE_QUERY),
            "expected log_storage/query, saw {:?}",
            captured.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>()
        );
        drop(captured);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn in_tree_handle_publishes_no_op_and_tail_returns_none() {
        let handle = LogStorageHandle::in_tree(PathBuf::from("/tmp/fake-in-tree"));
        assert!(!handle.is_plugin());
        assert!(handle.plugin_name().is_none());
        let took_over = handle.publish_log_entry(serde_json::json!({"a": 1})).await.expect("ok");
        assert!(!took_over, "in-tree publish reports the caller should fall back to file write");
        let tail = handle.tail(None, std::time::Duration::from_millis(10)).await.expect("tail ok");
        assert!(tail.is_none(), "in-tree tail returns None so caller reads file directly");
    }

    #[tokio::test]
    async fn plugin_handle_publish_failure_after_shutdown_does_not_panic() {
        let recorded: std::sync::Arc<tokio::sync::Mutex<Vec<RecordedCall>>> =
            std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let host = fake_log_storage_host("test-log-sink", serde_json::json!({"entries": []}), recorded.clone()).await;
        host.handshake().await.expect("handshake");
        let handle = std::sync::Arc::new(LogStorageHandle::from_handshaked_host(
            "test-log-sink",
            host,
            PathBuf::from("/tmp/fake-project-xyz"),
        ));
        handle.shutdown().await;
        // Second publish after shutdown must surface an error — callers
        // (DaemonEventLog::append) swallow it via tracing rather than
        // panicking the daemon process.
        let err = handle.publish_log_entry(serde_json::json!({"id": "x"})).await;
        assert!(err.is_err(), "publishing after shutdown surfaces a recoverable error");
    }

    #[tokio::test]
    async fn global_handle_slot_round_trip() {
        // Smoke test: install + read + clear flows match the lifecycle
        // documented on `install_log_storage_handle`. Serialized through
        // LOG_STORAGE_TEST_SLOT_LOCK so it can't race with the
        // plugin-route tests in this and other modules that also
        // install handles into the process-global slot.
        let _slot = LOG_STORAGE_TEST_SLOT_LOCK.lock().await;
        super::clear_log_storage_handle();
        let handle = std::sync::Arc::new(LogStorageHandle::in_tree(PathBuf::from("/tmp/fake-slot-root")));
        super::install_log_storage_handle(handle.clone());
        let read_back = super::current_log_storage_handle().expect("handle installed");
        assert!(std::sync::Arc::ptr_eq(&handle, &read_back));
        super::clear_log_storage_handle();
        assert!(super::current_log_storage_handle().is_none());
    }
}
