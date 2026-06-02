use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use animus_plugin_protocol::{
    error_codes, EnvRequirement, HealthCheckResult, HostCapabilities, HostInfo, InitializeParams, InitializeResult,
    RpcError, RpcNotification, RpcRequest, RpcResponse, PROTOCOL_VERSION,
};
use anyhow::{anyhow, Result};
use semver::Version;
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Child;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// Universal shell environment variables that every plugin gets regardless of
/// its declared `env_required` manifest. These are the locale + shell + Rust
/// telemetry vars that practically every CLI tool expects; withholding them
/// breaks even well-behaved plugins for no security gain (none of them carry
/// secrets).
///
/// Anything **not** in this list and **not** explicitly declared by the
/// plugin's manifest is scrubbed from the spawn environment via
/// [`std::process::Command::env_clear`].
pub const PLUGIN_BASE_ENV_ALLOWLIST: &[&str] =
    &["PATH", "HOME", "USER", "SHELL", "TERM", "TMPDIR", "LANG", "LC_ALL", "RUST_LOG", "RUST_BACKTRACE", "TZ"];

/// Compiled default for the per-host notification broadcast channel capacity.
///
/// Used when neither [`PluginManifest::notification_buffer_size`] nor the
/// `ANIMUS_PLUGIN_BROADCAST_CAPACITY` env override is set. Mirrors the
/// session-backend convention of ~256 in-flight notification slots per
/// subscriber.
///
/// [`PluginManifest::notification_buffer_size`]: animus_plugin_protocol::PluginManifest::notification_buffer_size
pub const DEFAULT_NOTIFICATION_BROADCAST_CAPACITY: usize = 256;

/// Environment variable operators set to override the per-plugin broadcast
/// channel capacity. Lower precedence than the plugin manifest hint, higher
/// precedence than [`DEFAULT_NOTIFICATION_BROADCAST_CAPACITY`].
pub const NOTIFICATION_BROADCAST_CAPACITY_ENV: &str = "ANIMUS_PLUGIN_BROADCAST_CAPACITY";

/// Deadline the [`PluginHost::shutdown`] flow waits for the child to exit
/// after sending the `shutdown` RPC.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Deadline the [`PluginHost::shutdown_transport`] flow waits for a transport
/// plugin's `transport/shutdown` reply before moving on to the generic
/// shutdown. Spec-compliant transports drain in-flight requests during this
/// call; a misbehaving plugin must not block daemon teardown so the upper
/// bound is enforced here.
const TRANSPORT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// JSON-RPC method name the host issues to ask a `transport_backend` plugin
/// to bind its external listener. Kept as a string constant so this crate
/// avoids a build-time dependency on `animus-transport-protocol`; the spec
/// freezes the literal at `transport/start` (see
/// `animus-transport-protocol::TRANSPORT_METHOD_START`).
pub const TRANSPORT_METHOD_START: &str = "transport/start";

/// JSON-RPC method name the host issues to ask a `transport_backend` plugin
/// to drain in-flight requests and release its bound address. Mirrors
/// `animus-transport-protocol::TRANSPORT_METHOD_SHUTDOWN`.
pub const TRANSPORT_METHOD_SHUTDOWN: &str = "transport/shutdown";

/// Structured plugin-host errors that benefit from being matched on by
/// callers. The supervisor pattern-matches on this enum to decide whether a
/// failure is death-like (retry-once safe) or a structured plugin-side error
/// (retry would just re-elicit). Constructing one of these at the point of
/// failure (vs coercing everything to `RpcError { code: INTERNAL_ERROR, ... }`
/// and parsing message substrings later) is the architectural fix shipped in
/// the typed-classifier refactor.
#[derive(Debug, Error)]
pub enum HostError {
    /// The plugin advertised a `protocol_version` that the host cannot speak.
    ///
    /// Major-version mismatch (or non-semver gibberish) trips this. The host
    /// should quarantine the plugin and surface the message so users can see
    /// which plugin is wedged.
    #[error("incompatible plugin protocol: {0}")]
    IncompatibleProtocol(String),
    /// The plugin transport closed (or never opened) while an awaiter was
    /// waiting for a response.
    ///
    /// Surfaced when the child process exits, its stdout closes, or the
    /// reader task observes a fatal I/O error. The host is no longer usable
    /// after this error; the supervisor should respawn.
    #[error("plugin connection lost")]
    ConnectionLost,
    /// A [`PluginHost::request_with_timeout`] call exceeded its deadline.
    ///
    /// The pending awaiter is removed from the router map so any late
    /// response from the plugin is silently discarded.
    #[error("plugin request timed out after {0:?}")]
    Timeout(Duration),
    /// The plugin child process exited mid-request with a non-zero (or
    /// known-fatal) status. Reserved for future use by callers that watch the
    /// child's wait status directly; the in-tree dispatch path currently
    /// observes process death indirectly via [`Self::ConnectionLost`] when
    /// stdout closes.
    #[error("plugin process exited: {0}")]
    ProcessExited(String),
    /// The plugin returned a structured JSON-RPC error frame in response to
    /// a request. The plugin process is still alive; retrying would just
    /// re-elicit the same error. The supervisor uses this distinction to
    /// avoid wasting a restart budget on plugin-author bugs.
    #[error("plugin returned RPC error {}: {}", .0.code, .0.message)]
    Rpc(RpcError),
    /// The plugin did not advertise the capability the host is trying to
    /// invoke. Returned by higher-level callers (e.g. the session backend's
    /// cancel routing) when the plugin's handshake-reported
    /// [`PluginCapabilities`](animus_plugin_protocol::PluginCapabilities) does
    /// not include the required feature.
    ///
    /// Carries the capability name so callers can surface a useful message
    /// (e.g. "plugin 'foo' does not advertise capability 'cancellation'").
    #[error("plugin does not advertise capability: {0}")]
    CapabilityNotSupported(String),
}

impl From<HostError> for RpcError {
    fn from(err: HostError) -> Self {
        match err {
            HostError::Rpc(inner) => inner,
            HostError::Timeout(duration) => {
                RpcError { code: error_codes::TIMEOUT, message: HostError::Timeout(duration).to_string(), data: None }
            }
            other => RpcError { code: error_codes::INTERNAL_ERROR, message: other.to_string(), data: None },
        }
    }
}

/// Validate that a plugin's advertised `protocol_version` is wire-compatible
/// with the host's [`PROTOCOL_VERSION`].
///
/// Compatibility is gated by the semver major component. Plugins reporting a
/// matching major are accepted (minor/patch drift is treated as additive and
/// backwards-compatible). Plugins reporting a different major — or a
/// non-semver string — are rejected with [`HostError::IncompatibleProtocol`].
pub fn check_protocol_compat(plugin_version: &str) -> Result<(), HostError> {
    let host: Version = PROTOCOL_VERSION
        .parse()
        .map_err(|err| HostError::IncompatibleProtocol(format!("host protocol version is not valid semver: {err}")))?;
    let plugin: Version = plugin_version.parse().map_err(|_| {
        HostError::IncompatibleProtocol(format!(
            "plugin advertised non-semver protocol_version '{plugin_version}' (host speaks {PROTOCOL_VERSION})"
        ))
    })?;
    if plugin.major != host.major {
        return Err(HostError::IncompatibleProtocol(format!(
            "plugin protocol_version {plugin_version} incompatible with host {PROTOCOL_VERSION} (major version mismatch)"
        )));
    }
    Ok(())
}

/// Sink for plugin stderr lines. Receives `(plugin_name, line)` on each stderr line.
pub type PluginStderrSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Caller-supplied options that drive how the plugin host spawns a plugin
/// process.
///
/// Use [`PluginSpawnOptions::for_manifest`] to derive an environment allowlist
/// from a plugin's [`PluginManifest::env_required`](animus_plugin_protocol::PluginManifest::env_required)
/// list. See [`PLUGIN_BASE_ENV_ALLOWLIST`] for the universally-forwarded vars.
#[derive(Default, Clone)]
pub struct PluginSpawnOptions {
    /// Routes every stderr line through this sink in addition to the standard
    /// `tracing::warn!` log. Useful for surfacing plugin diagnostics into a
    /// project's structured events log.
    pub stderr_sink: Option<PluginStderrSink>,
    /// Names of environment variables the plugin is allowed to see. The host
    /// always forwards [`PLUGIN_BASE_ENV_ALLOWLIST`] on top of this list.
    /// Anything else is scrubbed.
    pub env_allowlist: Vec<String>,
    /// Plugin-name label used in any spawn-time warnings (e.g. missing
    /// required env). When empty, the host falls back to the binary file name.
    pub plugin_label: Option<String>,
    /// Required-but-missing env variable names. The host emits a `warn!` for
    /// each at spawn time so operators can see why the plugin will likely
    /// fail.
    pub missing_required_env: Vec<String>,
    /// Optional override for the broadcast channel capacity used for plugin
    /// notifications. When `Some`, it wins over both the manifest hint and
    /// the env override. Used by tests; production callers typically leave
    /// this `None` and rely on
    /// [`PluginManifest::notification_buffer_size`] +
    /// [`NOTIFICATION_BROADCAST_CAPACITY_ENV`].
    ///
    /// [`PluginManifest::notification_buffer_size`]: animus_plugin_protocol::PluginManifest::notification_buffer_size
    pub notification_capacity: Option<usize>,
    /// Optional working directory for the spawned plugin process. When set,
    /// the host pins the child's cwd here instead of inheriting the
    /// caller's cwd. Subject-backend and provider plugins use cwd-relative
    /// paths for their on-disk state (e.g. `.animus/subjects/tasks.db`), so
    /// the daemon must pin cwd to `--project-root` rather than letting it
    /// depend on which shell happened to start the daemon. Leave `None`
    /// when the plugin has no cwd-relative state — the spawn then inherits
    /// the parent's cwd, matching pre-fix behavior.
    pub working_dir: Option<PathBuf>,
}

impl PluginSpawnOptions {
    /// Build options for a plugin whose manifest declares the supplied env
    /// requirements. Returns the assembled options and a list of declared-as
    /// `required = true` vars that are not currently set in the host process.
    ///
    /// The returned options force the spawn to scrub the daemon's environment
    /// to [`PLUGIN_BASE_ENV_ALLOWLIST`] plus the manifest's declared variables
    /// plus any explicit `extra` names supplied by the caller (e.g. one-off
    /// runtime overrides).
    pub fn for_manifest(
        plugin_label: impl Into<String>,
        env_required: &[EnvRequirement],
        extra_env_vars: impl IntoIterator<Item = String>,
        stderr_sink: Option<PluginStderrSink>,
    ) -> Self {
        let plugin_label = plugin_label.into();
        let mut allow: BTreeSet<String> = env_required.iter().map(|requirement| requirement.name.clone()).collect();
        allow.extend(extra_env_vars);
        let missing_required: Vec<String> = env_required
            .iter()
            .filter(|requirement| requirement.required)
            .filter(|requirement| std::env::var_os(&requirement.name).is_none())
            .map(|requirement| requirement.name.clone())
            .collect();
        Self {
            stderr_sink,
            env_allowlist: allow.into_iter().collect(),
            plugin_label: if plugin_label.is_empty() { None } else { Some(plugin_label) },
            missing_required_env: missing_required,
            notification_capacity: None,
            working_dir: None,
        }
    }

    /// Pin the spawned plugin's working directory. Used for subject-backend
    /// and provider plugins so their cwd-relative state paths
    /// (e.g. `.animus/subjects/tasks.db`) resolve under the project root
    /// rather than under whatever cwd the daemon happened to be started
    /// from.
    #[must_use]
    pub fn with_working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }
}

/// Receiver for plugin-emitted JSON-RPC notifications (frames without `id`).
///
/// Returned by [`PluginHost::subscribe_notifications`]. Each subscriber gets
/// an independent receiver fed by the host's single-reader router task; a
/// slow subscriber observes [`broadcast::error::RecvError::Lagged`] rather
/// than backpressuring the request path.
pub type PluginNotificationRx = broadcast::Receiver<RpcNotification>;

/// Choose the notification broadcast capacity for a plugin host using the
/// documented priority: explicit option override → plugin manifest hint →
/// env override → compiled default. Always returns a non-zero capacity (a
/// `broadcast::channel` with capacity 0 panics).
pub(crate) fn resolve_broadcast_capacity(spawn_override: Option<usize>, manifest_hint: Option<usize>) -> usize {
    if let Some(cap) = spawn_override {
        if cap > 0 {
            return cap;
        }
    }
    if let Some(cap) = manifest_hint {
        if cap > 0 {
            return cap;
        }
    }
    if let Ok(raw) = std::env::var(NOTIFICATION_BROADCAST_CAPACITY_ENV) {
        if let Ok(cap) = raw.trim().parse::<usize>() {
            if cap > 0 {
                return cap;
            }
        }
    }
    DEFAULT_NOTIFICATION_BROADCAST_CAPACITY
}

/// Opaque RAII guard returned by [`ProcessSlotFactory::acquire`]. Dropping it
/// must release the underlying quota slot. The plugin host holds one of these
/// alongside the spawned child for the child's lifetime, so a slot is held for
/// exactly the same duration as the live plugin process.
///
/// The marker trait is intentionally empty: the only behaviour the host cares
/// about is `Drop`. Implementors typically wrap a concrete RAII type owned by
/// the quota module (e.g. `orchestrator_daemon_runtime::PluginProcessSlot`).
pub trait ProcessSlotGuard: Send + Sync + std::fmt::Debug {}

/// Boxed trait object alias used everywhere the host stores or returns a slot.
pub type BoxedProcessSlotGuard = Box<dyn ProcessSlotGuard>;

/// Structured error returned by [`ProcessSlotFactory::acquire`] when the
/// configured per-process plugin cap is at its limit. The host translates this
/// into an `anyhow::Error` at the spawn site so callers see a single error
/// type from `spawn_with_options`.
#[derive(Debug, Clone)]
pub struct ProcessSlotError {
    /// Currently-live plugin process count as observed by the factory.
    pub current: usize,
    /// Configured cap (e.g. `RuntimeQuotas::plugin_process_max`).
    pub cap: usize,
    /// Human-readable diagnostic appended by the factory (often the factory's
    /// own `Display` formatting of its native error type).
    pub message: String,
}

impl std::fmt::Display for ProcessSlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProcessSlotError {}

/// Quota-enforcement boundary the plugin host uses at the spawn site.
///
/// The plugin-host crate intentionally does NOT depend on
/// `orchestrator-daemon-runtime` (that crate already depends on this one;
/// adding a reverse dep would form a cycle). Instead the daemon installs an
/// implementation of this trait at startup via [`install_process_slot_factory`].
/// When no factory is installed, the host falls back to a no-op slot and
/// behaviour is identical to pre-quota releases (used by unit tests and any
/// embedder that hasn't opted in).
pub trait ProcessSlotFactory: Send + Sync + 'static {
    /// Try to claim a slot. Returns `Err` if the cap is reached; the host
    /// surfaces this as a spawn failure rather than queuing or blocking.
    fn acquire(&self) -> Result<BoxedProcessSlotGuard, ProcessSlotError>;
}

/// Lazy-init container for the process-wide factory. Production daemon
/// startup installs exactly once; tests may swap via
/// [`install_process_slot_factory_for_test`] under a serializing mutex.
fn process_slot_factory_slot() -> &'static RwLock<Option<Arc<dyn ProcessSlotFactory>>> {
    static SLOT: OnceLock<RwLock<Option<Arc<dyn ProcessSlotFactory>>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install the process-wide [`ProcessSlotFactory`]. First-installer-wins:
/// subsequent calls return `false` and leave the existing factory in place so
/// a test that pre-installed a stub keeps its override even if the daemon
/// startup path also runs.
pub fn install_process_slot_factory(factory: Arc<dyn ProcessSlotFactory>) -> bool {
    let mut guard = process_slot_factory_slot().write().expect("process slot factory lock poisoned");
    if guard.is_some() {
        return false;
    }
    *guard = Some(factory);
    true
}

/// Test-only: unconditionally replace the installed factory. Production code
/// must never call this; the daemon startup path uses
/// [`install_process_slot_factory`] which is first-installer-wins.
#[cfg(any(test, feature = "test-support"))]
pub fn install_process_slot_factory_for_test(factory: Arc<dyn ProcessSlotFactory>) {
    let mut guard = process_slot_factory_slot().write().expect("process slot factory lock poisoned");
    *guard = Some(factory);
}

/// Test-only: clear the installed factory so the spawn path falls back to
/// the no-quota path.
#[cfg(any(test, feature = "test-support"))]
pub fn clear_process_slot_factory_for_test() {
    let mut guard = process_slot_factory_slot().write().expect("process slot factory lock poisoned");
    *guard = None;
}

/// Snapshot of the currently-installed factory. Cloned `Arc` so the caller
/// doesn't hold the lock across an `.acquire()` call.
fn current_process_slot_factory() -> Option<Arc<dyn ProcessSlotFactory>> {
    process_slot_factory_slot().read().expect("process slot factory lock poisoned").clone()
}

/// Shared inner state for a [`PluginHost`]. One per spawned plugin process.
///
/// The host follows the single-reader-router pattern: one tokio task owns
/// the transport's read half and demultiplexes inbound frames. Frames with
/// an `id` field route to the pending-map awaiter via a oneshot channel;
/// frames without an `id` fan out via [`broadcast`] to every subscriber.
/// Writes go through `transport_write` so concurrent `request()` calls
/// serialize cleanly on the line-delimited wire.
pub struct PluginHostInner {
    /// Human-readable plugin name, used in log messages and shutdown.
    pub name: String,
    /// Locked write half of the stdio transport. Concurrent senders
    /// interleave one frame at a time.
    transport_write: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    /// Pending request awaiters keyed by JSON-RPC id. Populated by
    /// `request()` / `request_with_timeout()`, drained by the reader task
    /// (or by the host itself on shutdown).
    pending: Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>,
    /// Sender owned by the reader task; subscribers come and go via
    /// [`PluginHost::subscribe_notifications`].
    notifications_tx: broadcast::Sender<RpcNotification>,
    /// Monotonic JSON-RPC id allocator. We allocate from `1` so a freshly
    /// constructed host doesn't collide with the spec's "null id" sentinel.
    next_id: AtomicU64,
    /// The plugin child process. Owned so [`PluginHost::shutdown`] can kill
    /// it if `shutdown` RPC times out. `None` for hosts constructed from
    /// in-memory pipes (tests).
    child: Mutex<Option<Child>>,
    /// Reader task handle. `Some` until [`PluginHost::shutdown`] reaps it.
    /// Held under a sync mutex so [`PluginHost::launch`] can stash it
    /// immediately (no awaits) before returning the host to callers.
    reader_handle: std::sync::Mutex<Option<JoinHandle<()>>>,
    /// Flips to `false` when the reader task exits (EOF, fatal error, or
    /// shutdown). New requests issued after this point short-circuit with
    /// [`HostError::ConnectionLost`] instead of inserting an awaiter that
    /// would never be answered.
    alive: AtomicBool,
    /// Process-quota RAII guard acquired at spawn time. Held for the lifetime
    /// of the host (and therefore the child); dropped when the `Arc<...Inner>`
    /// goes away, which is after [`PluginHost::shutdown`] has reaped the
    /// child. `None` for tests / embedders that haven't installed a
    /// [`ProcessSlotFactory`].
    ///
    /// Held inside a mutex purely so [`PluginHost::shutdown`] can take the
    /// guard and drop it eagerly after the child wait completes, ahead of the
    /// last `Arc` drop. In steady state nothing else touches this field.
    _process_slot: std::sync::Mutex<Option<BoxedProcessSlotGuard>>,
}

/// Single-process JSON-RPC plugin host.
///
/// Cloning a [`PluginHost`] hands out another shared reference to the same
/// underlying transport — all methods take `&self` and may be called
/// concurrently. The router task is single-reader; writes are serialized
/// through an internal mutex so frames stay intact on the wire.
///
/// Construct one via [`PluginHost::spawn_with_options`] for a real child
/// process or [`PluginHost::from_streams`] for in-memory tests.
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<PluginHostInner>,
}

impl PluginHost {
    /// Spawn a plugin without forwarding any environment beyond
    /// [`PLUGIN_BASE_ENV_ALLOWLIST`]. Most production callers should use
    /// [`PluginHost::spawn_with_options`] instead so the plugin sees the
    /// env it declared in its manifest.
    pub async fn spawn(binary_path: &Path, args: &[&str]) -> Result<Self> {
        Self::spawn_with_options(binary_path, args, PluginSpawnOptions::default()).await
    }

    /// Spawn a plugin and route every stderr line through the supplied sink in addition
    /// to the standard `tracing::warn!` log. Use this from the host runtime so plugin
    /// diagnostics land in the project's structured `events.jsonl`.
    ///
    /// Note: this convenience does not forward any plugin-specific env vars.
    /// Prefer [`PluginHost::spawn_with_options`] (with options built via
    /// [`PluginSpawnOptions::for_manifest`]) for production spawns so the
    /// plugin's manifest-declared environment is honored.
    pub async fn spawn_with_stderr(
        binary_path: &Path,
        args: &[&str],
        stderr_sink: Option<PluginStderrSink>,
    ) -> Result<Self> {
        let options = PluginSpawnOptions { stderr_sink, ..PluginSpawnOptions::default() };
        Self::spawn_with_options(binary_path, args, options).await
    }

    /// Spawn a plugin under the supplied [`PluginSpawnOptions`].
    ///
    /// The host always calls `env_clear()` on the child process and forwards
    /// only the union of [`PLUGIN_BASE_ENV_ALLOWLIST`] and
    /// `options.env_allowlist`. This is the v0.4.x trust boundary: plugins
    /// only see secrets they explicitly declared in their manifest.
    pub async fn spawn_with_options(binary_path: &Path, args: &[&str], options: PluginSpawnOptions) -> Result<Self> {
        let binary_name = binary_path.file_name().and_then(|value| value.to_str()).unwrap_or("plugin").to_string();
        let name = options.plugin_label.clone().unwrap_or_else(|| binary_name.clone());

        // Quota check BEFORE the fork: if the daemon has installed a
        // ProcessSlotFactory and the per-process cap is reached, refuse
        // the spawn instead of letting the fd/memory pressure build. The
        // slot is held alongside the child for the rest of its lifetime;
        // dropping it (in shutdown or when the Arc<...Inner> goes away)
        // releases capacity for the next spawn.
        let process_slot = match current_process_slot_factory() {
            Some(factory) => Some(factory.acquire().map_err(|err| {
                warn!(plugin = %name, error = %err, "refused plugin spawn: process slot cap reached");
                anyhow!("{err}")
            })?),
            None => None,
        };

        let mut command = tokio::process::Command::new(binary_path);
        command
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(working_dir) = options.working_dir.as_ref() {
            command.current_dir(working_dir);
        }

        // Build the allowlist: universal base + caller-declared. Deduplicate
        // case-sensitively (env var names are case-sensitive on POSIX).
        let mut allow: BTreeSet<&str> = PLUGIN_BASE_ENV_ALLOWLIST.iter().copied().collect();
        for var in &options.env_allowlist {
            allow.insert(var.as_str());
        }

        command.env_clear();
        for var in &allow {
            if let Some(value) = std::env::var_os(var) {
                command.env(var, value);
            }
        }

        for missing in &options.missing_required_env {
            warn!(
                plugin = %name,
                env_var = %missing,
                "plugin declared env_required={{name={missing}, required=true}} but the host environment does not have it set; the plugin will likely fail to start"
            );
        }

        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("failed to take plugin stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("failed to take plugin stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("failed to take plugin stderr"))?;

        let stderr_plugin_name = name.clone();
        let stderr_sink = options.stderr_sink.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                warn!(plugin = %stderr_plugin_name, "{}", line);
                if let Some(sink) = stderr_sink.as_ref() {
                    sink(&stderr_plugin_name, &line);
                }
            }
        });

        let capacity = resolve_broadcast_capacity(options.notification_capacity, None);
        Ok(Self::launch_with_slot(name, Box::new(stdout), Box::new(stdin), Some(child), capacity, process_slot))
    }

    /// Build a host from caller-supplied in-memory streams. Used by tests
    /// that script a plugin in-process without spawning a real binary.
    ///
    /// The reader and writer are boxed and erased; the resulting
    /// [`PluginHost`] is identical in behavior to a spawned-process host.
    pub fn from_streams<R, W>(name: impl Into<String>, reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::launch(name.into(), Box::new(reader), Box::new(writer), None, DEFAULT_NOTIFICATION_BROADCAST_CAPACITY)
    }

    /// Build a host from in-memory streams with an explicit broadcast
    /// capacity override. Convenience for tests that need to exercise the
    /// `Lagged` path.
    pub fn from_streams_with_capacity<R, W>(name: impl Into<String>, reader: R, writer: W, capacity: usize) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let capacity = capacity.max(1);
        Self::launch(name.into(), Box::new(reader), Box::new(writer), None, capacity)
    }

    /// Internal hot-path constructor: wires up the pending-map, broadcast
    /// channel, and reader task in one place so both `spawn_with_options`
    /// and `from_streams` produce the same shape of host.
    fn launch(
        name: String,
        reader: Box<dyn AsyncRead + Send + Unpin>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
        child: Option<Child>,
        notification_capacity: usize,
    ) -> Self {
        Self::launch_with_slot(name, reader, writer, child, notification_capacity, None)
    }

    /// Variant of [`Self::launch`] that also stashes the process-quota slot
    /// alongside the child. Only `spawn_with_options` calls this with a
    /// `Some` slot; in-memory stream constructors pass `None`.
    fn launch_with_slot(
        name: String,
        reader: Box<dyn AsyncRead + Send + Unpin>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
        child: Option<Child>,
        notification_capacity: usize,
        process_slot: Option<BoxedProcessSlotGuard>,
    ) -> Self {
        let (notifications_tx, _) = broadcast::channel::<RpcNotification>(notification_capacity);
        let inner = Arc::new(PluginHostInner {
            name,
            transport_write: Mutex::new(writer),
            pending: Mutex::new(HashMap::new()),
            notifications_tx: notifications_tx.clone(),
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
            reader_handle: std::sync::Mutex::new(None),
            alive: AtomicBool::new(true),
            _process_slot: std::sync::Mutex::new(process_slot),
        });

        let reader_inner = inner.clone();
        let handle = tokio::spawn(reader_loop(reader, reader_inner, notifications_tx));
        // Stash the handle synchronously so shutdown() can find it without
        // racing the spawn that owns the reader loop.
        *inner.reader_handle.lock().expect("reader_handle mutex poisoned at launch") = Some(handle);

        Self { inner }
    }

    /// Plugin name (label) — same as the `name` field passed to spawn.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Subscribe to JSON-RPC notifications (frames with no `id`) emitted by
    /// the plugin. Each call returns an independent receiver fed by the
    /// shared broadcast channel; subscribers are responsible for keeping up
    /// (and observing `Lagged` if they don't).
    pub fn subscribe_notifications(&self) -> PluginNotificationRx {
        self.inner.notifications_tx.subscribe()
    }

    /// The next request id this host will allocate. Useful for tests; not
    /// part of the steady-state API.
    pub fn next_request_id(&self) -> u64 {
        self.inner.next_id.load(Ordering::Relaxed)
    }

    /// Send a JSON-RPC request and await its response.
    ///
    /// Multiple concurrent calls share the transport but each gets its own
    /// pending-map entry; they multiplex independently.
    ///
    /// This is the legacy-shape API (`Result<Value, RpcError>`) preserved for
    /// callers that don't care about the structural distinction between
    /// process-death and a plugin-side error. New callers should prefer
    /// [`PluginHost::request_typed`], which returns the typed [`HostError`]
    /// enum so the supervisor can pattern-match instead of parsing message
    /// substrings.
    pub async fn request(&self, method: impl Into<String>, params: Option<Value>) -> Result<Value, RpcError> {
        self.request_typed(method, params).await.map_err(RpcError::from)
    }

    /// Typed variant of [`PluginHost::request`]: surfaces process-death
    /// (`HostError::ConnectionLost`) and plugin-side RPC errors
    /// (`HostError::Rpc(_)`) as distinct enum variants. The dispatcher
    /// classifier in `orchestrator-session-host` matches on this enum to
    /// decide whether a retry-once is safe.
    pub async fn request_typed(&self, method: impl Into<String>, params: Option<Value>) -> Result<Value, HostError> {
        let method = method.into();
        let response = self.request_raw(&method, params).await?;
        match response.error {
            Some(error) => Err(HostError::Rpc(error)),
            None => Ok(response.result.unwrap_or(Value::Null)),
        }
    }

    /// Same as [`PluginHost::request`] but bails with [`HostError::Timeout`]
    /// if the plugin doesn't respond within `timeout`. The pending awaiter
    /// is removed from the router map so any late response is silently
    /// discarded.
    pub async fn request_with_timeout(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        self.request_typed_with_timeout(method, params, timeout).await.map_err(RpcError::from)
    }

    /// Typed variant of [`PluginHost::request_with_timeout`]. See
    /// [`PluginHost::request_typed`] for the distinction between
    /// process-death and plugin-side errors.
    pub async fn request_typed_with_timeout(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, HostError> {
        let method = method.into();
        let response = self.request_raw_with_timeout(&method, params, timeout).await?;
        match response.error {
            Some(error) => Err(HostError::Rpc(error)),
            None => Ok(response.result.unwrap_or(Value::Null)),
        }
    }

    /// Run the standard host→plugin `initialize`/`initialized` handshake.
    ///
    /// Returns the plugin's [`InitializeResult`] on success and rejects on
    /// protocol-version drift via [`check_protocol_compat`].
    pub async fn handshake(&self) -> Result<InitializeResult> {
        const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

        let params = InitializeParams {
            protocol_version: PROTOCOL_VERSION.to_string(),
            host_info: HostInfo { name: "animus".to_string(), version: env!("CARGO_PKG_VERSION").to_string() },
            capabilities: HostCapabilities { streaming: true, progress: true, cancellation: true },
        };

        let response = self
            .request_raw_with_timeout("initialize", Some(serde_json::to_value(params)?), HANDSHAKE_TIMEOUT)
            .await
            .map_err(|error| anyhow!("plugin '{}' initialize failed: {error}", self.inner.name))?;

        if let Some(error) = response.error {
            return Err(anyhow!("plugin initialize failed ({}): {}", error.code, error.message));
        }

        let result: InitializeResult =
            serde_json::from_value(response.result.ok_or_else(|| anyhow!("plugin initialize returned no result"))?)?;

        if let Err(host_error) = check_protocol_compat(&result.protocol_version) {
            return Err(anyhow!("plugin '{}' rejected at handshake: {host_error}", self.inner.name));
        }

        self.notify("initialized", None).await?;
        debug!(plugin = %self.inner.name, plugin_name = %result.plugin_info.name, "stdio plugin initialized");
        Ok(result)
    }

    /// Fire-and-forget JSON-RPC notification (no id, no response expected).
    pub async fn notify(&self, method: impl Into<String>, params: Option<Value>) -> Result<()> {
        self.write_frame(&RpcNotification::new(method, params)).await
    }

    /// Liveness probe — sends `$/ping` and waits 2 seconds for a response.
    pub async fn ping(&self) -> Result<()> {
        let response = self
            .request_raw_with_timeout("$/ping", None, Duration::from_secs(2))
            .await
            .map_err(|error| anyhow!("plugin ping failed: {error}"))?;
        if let Some(error) = response.error {
            return Err(anyhow!("plugin ping failed ({}): {}", error.code, error.message));
        }
        Ok(())
    }

    /// Structured health probe — sends `health/check` and decodes the
    /// response as a [`HealthCheckResult`].
    pub async fn health_check(&self) -> Result<HealthCheckResult> {
        let result = self
            .request_with_timeout("health/check", None, Duration::from_secs(2))
            .await
            .map_err(|error| anyhow!("plugin health/check failed ({}): {}", error.code, error.message))?;
        Ok(serde_json::from_value(result)?)
    }

    /// Transport-lifecycle drain: sends the spec-mandated
    /// `transport/shutdown` RPC so a `transport_backend` plugin can stop
    /// accepting new connections and drain in-flight requests before the
    /// host issues the generic `shutdown` RPC + `exit` notification.
    ///
    /// Behaviour:
    ///
    /// - Waits at most [`TRANSPORT_SHUTDOWN_GRACE`] for the plugin to reply
    ///   so a misbehaving plugin can never block daemon teardown.
    /// - Treats `METHOD_NOT_FOUND` (-32601) and `METHOD_NOT_SUPPORTED`
    ///   (-32001) as a no-op (logged as a deprecation warning) — these are
    ///   the responses returned by transport plugins that pre-date the
    ///   `transport/start`/`transport/shutdown` lifecycle and bind/unbind
    ///   during `initialize`/`shutdown` instead. The host MUST NOT fail
    ///   serve on this, since the legacy launchapp-dev transports relied
    ///   on the non-compliant happenstance for the entire v0.4.x cycle.
    /// - Treats `ConnectionLost` as a no-op — the plugin is already dead
    ///   and the subsequent `shutdown()` call will reap it.
    /// - All other errors are returned to the caller so unusual failures
    ///   surface in CLI output (and `serve` can decide whether to bail).
    ///
    /// Callers should always invoke this BEFORE [`Self::shutdown`] on
    /// `transport_backend` plugins. For other plugin kinds, calling this is
    /// a no-op (the plugin returns `METHOD_NOT_FOUND` and the host logs +
    /// continues), so passing every shutdown through this helper is safe.
    pub async fn shutdown_transport(&self) -> Result<()> {
        let outcome = self.request_typed_with_timeout(TRANSPORT_METHOD_SHUTDOWN, None, TRANSPORT_SHUTDOWN_GRACE).await;
        match outcome {
            Ok(_) => {
                debug!(plugin = %self.inner.name, "transport plugin acknowledged transport/shutdown");
                Ok(())
            }
            Err(HostError::Rpc(error)) if is_method_unimplemented(&error) => {
                warn!(
                    plugin = %self.inner.name,
                    code = error.code,
                    message = %error.message,
                    "transport plugin does not implement transport/shutdown — legacy lifecycle, continuing"
                );
                Ok(())
            }
            Err(HostError::ConnectionLost) => {
                debug!(plugin = %self.inner.name, "transport plugin already dead before transport/shutdown");
                Ok(())
            }
            Err(HostError::Timeout(deadline)) => {
                warn!(
                    plugin = %self.inner.name,
                    timeout_ms = u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
                    "transport/shutdown timed out; proceeding with generic shutdown"
                );
                Ok(())
            }
            Err(other) => Err(anyhow!("transport/shutdown failed on plugin '{}': {other}", self.inner.name)),
        }
    }

    /// Graceful shutdown: sends `shutdown` RPC + `exit` notification, waits
    /// up to [`SHUTDOWN_GRACE`] for the child to exit, then kills it.
    ///
    /// After this returns, every clone of the host observes
    /// [`HostError::ConnectionLost`] (surfaced as `RpcError`) on any
    /// subsequent `request()`.
    pub async fn shutdown(self) -> Result<()> {
        let inner = self.inner;
        // Best-effort shutdown RPC under a tight deadline. We don't trust
        // the plugin to actually respond, so we move on regardless.
        let _ = tokio::time::timeout(SHUTDOWN_GRACE, request_raw_inner(inner.as_ref(), "shutdown", None)).await;
        let _ = write_frame_inner(inner.as_ref(), &RpcNotification::new("exit", None)).await;

        // Mark the host as no longer accepting new work. Future request()
        // calls on cloned hosts short-circuit to ConnectionLost via the
        // alive flag.
        inner.alive.store(false, Ordering::Release);

        // Killing the child closes stdout, which causes the reader task to
        // see EOF and drain the pending map. We wait briefly for that
        // graceful drain before forcing the issue.
        let mut child_guard = inner.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            if tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await.is_err() {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
        drop(child_guard);

        // Reader task: should exit on its own after the child stdout closes.
        // For in-memory pipes (tests) the reader keeps running until the
        // fake plugin closes its writer half. Wait briefly so we don't
        // leak the join handle.
        let handle = inner.reader_handle.lock().expect("reader_handle mutex poisoned").take();
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, handle).await;
        }

        // Final safety net: drain every awaiter still parked on the
        // pending map. If the child died before this and the reader task
        // already drained, this is a no-op.
        drain_pending(inner.as_ref()).await;

        // Release the process-quota slot eagerly now that the child has
        // been reaped. Dropping the Arc<...Inner> would also drop the
        // slot, but other clones of the host may still hold a reference;
        // releasing here lets a follow-up spawn proceed without waiting
        // for those to drop.
        let _ = inner._process_slot.lock().expect("process slot mutex poisoned").take();

        Ok(())
    }

    async fn request_raw(&self, method: &str, params: Option<Value>) -> Result<RpcResponse, HostError> {
        request_raw_inner(self.inner.as_ref(), method, params).await
    }

    async fn request_raw_with_timeout(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<RpcResponse, HostError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);

        if let Err(_error) = self.write_frame(&RpcRequest::new(id, method, params)).await {
            self.inner.pending.lock().await.remove(&id);
            return Err(HostError::ConnectionLost);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => {
                self.inner.pending.lock().await.remove(&id);
                Err(HostError::ConnectionLost)
            }
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(HostError::Timeout(timeout))
            }
        }
    }

    async fn write_frame<T: serde::Serialize>(&self, frame: &T) -> Result<()> {
        write_frame_inner(self.inner.as_ref(), frame).await
    }
}

/// True when an [`RpcError`] indicates the plugin recognized the method name
/// but does not (yet) implement it. Both classic JSON-RPC `METHOD_NOT_FOUND`
/// (-32601) and the protocol's domain-specific `METHOD_NOT_SUPPORTED`
/// (-32001) qualify. Used by [`PluginHost::shutdown_transport`] to keep
/// pre-lifecycle transport plugins working while the ecosystem catches up.
fn is_method_unimplemented(error: &RpcError) -> bool {
    error.code == error_codes::METHOD_NOT_FOUND || error.code == error_codes::METHOD_NOT_SUPPORTED
}

/// Module-level helper so [`PluginHost::shutdown`] (which consumes `self`)
/// and the [`PluginHost`] inherent methods can share the same request path
/// without fighting the borrow checker.
async fn request_raw_inner(
    inner: &PluginHostInner,
    method: &str,
    params: Option<Value>,
) -> Result<RpcResponse, HostError> {
    if !inner.alive.load(Ordering::Acquire) {
        return Err(HostError::ConnectionLost);
    }
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    inner.pending.lock().await.insert(id, tx);

    if let Err(_error) = write_frame_inner(inner, &RpcRequest::new(id, method, params)).await {
        inner.pending.lock().await.remove(&id);
        return Err(HostError::ConnectionLost);
    }

    match rx.await {
        Ok(response) => Ok(response),
        Err(_) => {
            inner.pending.lock().await.remove(&id);
            Err(HostError::ConnectionLost)
        }
    }
}

async fn write_frame_inner<T: serde::Serialize>(inner: &PluginHostInner, frame: &T) -> Result<()> {
    let mut line = serde_json::to_string(frame)?;
    line.push('\n');
    let mut writer = inner.transport_write.lock().await;
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Fail every awaiter currently parked on the pending map. Used when the
/// reader observes EOF / a fatal error, and again as a safety net inside
/// [`PluginHost::shutdown`].
async fn drain_pending(inner: &PluginHostInner) {
    let mut guard = inner.pending.lock().await;
    for (_id, sender) in guard.drain() {
        // Drop the sender; the awaiter sees `RecvError` and translates it
        // into `HostError::ConnectionLost`.
        drop(sender);
    }
}

/// Single-reader router: own the transport's read half, demultiplex
/// inbound frames to pending awaiters and notification subscribers.
///
/// Streaming JSON-RPC frame reader. Issue #241: reads raw bytes into a
/// buffer and peels off complete JSON values with
/// `serde_json::Deserializer`, independent of newline framing. Accepts
/// both the canonical NDJSON wire form and pretty-printed multi-line
/// frames for forward-compat with hosts that emit indented JSON-RPC.
async fn reader_loop(
    mut reader: Box<dyn AsyncRead + Send + Unpin>,
    inner: Arc<PluginHostInner>,
    notifications_tx: broadcast::Sender<RpcNotification>,
) {
    let mut buffer: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 4096];
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) => {
                debug!(plugin = %inner.name, "plugin stdout reached EOF; draining pending awaiters");
                break;
            }
            Ok(n) => n,
            Err(error) => {
                tracing::error!(plugin = %inner.name, %error, "plugin stdout read error; tearing down router");
                break;
            }
        };
        buffer.extend_from_slice(&chunk[..n]);

        loop {
            // Skip leading whitespace before attempting to deserialize.
            let leading_ws = buffer.iter().take_while(|b| b.is_ascii_whitespace()).count();
            if leading_ws > 0 {
                buffer.drain(..leading_ws);
            }
            if buffer.is_empty() {
                break;
            }

            let mut stream = serde_json::Deserializer::from_slice(&buffer).into_iter::<Value>();
            match stream.next() {
                Some(Ok(frame)) => {
                    let consumed = stream.byte_offset();
                    drop(stream);
                    buffer.drain(..consumed);
                    handle_frame(&inner, &notifications_tx, frame).await;
                }
                Some(Err(error)) if error.is_eof() => {
                    // Need more bytes for the current frame.
                    break;
                }
                Some(Err(error)) => {
                    tracing::error!(plugin = %inner.name, %error, "malformed JSON frame from plugin; skipping");
                    // Recover by discarding bytes up to the next newline
                    // so we can keep parsing any valid frames already
                    // buffered. If no newline is in sight, wait for more
                    // bytes — clearing the buffer here would drop unread
                    // partial frames the plugin may complete on its next
                    // write.
                    if let Some(pos) = buffer.iter().position(|b| *b == b'\n') {
                        buffer.drain(..=pos);
                        continue;
                    }
                    break;
                }
                None => break,
            }
        }
    }
    // Mark the host as dead BEFORE draining so any concurrent request()
    // that races us into the pending map sees the alive=false flag and
    // returns ConnectionLost rather than parking on a sender we just
    // dropped.
    inner.alive.store(false, Ordering::Release);
    drain_pending(inner.as_ref()).await;
    // Dropping notifications_tx here drops one of two clones (the inner
    // still holds the other). The channel only closes when the inner is
    // also torn down; subscribers observe Closed on subsequent recv()
    // once the last Arc<PluginHostInner> goes away.
    drop(notifications_tx);
}

async fn handle_frame(inner: &PluginHostInner, notifications_tx: &broadcast::Sender<RpcNotification>, frame: Value) {
    if frame.get("id").is_some() {
        let response: RpcResponse = match serde_json::from_value(frame) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(plugin = %inner.name, %error, "plugin response with id but invalid shape; skipping");
                return;
            }
        };
        let Some(id_u64) = response.id.as_ref().and_then(Value::as_u64) else {
            debug!(plugin = %inner.name, "plugin response with non-u64 id; dropping (no awaiter could match)");
            return;
        };
        let sender = inner.pending.lock().await.remove(&id_u64);
        match sender {
            Some(sender) => {
                if sender.send(response).is_err() {
                    debug!(plugin = %inner.name, id = id_u64, "awaiter gave up before response arrived");
                }
            }
            None => {
                debug!(plugin = %inner.name, id = id_u64, "received response for unknown id (awaiter timed out or never existed)");
            }
        }
    } else {
        let notification: RpcNotification = match serde_json::from_value(frame) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(plugin = %inner.name, %error, "plugin notification with invalid shape; skipping");
                return;
            }
        };
        // Broadcast send errors mean "no subscribers" — not fatal.
        let _ = notifications_tx.send(notification);
    }
}

#[cfg(test)]
mod tests {
    use animus_plugin_protocol::{PluginCapabilities, PluginInfo, RpcRequest, RpcResponse};
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    fn ok_initialize_response(id: Option<Value>, protocol_version: &str) -> RpcResponse {
        RpcResponse::ok(
            id,
            serde_json::json!(InitializeResult {
                protocol_version: protocol_version.to_string(),
                plugin_info: PluginInfo {
                    name: "test".to_string(),
                    version: "0.1.0".to_string(),
                    plugin_kind: "custom".to_string(),
                    description: None,
                },
                capabilities: PluginCapabilities::default(),
            }),
        )
    }

    async fn drive_handshake(plugin_protocol_version: &'static str) -> Result<InitializeResult> {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read initialize");
            let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse initialize");

            let response = ok_initialize_response(request.id, plugin_protocol_version);
            let mut encoded = serde_json::to_string(&response).expect("encode response");
            encoded.push('\n');
            plugin_writer.write_all(encoded.as_bytes()).await.expect("write response");

            // The host only sends `initialized` after compat check passes; reading
            // here is best-effort so rejected handshakes don't deadlock the test.
            let _ = reader.read_line(&mut line).await;
        });

        let host = PluginHost::from_streams("test", host_reader, host_writer);
        host.handshake().await
    }

    #[tokio::test]
    async fn handshake_sends_initialize_and_initialized() {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read initialize");
            let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse initialize");
            assert_eq!(request.method, "initialize");

            let response = ok_initialize_response(request.id, PROTOCOL_VERSION);
            let mut encoded = serde_json::to_string(&response).expect("encode response");
            encoded.push('\n');
            plugin_writer.write_all(encoded.as_bytes()).await.expect("write response");

            line.clear();
            reader.read_line(&mut line).await.expect("read initialized");
            let notification: serde_json::Value = serde_json::from_str(line.trim()).expect("parse initialized");
            assert_eq!(notification["method"], "initialized");
        });

        let host = PluginHost::from_streams("test", host_reader, host_writer);
        let result = host.handshake().await.expect("handshake should succeed");

        assert_eq!(result.plugin_info.name, "test");
    }

    /// Issue #241: reader_loop must accept pretty-printed multi-line
    /// JSON-RPC frames, not just NDJSON. This test drives the host
    /// against a plugin stub that writes its initialize response as a
    /// pretty-printed value (containing literal newlines mid-frame).
    #[tokio::test]
    async fn reader_loop_accepts_pretty_printed_multi_line_response() {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read initialize");
            let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse initialize");

            let response = ok_initialize_response(request.id, PROTOCOL_VERSION);
            let pretty = serde_json::to_string_pretty(&response).expect("pretty json");
            assert!(pretty.contains('\n'), "test setup expected multi-line frame");
            // Write the pretty-printed frame WITHOUT a trailing newline.
            // The streaming reader must peel it off purely by parsing.
            plugin_writer.write_all(pretty.as_bytes()).await.expect("write pretty response");

            // Drain the host's `initialized` notification so the duplex
            // doesn't block the handshake on backpressure.
            let _ = reader.read_line(&mut line).await;
        });

        let host = PluginHost::from_streams("test", host_reader, host_writer);
        let result = host.handshake().await.expect("handshake should succeed against multi-line frame");
        assert_eq!(result.plugin_info.name, "test");
    }

    #[test]
    fn check_protocol_compat_accepts_matching_major() {
        // PROTOCOL_VERSION = "1.0.0"; same major => OK.
        assert!(check_protocol_compat(PROTOCOL_VERSION).is_ok());
        assert!(check_protocol_compat("1.0.0").is_ok());
    }

    #[test]
    fn check_protocol_compat_accepts_minor_patch_drift_within_major() {
        // Host 1.0.0 + plugin 1.2.5 => OK (additive minor/patch is backwards-compatible).
        assert!(check_protocol_compat("1.2.5").is_ok());
        assert!(check_protocol_compat("1.0.99").is_ok());
        assert!(check_protocol_compat("1.999.0").is_ok());
    }

    #[test]
    fn check_protocol_compat_rejects_major_mismatch() {
        // Host 1.0.0 + plugin 2.0.0 => error.
        let err = check_protocol_compat("2.0.0").expect_err("major mismatch must fail");
        let HostError::IncompatibleProtocol(message) = err else {
            panic!("expected IncompatibleProtocol");
        };
        assert!(message.contains("major version mismatch"), "unexpected message: {message}");
    }

    #[test]
    fn check_protocol_compat_rejects_non_semver() {
        // Host 1.0.0 + plugin "garbage" => error.
        let err = check_protocol_compat("garbage").expect_err("non-semver must fail");
        let HostError::IncompatibleProtocol(message) = err else {
            panic!("expected IncompatibleProtocol");
        };
        assert!(message.contains("non-semver"), "unexpected message: {message}");
    }

    #[tokio::test]
    async fn handshake_rejects_plugin_with_major_mismatch() {
        let err = drive_handshake("2.0.0").await.expect_err("major mismatch must abort handshake");
        let message = format!("{err}");
        assert!(
            message.contains("incompatible plugin protocol") && message.contains("major version mismatch"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn resolve_capacity_priority_order() {
        // Manifest hint beats default.
        assert_eq!(resolve_broadcast_capacity(None, Some(512)), 512);
        // Spawn override beats manifest hint.
        assert_eq!(resolve_broadcast_capacity(Some(1024), Some(512)), 1024);
        // Zero hint falls through to env / default.
        std::env::remove_var(NOTIFICATION_BROADCAST_CAPACITY_ENV);
        assert_eq!(resolve_broadcast_capacity(Some(0), Some(0)), DEFAULT_NOTIFICATION_BROADCAST_CAPACITY);
        // Env override beats default when neither hint nor explicit override is set.
        std::env::set_var(NOTIFICATION_BROADCAST_CAPACITY_ENV, "777");
        assert_eq!(resolve_broadcast_capacity(None, None), 777);
        std::env::remove_var(NOTIFICATION_BROADCAST_CAPACITY_ENV);
    }

    // ===== Env scrubbing tests =====
    //
    // These exercise the v0.4.x trust-boundary promise: a spawned plugin must
    // not inherit any env var that's not in PLUGIN_BASE_ENV_ALLOWLIST and not
    // declared in its manifest. We build a tiny shell-script "plugin" that
    // serializes its env to a file, spawn it via spawn_with_options, and
    // inspect the file.

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    fn write_env_dump_plugin(dir: &std::path::Path) -> std::path::PathBuf {
        let plugin = dir.join("env-dump-plugin");
        // Dump every env var as KEY=VALUE\n into ./env.out next to argv[1].
        std::fs::write(&plugin, "#!/bin/sh\nout=\"$1\"\nenv > \"$out\"\n").expect("write env-dump plugin");
        let mut perms = std::fs::metadata(&plugin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin, perms).unwrap();
        plugin
    }

    #[cfg(unix)]
    fn read_env_dump(path: &std::path::Path) -> std::collections::HashMap<String, String> {
        let body = std::fs::read_to_string(path).expect("env dump should be written");
        let mut env = std::collections::HashMap::new();
        for line in body.lines() {
            if let Some((k, v)) = line.split_once('=') {
                env.insert(k.to_string(), v.to_string());
            }
        }
        env
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: guards std::env mutation across spawn await
    async fn env_scrubbing_strips_unrelated_vars() {
        let _guard = slot_factory_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_env_dump_plugin(dir.path());
        let env_out = dir.path().join("env.out");

        std::env::set_var("ANIMUS_TEST_SECRET", "should-not-leak");
        let result =
            PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], PluginSpawnOptions::default()).await;
        let host = result.expect("spawn should succeed");
        // Wait long enough for the script to flush + exit. Shutdown is the
        // cleanest way to reap the child; we don't care about the response.
        let _ = host.shutdown().await;
        std::env::remove_var("ANIMUS_TEST_SECRET");

        let env = read_env_dump(&env_out);
        assert!(!env.contains_key("ANIMUS_TEST_SECRET"), "env_clear() must strip ANIMUS_TEST_SECRET; saw env={env:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: guards std::env mutation across spawn await
    async fn env_scrubbing_keeps_declared_vars() {
        let _guard = slot_factory_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_env_dump_plugin(dir.path());
        let env_out = dir.path().join("env.out");

        std::env::set_var("ANIMUS_TEST_OPENAI_KEY", "sk-test-value");
        let manifest_env = vec![EnvRequirement {
            name: "ANIMUS_TEST_OPENAI_KEY".to_string(),
            description: None,
            sensitive: true,
            required: true,
        }];
        let opts =
            PluginSpawnOptions::for_manifest("env-dump-plugin", &manifest_env, std::iter::empty::<String>(), None);

        let host = PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], opts).await.expect("spawn");
        let _ = host.shutdown().await;
        std::env::remove_var("ANIMUS_TEST_OPENAI_KEY");

        let env = read_env_dump(&env_out);
        assert_eq!(
            env.get("ANIMUS_TEST_OPENAI_KEY").map(String::as_str),
            Some("sk-test-value"),
            "declared env var must be forwarded; saw env={env:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: guards std::env mutation across spawn await
    async fn env_scrubbing_always_includes_path_and_home() {
        let _guard = slot_factory_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_env_dump_plugin(dir.path());
        let env_out = dir.path().join("env.out");

        // PATH and HOME are always set on a unix dev/CI machine.
        let host = PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], PluginSpawnOptions::default())
            .await
            .expect("spawn");
        let _ = host.shutdown().await;

        let env = read_env_dump(&env_out);
        assert!(env.contains_key("PATH"), "PATH must be in the base allowlist; saw env={env:?}");
        assert!(env.contains_key("HOME"), "HOME must be in the base allowlist; saw env={env:?}");
    }

    /// Minimal `ProcessSlotFactory` used by the cap-enforcement test. Tracks
    /// the live count itself (independent of the daemon-runtime global
    /// counter) so the test is hermetic and doesn't race other tests in the
    /// binary.
    #[derive(Debug)]
    struct CappedFactory {
        cap: usize,
        live: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[derive(Debug)]
    struct CappedGuard {
        live: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ProcessSlotGuard for CappedGuard {}

    impl Drop for CappedGuard {
        fn drop(&mut self) {
            self.live.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl ProcessSlotFactory for CappedFactory {
        fn acquire(&self) -> Result<BoxedProcessSlotGuard, ProcessSlotError> {
            loop {
                let current = self.live.load(std::sync::atomic::Ordering::SeqCst);
                if current >= self.cap {
                    return Err(ProcessSlotError {
                        current,
                        cap: self.cap,
                        message: format!("test cap reached ({} live, max {})", current, self.cap),
                    });
                }
                if self
                    .live
                    .compare_exchange(
                        current,
                        current + 1,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    return Ok(Box::new(CappedGuard { live: self.live.clone() }));
                }
            }
        }
    }

    /// Serialize the slot-factory tests so they don't race each other on the
    /// process-wide installed factory.
    fn slot_factory_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn process_slot_factory_enforces_cap_and_releases_on_drop() {
        let _guard = slot_factory_lock().lock().unwrap_or_else(|p| p.into_inner());

        let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let factory: Arc<dyn ProcessSlotFactory> = Arc::new(CappedFactory { cap: 2, live: live.clone() });
        install_process_slot_factory_for_test(factory.clone());

        // Drive the cap via the installed-factory path so we exercise the
        // exact wiring `spawn_with_options` uses.
        let installed = current_process_slot_factory().expect("factory installed");

        let slot1 = installed.acquire().expect("1st under cap");
        let slot2 = installed.acquire().expect("2nd under cap");
        assert_eq!(live.load(std::sync::atomic::Ordering::SeqCst), 2);

        let denied = installed.acquire();
        let err = denied.expect_err("3rd acquire must be refused at cap");
        assert_eq!(err.cap, 2);
        assert_eq!(err.current, 2);
        assert!(err.message.contains("test cap reached"), "unexpected message: {}", err.message);

        // Drop one slot — a fresh acquire must succeed and reuse the freed slot.
        drop(slot1);
        let slot3 = installed.acquire().expect("recovered slot after drop");
        assert_eq!(live.load(std::sync::atomic::Ordering::SeqCst), 2);

        drop(slot2);
        drop(slot3);
        assert_eq!(live.load(std::sync::atomic::Ordering::SeqCst), 0);

        clear_process_slot_factory_for_test();
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: serializes process-quota tests across spawn awaits
    async fn spawn_with_options_refuses_at_cap() {
        let _guard = slot_factory_lock().lock().unwrap_or_else(|p| p.into_inner());

        let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let factory: Arc<dyn ProcessSlotFactory> = Arc::new(CappedFactory { cap: 2, live: live.clone() });
        install_process_slot_factory_for_test(factory);

        let dir = tempfile::tempdir().unwrap();
        let plugin = write_env_dump_plugin(dir.path());
        let env_out = dir.path().join("env.out");

        // First two spawns should succeed (slots 1 and 2). The plugins are
        // trivial shell scripts that exit immediately, but the slot lives
        // until shutdown drops it — so we deliberately keep the hosts alive.
        let host1 =
            PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], PluginSpawnOptions::default())
                .await
                .expect("first spawn under cap");
        let host2 =
            PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], PluginSpawnOptions::default())
                .await
                .expect("second spawn under cap");
        assert_eq!(live.load(std::sync::atomic::Ordering::SeqCst), 2);

        // Third spawn must fail with our ProcessSlotError surfacing through anyhow.
        let denied =
            PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], PluginSpawnOptions::default()).await;
        let err = match denied {
            Ok(_) => panic!("third spawn must be refused at cap"),
            Err(err) => err,
        };
        let msg = format!("{err}");
        assert!(msg.contains("test cap reached"), "expected refusal to surface ProcessSlotError, got: {msg}");

        // Drop one slot via shutdown; a fresh spawn must succeed.
        host1.shutdown().await.ok();
        // Shutdown releases the slot eagerly — but the dropped child's stderr
        // task may still hold an Arc briefly. Give it a tick.
        tokio::task::yield_now().await;

        let host3 =
            PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], PluginSpawnOptions::default())
                .await
                .expect("spawn should succeed after slot freed");

        host2.shutdown().await.ok();
        host3.shutdown().await.ok();
        clear_process_slot_factory_for_test();
    }

    #[test]
    fn for_manifest_reports_missing_required_vars() {
        let unique = format!("ANIMUS_TEST_REQUIRED_MISSING_{}", std::process::id());
        // Ensure unset
        std::env::remove_var(&unique);
        let manifest_env = vec![
            EnvRequirement { name: unique.clone(), description: None, sensitive: false, required: true },
            EnvRequirement { name: format!("{unique}_OPTIONAL"), description: None, sensitive: false, required: false },
        ];
        let opts = PluginSpawnOptions::for_manifest("plugin-name", &manifest_env, std::iter::empty::<String>(), None);
        assert!(opts.missing_required_env.contains(&unique));
        assert!(!opts.missing_required_env.iter().any(|v| v.ends_with("_OPTIONAL")));
        // Both names should be in the allowlist regardless of "required".
        assert!(opts.env_allowlist.contains(&unique));
        assert!(opts.env_allowlist.contains(&format!("{unique}_OPTIONAL")));
    }
}
