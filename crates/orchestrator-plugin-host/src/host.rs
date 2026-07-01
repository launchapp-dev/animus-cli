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

/// Maximum cumulative bytes (sum of `key.len() + value.len()`) the spawn
/// path will merge from the installed [`SecretSnapshotProvider`] into a
/// child plugin's environment. The cap protects against a runaway
/// keychain index from blowing past the platform's `ARG_MAX`. Mirrors
/// `orchestrator_core::MAX_INJECTED_ENV_BYTES`.
pub const MAX_INJECTED_SECRET_BYTES: usize = 1024 * 1024;

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

/// Generous upper bound for a single frame write (mutex + write_all + flush)
/// on the untimed request path. A healthy plugin drains its stdin promptly;
/// one that stops reading would otherwise block the writer mutex forever and
/// wedge every queued request, including shutdown. Expiry marks the host
/// dead and surfaces as `ConnectionLost` to callers.
const WRITE_FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on the reader's unparsed frame buffer. A plugin that streams an
/// endless (or endlessly malformed) frame without ever completing it would
/// otherwise grow the buffer without bound; past this point the router tears
/// down and every awaiter observes [`HostError::ConnectionLost`].
const READER_BUFFER_CAP: usize = 8 * 1024 * 1024;

/// How many trailing stderr lines to retain per plugin for failure diagnostics.
const STDERR_TAIL_CAP: usize = 40;

/// Max bytes retained per captured stderr line (bounds memory for a plugin that
/// emits pathologically long lines; the tail is diagnostics, not a full log).
const MAX_STDERR_LINE_LEN: usize = 512;

/// Truncate `line` to at most [`MAX_STDERR_LINE_LEN`] bytes on a char boundary,
/// appending an ellipsis when clipped. Keeps the ring buffer bounded.
fn clip_stderr_line(line: String) -> String {
    if line.len() <= MAX_STDERR_LINE_LEN {
        return line;
    }
    let mut end = MAX_STDERR_LINE_LEN;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

/// Matches a credential label anywhere in a stderr line, tolerating word breaks
/// (`api key`, `api_key`, `api-key`) and any following separator/spacing. Once
/// matched, everything from the label to end-of-line is masked, so the value is
/// redacted regardless of how it is delimited or whether it spans tokens (PEM
/// blob, JSON, `password = hunter2`, `Authorization: Bearer <tok>`).
fn secret_marker_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(pass(?:word|wd)?|pwd|secret|token|api[ _-]?key|access[ _-]?key|client[ _-]?secret|authorization|credential|private[ _-]?key|session[ _-]?key|bearer|basic)",
        )
        .expect("valid secret-marker regex")
    })
}

/// Redact credentials from a stderr line before it is surfaced in a user-visible
/// error, so secrets stay in operator logs and do not leak into RPC/CLI errors.
///
/// Two passes: (1) mask the password of any `scheme://user:pass@host` connection
/// string (a Postgres plugin echoing its DATABASE_URL on a connect failure), even
/// when no credential *label* is present; (2) if a credential label (`password`,
/// `api key`, `Authorization`, `bearer`, ...) appears, keep the text up to and
/// including the label and mask everything after it. This over-redacts the tail of
/// such a line — the stderr tail is failure diagnostics, not a full log — but
/// never leaks the value regardless of separator, spacing, or token spanning.
fn redact_stderr_line(line: &str) -> String {
    let url_redacted = redact_url_userinfo(line);
    if let Some(m) = secret_marker_regex().find(&url_redacted) {
        return format!("{} ***", url_redacted[..m.end()].trim_end());
    }
    url_redacted
}

/// Mask the password in any `scheme://user:pass@host` token: `user:***@host`.
/// Uses the LAST `@` so an unescaped `@` inside the password cannot leave a
/// suffix unmasked (`u:p@ss@host` -> `u:***@host`).
fn redact_url_userinfo(line: &str) -> String {
    line.split(' ')
        .map(|token| {
            if let Some(scheme_end) = token.find("://") {
                let after = &token[scheme_end + 3..];
                if let Some(at) = after.rfind('@') {
                    let userinfo = &after[..at];
                    if let Some(colon) = userinfo.find(':') {
                        return format!("{}{}:***@{}", &token[..scheme_end + 3], &userinfo[..colon], &after[at + 1..]);
                    }
                }
            }
            token.to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

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
    /// The plugin manifest's declared
    /// [`PluginManifest::notification_buffer_size`] hint. Lower precedence
    /// than `notification_capacity`, higher than the env override and the
    /// compiled default. Set via [`PluginSpawnOptions::with_notification_buffer_hint`]
    /// by callers that hold the plugin's manifest at spawn time.
    ///
    /// [`PluginManifest::notification_buffer_size`]: animus_plugin_protocol::PluginManifest::notification_buffer_size
    pub notification_buffer_hint: Option<usize>,
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
            notification_buffer_hint: None,
            working_dir: None,
        }
    }

    /// Carry the plugin manifest's `notification_buffer_size` hint into the
    /// spawn so [`resolve_broadcast_capacity`]'s documented priority chain
    /// (explicit override → manifest hint → env override → default) actually
    /// sees the manifest value.
    #[must_use]
    pub fn with_notification_buffer_hint(mut self, hint: Option<usize>) -> Self {
        self.notification_buffer_hint = hint;
        self
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

/// Process-wide hook that supplies the keychain-backed secret snapshot the
/// spawn path merges into each plugin's child environment.
///
/// Decoupled from `orchestrator-core` so this crate stays dependency-free
/// w.r.t. the secret store: the daemon installs a real implementation at
/// startup, tests may install a mock, and any embedder that hasn't opted
/// in gets the historical "no extra env" behaviour.
pub trait SecretSnapshotProvider: Send + Sync + 'static {
    /// Return the (KEY, VALUE) pairs the host should merge into the
    /// next-spawned plugin's environment, **before** the caller's process
    /// environment is applied. Existing parent-process env wins on
    /// collision so explicit `KEY=val animus daemon start` overrides the
    /// keychain entry.
    ///
    /// Implementations MUST cap their own output at
    /// `orchestrator_core::MAX_INJECTED_ENV_BYTES` bytes cumulative
    /// (sum of key.len() + value.len()) and return the empty map on any
    /// error so a broken keychain never blocks plugin spawn.
    fn snapshot(&self) -> std::collections::BTreeMap<String, String>;

    /// Return only the entries whose KEYs appear in `requested`. Default
    /// impl scans the full snapshot; production providers SHOULD
    /// override this to avoid touching keychain items the caller does
    /// not need (so a plugin with no `env_required` block never causes
    /// the OS to prompt for unrelated secrets). (codex round-3 P2.)
    fn snapshot_filtered(&self, requested: &[String]) -> std::collections::BTreeMap<String, String> {
        if requested.is_empty() {
            return std::collections::BTreeMap::new();
        }
        let mut all = self.snapshot();
        all.retain(|k, _| requested.iter().any(|r| r == k));
        all
    }
}

fn secret_snapshot_provider_slot() -> &'static RwLock<Option<Arc<dyn SecretSnapshotProvider>>> {
    static SLOT: OnceLock<RwLock<Option<Arc<dyn SecretSnapshotProvider>>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install the process-wide [`SecretSnapshotProvider`]. First-installer-wins
/// to match [`install_process_slot_factory`] semantics; tests use
/// [`install_secret_snapshot_provider_for_test`] to swap unconditionally.
pub fn install_secret_snapshot_provider(provider: Arc<dyn SecretSnapshotProvider>) -> bool {
    let mut guard = secret_snapshot_provider_slot().write().expect("secret snapshot provider lock poisoned");
    if guard.is_some() {
        return false;
    }
    *guard = Some(provider);
    true
}

/// Test-only: unconditionally replace the installed provider.
#[cfg(any(test, feature = "test-support"))]
pub fn install_secret_snapshot_provider_for_test(provider: Arc<dyn SecretSnapshotProvider>) {
    let mut guard = secret_snapshot_provider_slot().write().expect("secret snapshot provider lock poisoned");
    *guard = Some(provider);
}

/// Test-only: clear the installed provider so the spawn path skips the
/// keychain merge entirely.
#[cfg(any(test, feature = "test-support"))]
pub fn clear_secret_snapshot_provider_for_test() {
    let mut guard = secret_snapshot_provider_slot().write().expect("secret snapshot provider lock poisoned");
    *guard = None;
}

/// Snapshot of the currently-installed [`SecretSnapshotProvider`], if
/// any. Exposed so out-of-tree subprocess spawn paths (e.g. the daemon's
/// `ProcessManager` workflow-runner spawn) can merge keychain entries
/// into their own command env without going through
/// [`PluginHost::spawn_with_options`].
pub fn current_secret_snapshot_provider() -> Option<Arc<dyn SecretSnapshotProvider>> {
    secret_snapshot_provider_slot().read().expect("secret snapshot provider lock poisoned").clone()
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
    /// Ring buffer of the plugin's most recent stderr lines (last
    /// [`STDERR_TAIL_CAP`]), captured by the stderr reader task. Surfaced in
    /// handshake / ConnectionLost errors so a plugin that dies during startup
    /// reports WHY (its stderr) instead of an opaque "connection lost". Empty
    /// for in-memory test hosts.
    stderr_tail: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
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
            .kill_on_drop(true)
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

        // Precedence (lowest to highest): keychain entries -> parent env.
        // Applying keychain first lets explicit `KEY=val animus daemon start`
        // still win, matching the documented contract in
        // `docs/reference/secrets.md`.
        let mut injected_keys: BTreeSet<String> = BTreeSet::new();
        if let Some(provider) = current_secret_snapshot_provider() {
            // Request only the secrets the plugin's manifest declares
            // AND that are not already satisfied by the parent
            // environment. The latter avoids touching the keychain for
            // entries an explicit `KEY=val animus daemon start` will
            // overwrite anyway — important on locked-keychain
            // platforms that prompt on access. (codex round-7 P2.)
            let requested: Vec<String> = options
                .env_allowlist
                .iter()
                .filter(|name| std::env::var_os(name.as_str()).is_none())
                .cloned()
                .collect();
            let snapshot = if requested.is_empty() {
                std::collections::BTreeMap::new()
            } else {
                provider.snapshot_filtered(&requested)
            };
            let mut total: usize = 0;
            let mut injected = 0usize;
            let mut skipped = 0usize;
            for (key, value) in snapshot {
                let next = total.saturating_add(key.len()).saturating_add(value.len());
                if next > MAX_INJECTED_SECRET_BYTES {
                    skipped += 1;
                    warn!(
                        plugin = %name,
                        skipped_key = %key,
                        "secret entry skipped: would exceed {MAX_INJECTED_SECRET_BYTES}-byte cumulative cap"
                    );
                    continue;
                }
                command.env(&key, value);
                injected_keys.insert(key);
                total = next;
                injected += 1;
            }
            if injected > 0 || skipped > 0 {
                debug!(plugin = %name, injected, skipped, "merged keychain-backed secrets into plugin env");
            }
        }

        for var in &allow {
            if let Some(value) = std::env::var_os(var) {
                command.env(var, value);
            }
        }

        // Bound each plugin's tokio runtime. A bare `#[tokio::main]` (every Animus
        // stdio plugin) sizes its multi-thread worker pool to
        // `available_parallelism()` — all CPU cores. With v0.6's resident-plugin
        // fleet (config_source + subject backends + queue + workflow_runner +
        // providers + transport) that is hundreds of threads on a many-core host,
        // exhausting the PID/thread budget so new forks — including the provider CLI
        // an agent phase spawns — fail with EAGAIN and the run hangs. Plugins are
        // I/O-bound stdio RPC servers, so a tiny pool is sufficient. `env_clear()`
        // above dropped any inherited value (TOKIO_WORKER_THREADS is not in the base
        // allowlist), so set it explicitly here, honoring an operator override on the
        // daemon env so a deploy can still tune it up or down.
        command.env(
            "TOKIO_WORKER_THREADS",
            std::env::var_os("TOKIO_WORKER_THREADS").unwrap_or_else(|| std::ffi::OsString::from("2")),
        );

        for missing in &options.missing_required_env {
            // Suppress the warning when the keychain already satisfied
            // the requirement during the snapshot merge above. The
            // `missing_required_env` list was computed at
            // `for_manifest` time from `std::env` only, so without this
            // check every successful keychain-backed spawn would print
            // a spurious "plugin will likely fail" warning.
            // (codex round-2 P3.)
            if injected_keys.contains(missing) {
                continue;
            }
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

        let capacity = resolve_broadcast_capacity(options.notification_capacity, options.notification_buffer_hint);
        let host = Self::launch_with_slot(name, Box::new(stdout), Box::new(stdin), Some(child), capacity, process_slot);

        // Capture the plugin's stderr into a bounded ring buffer (in addition to
        // the standard warn! + optional sink) so a startup/handshake failure can
        // report the plugin's own last words instead of a bare "connection lost".
        let stderr_tail = host.inner.stderr_tail.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                warn!(plugin = %stderr_plugin_name, "{}", line);
                if let Some(sink) = stderr_sink.as_ref() {
                    sink(&stderr_plugin_name, &line);
                }
                if let Ok(mut buf) = stderr_tail.lock() {
                    if buf.len() >= STDERR_TAIL_CAP {
                        buf.pop_front();
                    }
                    // Redact BEFORE clipping: clipping first could split a
                    // `scheme://user:pass@host` across the byte cutoff and defeat
                    // the URL detector, leaking the password prefix. The raw line
                    // still reaches operator logs above via `warn!`/`sink`.
                    buf.push_back(clip_stderr_line(redact_stderr_line(&line)));
                }
            }
        });

        Ok(host)
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

    /// Test-only: build an in-memory host with an explicit reader buffer cap
    /// so the overflow teardown path can be exercised without pushing the
    /// production [`READER_BUFFER_CAP`] worth of bytes through a duplex.
    #[cfg(test)]
    fn from_streams_with_reader_buffer_cap<R, W>(name: impl Into<String>, reader: R, writer: W, cap: usize) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::launch_full(
            name.into(),
            Box::new(reader),
            Box::new(writer),
            None,
            DEFAULT_NOTIFICATION_BROADCAST_CAPACITY,
            None,
            cap,
        )
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
        Self::launch_full(name, reader, writer, child, notification_capacity, process_slot, READER_BUFFER_CAP)
    }

    #[allow(clippy::too_many_arguments)]
    fn launch_full(
        name: String,
        reader: Box<dyn AsyncRead + Send + Unpin>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
        child: Option<Child>,
        notification_capacity: usize,
        process_slot: Option<BoxedProcessSlotGuard>,
        reader_buffer_cap: usize,
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
            stderr_tail: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
        });

        let reader_inner = inner.clone();
        let handle = tokio::spawn(reader_loop(reader, reader_inner, notifications_tx, reader_buffer_cap));
        // Stash the handle synchronously so shutdown() can find it without
        // racing the spawn that owns the reader loop.
        *inner.reader_handle.lock().expect("reader_handle mutex poisoned at launch") = Some(handle);

        Self { inner }
    }

    /// Plugin name (label) — same as the `name` field passed to spawn.
    /// Return the OS process id of the spawned child, if one is owned by
    /// this host. Returns `None` for in-memory hosts constructed by tests
    /// via [`Self::from_streams`], or when the child mutex is currently
    /// held by another task (so the synchronous accessor never blocks).
    pub fn child_pid(&self) -> Option<u32> {
        let guard = self.inner.child.try_lock().ok()?;
        guard.as_ref().and_then(|child| child.id())
    }

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
    /// A short diagnostic suffix built from the plugin's captured stderr tail,
    /// appended to startup/handshake failures so an opaque transport error
    /// ("connection lost") carries the plugin's own last words. When the plugin
    /// died without printing anything, that itself is the signal (killed by a
    /// signal / OOM / exec failure).
    fn stderr_tail_context(&self) -> String {
        match self.inner.stderr_tail.lock() {
            Ok(buf) if !buf.is_empty() => {
                let tail: Vec<String> = buf.iter().rev().take(15).map(|line| redact_stderr_line(line)).collect();
                let joined = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
                format!("; last plugin stderr:\n{joined}")
            }
            _ => "; plugin emitted no stderr before exit (likely killed by a signal / OOM, or an exec/fork failure)"
                .to_string(),
        }
    }

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
            .map_err(|error| {
                anyhow!("plugin '{}' initialize failed: {error}{}", self.inner.name, self.stderr_tail_context())
            })?;

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
        let _ = tokio::time::timeout(
            SHUTDOWN_GRACE,
            write_frame_inner(inner.as_ref(), &RpcNotification::new("exit", None)),
        )
        .await;

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

    /// Like [`PluginHost::request_with_timeout`] but also returns the JSON-RPC
    /// request id the host allocated for this call.
    ///
    /// Needed by streaming methods whose plugin emits correlated
    /// notifications: the `subject/watch` runtime, for example, echoes the
    /// request id in every `subject/changed` notification's `params.id`, so a
    /// subscriber must know its own id to demultiplex notifications when
    /// several watch RPCs share one plugin host's notification broadcast.
    pub async fn request_with_timeout_capturing_id(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
        timeout: Duration,
    ) -> (u64, Result<Value, RpcError>) {
        let method = method.into();
        let (id, raw) = self.request_raw_with_timeout_capturing_id(&method, params, timeout).await;
        let result = match raw {
            Ok(response) => match response.error {
                Some(error) => Err(error),
                None => Ok(response.result.unwrap_or(Value::Null)),
            },
            Err(host_error) => Err(RpcError::from(host_error)),
        };
        (id, result)
    }

    async fn request_raw_with_timeout(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<RpcResponse, HostError> {
        self.request_raw_with_timeout_capturing_id(method, params, timeout).await.1
    }

    /// Implementation shared by [`Self::request_raw_with_timeout`] and
    /// [`Self::request_with_timeout_capturing_id`]. Returns the allocated id
    /// alongside the raw result. On the early-out paths (dead host before the
    /// id is allocated) the returned id is `0`, which is never a live request
    /// id (the counter starts at 1).
    async fn request_raw_with_timeout_capturing_id(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> (u64, Result<RpcResponse, HostError>) {
        if !self.inner.alive.load(Ordering::Acquire) {
            return (0, Err(HostError::ConnectionLost));
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        (id, self.request_raw_with_id(id, method, params, timeout).await)
    }

    async fn request_raw_with_id(
        &self,
        id: u64,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<RpcResponse, HostError> {
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);

        // Re-check liveness AFTER the insert. The reader task flips alive to
        // false and THEN drains the pending map; an insert that raced past
        // that drain would otherwise park on a sender nobody will answer.
        // The pending mutex orders the two sides: if the drain missed this
        // entry, this load is guaranteed to observe alive == false.
        if !self.inner.alive.load(Ordering::Acquire) {
            self.inner.pending.lock().await.remove(&id);
            return Err(HostError::ConnectionLost);
        }

        // One deadline covers BOTH the stdin write and the response wait so a
        // plugin that stopped reading its stdin can't pin the caller past the
        // requested timeout while it holds the transport_write mutex.
        let deadline = tokio::time::Instant::now() + timeout;
        match write_frame_bounded(self.inner.as_ref(), &RpcRequest::new(id, method, params), deadline).await {
            Ok(()) => {}
            Err(FrameWriteFailure::Io(_error)) => {
                self.inner.pending.lock().await.remove(&id);
                return Err(HostError::ConnectionLost);
            }
            Err(FrameWriteFailure::LockWait) => {
                // The deadline expired while we were still queued behind
                // another writer: no bytes from THIS frame hit the wire, so
                // the transport stays coherent for everyone else. Only this
                // request fails.
                self.inner.pending.lock().await.remove(&id);
                return Err(HostError::Timeout(timeout));
            }
            Err(FrameWriteFailure::MidWrite) => {
                // The write itself blew the deadline. The frame may be
                // half-written, so the transport can no longer be trusted;
                // write_frame_bounded already marked the host dead and
                // drained every parked awaiter.
                return Err(HostError::Timeout(timeout));
            }
        }

        match tokio::time::timeout_at(deadline, rx).await {
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

    // Re-check liveness AFTER the insert. The reader task flips alive to
    // false and THEN drains the pending map; an insert that raced past that
    // drain would otherwise park forever on the untimed path. The pending
    // mutex orders the two sides: if the drain missed this entry, this load
    // is guaranteed to observe alive == false.
    if !inner.alive.load(Ordering::Acquire) {
        inner.pending.lock().await.remove(&id);
        return Err(HostError::ConnectionLost);
    }

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

/// How a deadline-bounded frame write failed. The distinction matters: a
/// timeout while still queued on the writer mutex leaves zero bytes of this
/// frame on the wire (transport intact), while a timeout mid-write may leave
/// a partial frame behind (transport corrupt — host must die).
enum FrameWriteFailure {
    /// Serialization or I/O error from the underlying writer.
    Io(anyhow::Error),
    /// Deadline expired while waiting for the `transport_write` mutex. The
    /// frame was never started; the transport is still coherent.
    LockWait,
    /// Deadline expired after the write began. A partial frame may be on the
    /// wire; the host has been marked dead and pending awaiters drained.
    MidWrite,
}

/// Write one frame with `deadline` covering both the writer-mutex wait and
/// the write+flush. Only a mid-write expiry poisons the host.
async fn write_frame_bounded<T: serde::Serialize>(
    inner: &PluginHostInner,
    frame: &T,
    deadline: tokio::time::Instant,
) -> std::result::Result<(), FrameWriteFailure> {
    let mut line = match serde_json::to_string(frame) {
        Ok(line) => line,
        Err(error) => return Err(FrameWriteFailure::Io(error.into())),
    };
    line.push('\n');
    let mut writer = match tokio::time::timeout_at(deadline, inner.transport_write.lock()).await {
        Ok(writer) => writer,
        Err(_) => return Err(FrameWriteFailure::LockWait),
    };
    let write = async {
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await?;
        Ok::<(), std::io::Error>(())
    };
    match tokio::time::timeout_at(deadline, write).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(FrameWriteFailure::Io(error.into())),
        Err(_) => {
            // A wedged plugin stopped draining its stdin while we held the
            // transport_write mutex. Cancelling the write releases the mutex
            // but may leave a partial frame on the wire, so the host is done:
            // flip alive off and fail every parked awaiter.
            inner.alive.store(false, Ordering::Release);
            drain_pending(inner).await;
            Err(FrameWriteFailure::MidWrite)
        }
    }
}

async fn write_frame_inner<T: serde::Serialize>(inner: &PluginHostInner, frame: &T) -> Result<()> {
    let deadline = tokio::time::Instant::now() + WRITE_FRAME_TIMEOUT;
    match write_frame_bounded(inner, frame, deadline).await {
        Ok(()) => Ok(()),
        Err(FrameWriteFailure::Io(error)) => Err(error),
        Err(FrameWriteFailure::LockWait) => Err(anyhow!(
            "plugin '{}' stdin write timed out after {:?} waiting for the writer; frame not sent",
            inner.name,
            WRITE_FRAME_TIMEOUT
        )),
        Err(FrameWriteFailure::MidWrite) => Err(anyhow!(
            "plugin '{}' stdin write timed out after {:?}; marking host dead",
            inner.name,
            WRITE_FRAME_TIMEOUT
        )),
    }
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
    buffer_cap: usize,
) {
    let mut buffer: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 4096];
    // Set after a definitively-malformed frame with no newline in sight: the
    // rest of that line is known garbage, so drop bytes until the next
    // newline instead of re-parsing (and re-logging) the same prefix on
    // every chunk.
    let mut skip_to_newline = false;
    'read: loop {
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
        let mut data = &chunk[..n];
        if skip_to_newline {
            match data.iter().position(|b| *b == b'\n') {
                Some(pos) => {
                    data = &data[pos + 1..];
                    skip_to_newline = false;
                }
                None => continue,
            }
        }
        buffer.extend_from_slice(data);
        if buffer.len() > buffer_cap {
            tracing::error!(
                plugin = %inner.name,
                buffered = buffer.len(),
                cap = buffer_cap,
                "plugin frame buffer exceeded cap without a complete frame; tearing down router"
            );
            break;
        }

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
                    // buffered. If no newline is in sight, the rest of the
                    // line is known garbage: drop what we have and skip
                    // incoming bytes until a newline arrives so the same
                    // failed prefix isn't re-parsed and re-logged on every
                    // chunk.
                    if let Some(pos) = buffer.iter().position(|b| *b == b'\n') {
                        buffer.drain(..=pos);
                        continue;
                    }
                    buffer.clear();
                    skip_to_newline = true;
                    continue 'read;
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

    #[test]
    fn redact_stderr_line_masks_credentials_without_leaking_tails() {
        // URL userinfo password, even with no credential label and an '@' in it.
        assert_eq!(
            redact_stderr_line("connect failed postgres://user:p@ss@db:5432/x"),
            "connect failed postgres://user:***@db:5432/x"
        );
        // Labelled secrets with assorted separators / spacing — value + rest of
        // line masked, prefix kept.
        assert_eq!(redact_stderr_line("db password = hunter2 for role app"), "db password ***");
        assert_eq!(redact_stderr_line("using api key: sk-abc123 now"), "using api key ***");
        assert_eq!(redact_stderr_line("Authorization: Bearer sk-xyz trailing"), "Authorization ***");
        // Multi-token / PEM value cannot leak its tail.
        assert_eq!(
            redact_stderr_line("private_key=-----BEGIN KEY----- abcd -----END KEY-----"),
            "private_key ***"
        );
        // Query-string credential in a URL without userinfo.
        assert_eq!(
            redact_stderr_line("cb https://h/x?client_id=a&client_secret=zzz"),
            "cb https://h/x?client_id=a&client_secret ***"
        );
        // A line with no credential material is untouched.
        assert_eq!(redact_stderr_line("could not connect to host db:5432"), "could not connect to host db:5432");
    }

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

    /// A timed request against a host whose reader task already exited must
    /// short-circuit with `ConnectionLost` instead of parking an awaiter that
    /// can never be answered and burning the full timeout into a misleading
    /// `Timeout` error.
    #[tokio::test]
    async fn timed_request_short_circuits_when_host_already_dead() {
        let (host_reader, plugin_writer) = duplex(8192);
        let (_plugin_reader, host_writer) = duplex(8192);
        let host = PluginHost::from_streams("test", host_reader, host_writer);

        // Close the plugin's stdout: the reader task sees EOF and flips alive off.
        drop(plugin_writer);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let started = std::time::Instant::now();
        let err = host
            .request_typed_with_timeout("$/ping", None, Duration::from_secs(30))
            .await
            .expect_err("request against a dead host must fail");
        assert!(matches!(err, HostError::ConnectionLost), "expected ConnectionLost, got: {err:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "alive check must short-circuit, not burn the timeout");
    }

    /// An untimed request racing the reader teardown (alive observed true,
    /// reader flips alive + drains pending, THEN the request inserts its
    /// awaiter) must not park forever: the post-insert alive re-check
    /// converts the race into `ConnectionLost`. Looped to give the race a
    /// real chance to interleave; without the re-check this test hangs into
    /// the outer timeout.
    #[tokio::test]
    async fn untimed_request_racing_reader_teardown_returns_connection_lost() {
        for _ in 0..50 {
            let (host_reader, plugin_writer) = duplex(8192);
            let (_plugin_reader, host_writer) = duplex(64 * 1024);
            let host = PluginHost::from_streams("test", host_reader, host_writer);

            let racing_host = host.clone();
            let request = tokio::spawn(async move { racing_host.request_typed("$/ping", None).await });
            // Close the plugin's stdout concurrently with the request: the
            // reader task sees EOF, flips alive off, and drains pending.
            drop(plugin_writer);

            let outcome = tokio::time::timeout(Duration::from_secs(5), request)
                .await
                .expect("untimed request must not hang after reader teardown")
                .expect("request task must not panic");
            let err = outcome.expect_err("no response was ever sent");
            assert!(matches!(err, HostError::ConnectionLost), "expected ConnectionLost, got: {err:?}");
        }
    }

    /// A plugin that stops draining its stdin must not pin a timed request
    /// past its deadline: the write is covered by the same deadline as the
    /// response wait, and expiry marks the host dead so follow-up requests
    /// fail fast instead of queuing on the wedged writer mutex.
    #[tokio::test]
    async fn timed_request_write_respects_deadline_against_wedged_stdin() {
        // Tiny write buffer + a plugin that never reads its stdin: the frame
        // write blocks after 64 bytes.
        let (host_reader, _plugin_writer) = duplex(8192);
        let (_plugin_reader, host_writer) = duplex(64);
        let host = PluginHost::from_streams("test", host_reader, host_writer);

        let big_params = serde_json::json!({ "blob": "x".repeat(64 * 1024) });
        let started = std::time::Instant::now();
        let err = host
            .request_typed_with_timeout("agent/run", Some(big_params), Duration::from_millis(200))
            .await
            .expect_err("write against a wedged stdin must fail");
        assert!(matches!(err, HostError::Timeout(_)), "expected Timeout, got: {err:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "write must be bounded by the request deadline");

        // The wedged write marked the host dead: the next request fails fast
        // with ConnectionLost instead of queuing behind the dead transport.
        let err = host
            .request_typed_with_timeout("$/ping", None, Duration::from_secs(30))
            .await
            .expect_err("follow-up request must fail");
        assert!(matches!(err, HostError::ConnectionLost), "expected ConnectionLost, got: {err:?}");
    }

    /// A short-timeout request that expires while still QUEUED behind another
    /// request's write must not poison the shared host: none of its bytes hit
    /// the wire, so only the queued request fails (plain `Timeout`) and the
    /// host stays alive for everyone else.
    #[tokio::test]
    async fn lock_wait_timeout_fails_only_the_queued_request() {
        let (host_reader, _plugin_writer) = duplex(8192);
        let (_plugin_reader, host_writer) = duplex(64);
        let host = PluginHost::from_streams("test", host_reader, host_writer);

        // Request A wedges mid-write with a long deadline, holding the
        // writer mutex.
        let host_a = host.clone();
        let blocker = tokio::spawn(async move {
            let big = serde_json::json!({ "blob": "x".repeat(64 * 1024) });
            host_a.request_typed_with_timeout("agent/run", Some(big), Duration::from_secs(30)).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Request B expires waiting for the mutex: plain Timeout, host alive.
        let err = host
            .request_typed_with_timeout("$/ping", None, Duration::from_millis(100))
            .await
            .expect_err("queued request must time out");
        assert!(matches!(err, HostError::Timeout(_)), "lock-wait expiry must be a plain Timeout, got: {err:?}");
        assert!(host.inner.alive.load(Ordering::Acquire), "a lock-wait timeout must not mark the shared host dead");
        blocker.abort();
    }

    /// Malformed bytes with no trailing newline must not wedge the reader:
    /// the router drops the garbage line and resumes parsing at the next
    /// newline, so a later valid response still reaches its awaiter.
    #[tokio::test]
    async fn reader_recovers_from_malformed_frame_without_trailing_newline() {
        let (host_reader, mut plugin_writer) = duplex(8192);
        let (plugin_reader, host_writer) = duplex(8192);

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("read request");
            let request: RpcRequest = serde_json::from_str(line.trim()).expect("parse request");

            // Garbage with no newline, split across writes to exercise the
            // skip-to-newline path across chunks.
            plugin_writer.write_all(b"not json at all").await.expect("write garbage");
            plugin_writer.flush().await.expect("flush");
            tokio::time::sleep(Duration::from_millis(20)).await;
            plugin_writer.write_all(b" still not json").await.expect("write more garbage");
            plugin_writer.flush().await.expect("flush");
            tokio::time::sleep(Duration::from_millis(20)).await;

            let response = RpcResponse::ok(request.id, serde_json::json!({ "ok": true }));
            let mut encoded = serde_json::to_string(&response).expect("encode");
            encoded = format!("\n{encoded}\n");
            plugin_writer.write_all(encoded.as_bytes()).await.expect("write response");
            plugin_writer.flush().await.expect("flush");
        });

        let host = PluginHost::from_streams("test", host_reader, host_writer);
        let result = host
            .request_typed_with_timeout("$/ping", None, Duration::from_secs(5))
            .await
            .expect("valid response after garbage line must still resolve");
        assert_eq!(result.get("ok"), Some(&serde_json::Value::Bool(true)));
    }

    /// An endless incomplete frame (e.g. a never-terminated JSON string) must
    /// not buffer without bound: past the cap the router tears down and every
    /// pending awaiter observes `ConnectionLost`.
    #[tokio::test]
    async fn reader_tears_down_when_frame_buffer_exceeds_cap() {
        let (host_reader, mut plugin_writer) = duplex(64 * 1024);
        let (plugin_reader, host_writer) = duplex(8192);

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_reader);
            let mut line = String::new();
            let _ = reader.read_line(&mut line).await;
            // Open a JSON string and never close it: the streaming parser
            // reports EOF-pending forever, so the buffer just grows.
            let _ = plugin_writer.write_all(b"\"").await;
            let chunk = vec![b'x'; 16 * 1024];
            for _ in 0..16 {
                if plugin_writer.write_all(&chunk).await.is_err() {
                    return;
                }
            }
        });

        // Small test-only cap (128 KiB) so the overflow path is reachable
        // without pushing the production 8 MiB through a duplex in debug mode.
        let host = PluginHost::from_streams_with_reader_buffer_cap("test", host_reader, host_writer, 128 * 1024);
        let err = host
            .request_typed_with_timeout("$/ping", None, Duration::from_secs(30))
            .await
            .expect_err("request must fail once the buffer cap trips");
        assert!(matches!(err, HostError::ConnectionLost), "expected ConnectionLost, got: {err:?}");
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
    /// process-wide installed factory — or the `subject_router` lazy-spawn
    /// tests, which also spawn real plugin children that draw slots from any
    /// installed factory.
    fn slot_factory_lock() -> &'static std::sync::Mutex<()> {
        &crate::TEST_SLOT_FACTORY_GUARD
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

    /// Provider used by the secret-injection tests. Returns whatever map
    /// the test seeds it with.
    #[derive(Debug)]
    struct FixedSnapshotProvider(std::collections::BTreeMap<String, String>);

    impl SecretSnapshotProvider for FixedSnapshotProvider {
        fn snapshot(&self) -> std::collections::BTreeMap<String, String> {
            self.0.clone()
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: serializes process-wide secret provider across spawn
    async fn secret_injection_forwards_keychain_entries_declared_in_manifest() {
        let _guard = slot_factory_lock().lock().unwrap_or_else(|p| p.into_inner());
        let mut snap = std::collections::BTreeMap::new();
        snap.insert("ANIMUS_SECRET_INJECTED".to_string(), "from-keychain".to_string());
        install_secret_snapshot_provider_for_test(Arc::new(FixedSnapshotProvider(snap)));

        let dir = tempfile::tempdir().unwrap();
        let plugin = write_env_dump_plugin(dir.path());
        let env_out = dir.path().join("env.out");

        let manifest_env = vec![EnvRequirement {
            name: "ANIMUS_SECRET_INJECTED".to_string(),
            description: None,
            sensitive: true,
            required: true,
        }];
        let opts =
            PluginSpawnOptions::for_manifest("env-dump-plugin", &manifest_env, std::iter::empty::<String>(), None);

        let host = PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], opts).await.expect("spawn");
        let _ = host.shutdown().await;
        clear_secret_snapshot_provider_for_test();

        let env = read_env_dump(&env_out);
        assert_eq!(env.get("ANIMUS_SECRET_INJECTED").map(String::as_str), Some("from-keychain"));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: serializes process-wide secret provider across spawn
    async fn secret_injection_skips_entries_not_declared_in_manifest() {
        let _guard = slot_factory_lock().lock().unwrap_or_else(|p| p.into_inner());
        let mut snap = std::collections::BTreeMap::new();
        snap.insert("ANIMUS_SECRET_UNDECLARED".to_string(), "from-keychain".to_string());
        install_secret_snapshot_provider_for_test(Arc::new(FixedSnapshotProvider(snap)));

        let dir = tempfile::tempdir().unwrap();
        let plugin = write_env_dump_plugin(dir.path());
        let env_out = dir.path().join("env.out");

        // Empty manifest -> empty allowlist -> the keychain entry must
        // NOT be injected (preserves the env trust boundary).
        let host = PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], PluginSpawnOptions::default())
            .await
            .expect("spawn");
        let _ = host.shutdown().await;
        clear_secret_snapshot_provider_for_test();

        let env = read_env_dump(&env_out);
        assert!(
            !env.contains_key("ANIMUS_SECRET_UNDECLARED"),
            "undeclared keychain entry must not leak into a plugin with no manifest allowlist; saw env={env:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: serializes process-wide secret provider across spawn
    async fn secret_injection_yields_to_parent_env_on_collision() {
        let _guard = slot_factory_lock().lock().unwrap_or_else(|p| p.into_inner());
        let mut snap = std::collections::BTreeMap::new();
        snap.insert("ANIMUS_SECRET_COLLIDE".to_string(), "from-keychain".to_string());
        install_secret_snapshot_provider_for_test(Arc::new(FixedSnapshotProvider(snap)));

        std::env::set_var("ANIMUS_SECRET_COLLIDE", "from-parent-env");
        let manifest_env = vec![EnvRequirement {
            name: "ANIMUS_SECRET_COLLIDE".to_string(),
            description: None,
            sensitive: true,
            required: true,
        }];
        let opts =
            PluginSpawnOptions::for_manifest("env-dump-plugin", &manifest_env, std::iter::empty::<String>(), None);

        let dir = tempfile::tempdir().unwrap();
        let plugin = write_env_dump_plugin(dir.path());
        let env_out = dir.path().join("env.out");

        let host = PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], opts).await.expect("spawn");
        let _ = host.shutdown().await;
        std::env::remove_var("ANIMUS_SECRET_COLLIDE");
        clear_secret_snapshot_provider_for_test();

        let env = read_env_dump(&env_out);
        assert_eq!(
            env.get("ANIMUS_SECRET_COLLIDE").map(String::as_str),
            Some("from-parent-env"),
            "parent env must override keychain on collision"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: serializes process-wide secret provider across spawn
    async fn secret_injection_respects_cumulative_byte_cap() {
        let _guard = slot_factory_lock().lock().unwrap_or_else(|p| p.into_inner());
        let big = "x".repeat(MAX_INJECTED_SECRET_BYTES + 1);
        let mut snap = std::collections::BTreeMap::new();
        snap.insert("ANIMUS_SECRET_TINY".to_string(), "ok".to_string());
        snap.insert("ANIMUS_SECRET_HUGE".to_string(), big);
        install_secret_snapshot_provider_for_test(Arc::new(FixedSnapshotProvider(snap)));

        let dir = tempfile::tempdir().unwrap();
        let plugin = write_env_dump_plugin(dir.path());
        let env_out = dir.path().join("env.out");

        let manifest_env = vec![
            EnvRequirement {
                name: "ANIMUS_SECRET_TINY".to_string(),
                description: None,
                sensitive: true,
                required: false,
            },
            EnvRequirement {
                name: "ANIMUS_SECRET_HUGE".to_string(),
                description: None,
                sensitive: true,
                required: false,
            },
        ];
        let opts =
            PluginSpawnOptions::for_manifest("env-dump-plugin", &manifest_env, std::iter::empty::<String>(), None);

        let host = PluginHost::spawn_with_options(&plugin, &[env_out.to_str().unwrap()], opts).await.expect("spawn");
        let _ = host.shutdown().await;
        clear_secret_snapshot_provider_for_test();

        let env = read_env_dump(&env_out);
        assert_eq!(env.get("ANIMUS_SECRET_TINY").map(String::as_str), Some("ok"));
        assert!(
            !env.contains_key("ANIMUS_SECRET_HUGE"),
            "the > {MAX_INJECTED_SECRET_BYTES}-byte entry must be skipped"
        );
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
