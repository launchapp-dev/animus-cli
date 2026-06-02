//! Wire types for the Animus stdio plugin protocol.
//!
//! Every Animus plugin — providers (LLM CLIs), subject backends (Linear, Jira,
//! GitHub Issues, ...), trigger backends (Slack, webhooks, ...), and any future
//! plugin kind — speaks the same JSON-RPC 2.0 protocol over stdin/stdout. The
//! canonical framing is one JSON value per line (NDJSON), and current host-side
//! readers also tolerate pretty-printed multi-line frames from plugins for
//! forward compatibility. This crate defines the language-neutral wire shapes
//! the host and plugin agree on: the request/response envelope, error codes,
//! the `initialize`/`initialized`/`health/check` lifecycle, plugin kinds, and
//! the capability declarations exchanged during the handshake.
//!
//! Plugin compatibility is intentionally defined by these wire shapes rather
//! than by Rust crate linkage. A Python or TypeScript plugin that emits the
//! same JSON over stdio is just as compatible as a Rust plugin that links this
//! crate.
//!
//! # See also
//!
//! - The companion `spec.md` in this repository — the language-agnostic
//!   protocol specification.
//! - [`animus-subject-protocol`] for the subject-backend trait + schema layered
//!   on top of these wire types.
//! - [`animus-provider-protocol`] for the provider-backend trait layered on top
//!   of these wire types.
//! - [`animus-plugin-runtime`] for the shared stdio loop that consumes these
//!   types and dispatches into trait implementations.

#![warn(missing_docs)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current protocol version implemented by this crate.
///
/// Plugins declare the version they were built against in
/// [`InitializeResult::protocol_version`] during the handshake, and the host
/// declares its own in [`InitializeParams::protocol_version`]. A plugin and
/// host with the same major version are compatible. See `spec.md` for the
/// full versioning policy.
pub const PROTOCOL_VERSION: &str = "1.0.0";

/// Plugin kind for LLM provider plugins (Claude, Codex, Gemini, OpenAI-compat,
/// on-prem, ...).
///
/// Provider plugins implement `agent/run`, `agent/resume`, and `agent/cancel`.
pub const PLUGIN_KIND_PROVIDER: &str = "provider";

/// Plugin kind for subject backend plugins (Linear, Jira, GitHub Issues,
/// Notion, Asana, native task store, ...).
///
/// Subject backends implement the `subject/*` method family — `subject/list`,
/// `subject/get`, `subject/update`, optional `subject/watch`, and
/// `subject/schema`.
pub const PLUGIN_KIND_SUBJECT_BACKEND: &str = "subject_backend";

/// Plugin kind for task backend plugins.
///
/// Reserved for plugins that own the task store itself (legacy alias used by
/// some in-tree probes). New plugins should prefer
/// [`PLUGIN_KIND_SUBJECT_BACKEND`].
pub const PLUGIN_KIND_TASK_BACKEND: &str = "task_backend";

/// Plugin kind for trigger backend plugins (Slack, generic webhooks, file
/// watchers, ...).
///
/// Reserved for v0.4.x. The trigger protocol is not finalized in v0.4.0.
pub const PLUGIN_KIND_TRIGGER_BACKEND: &str = "trigger_backend";

/// Plugin kind for log storage backend plugins (file-backed, hosted SaaS,
/// OpenTelemetry exporters, ...).
///
/// Log storage backends receive `log/entry` notifications from the daemon
/// and any other supervised plugin and own persisting / forwarding them.
/// When no plugin is installed the daemon falls back to the in-tree
/// `orchestrator-logging::Logger` which writes structured events to
/// `events.jsonl`.
pub const PLUGIN_KIND_LOG_STORAGE_BACKEND: &str = "log_storage_backend";

/// Plugin kind for transport backend plugins (HTTP, GraphQL, gRPC, ...).
///
/// Transport backends expose the daemon's control surface over a network
/// protocol so out-of-tree web UIs and SDKs can talk to it. Discovered and
/// spawned by `animus web serve` alongside any installed `web_ui` plugin.
pub const PLUGIN_KIND_TRANSPORT_BACKEND: &str = "transport_backend";

/// Plugin kind for web UI plugins.
///
/// Web UI plugins ship the assets and entry-point for a browser-facing
/// dashboard. They are spawned together with a matching
/// [`PLUGIN_KIND_TRANSPORT_BACKEND`] by `animus web serve`.
pub const PLUGIN_KIND_WEB_UI: &str = "web_ui";

/// Method name for the log-storage `log/entry` notification.
///
/// Emitted by any supervised plugin to forward a structured log entry to
/// the active log storage backend (plugin or in-tree fallback). The
/// notification payload is JSON-typed to match
/// `orchestrator_logging::LogEntry` so the in-tree fallback can persist the
/// entry verbatim and a plugin backend can choose its own schema mapping.
pub const LOG_STORAGE_METHOD_ENTRY: &str = "log/entry";

/// Method name for the log-storage `log_storage/tail` request.
///
/// Hosts call this against an active log storage backend plugin to fetch
/// a bounded slice of recent entries. Streaming follow-up notifications
/// (when supported by the plugin) carry the original request id per the
/// notification streaming contract documented in `spec.md`.
pub const LOG_STORAGE_METHOD_TAIL: &str = "log_storage/tail";

/// Plugin kind for plugins that don't fit a built-in category.
///
/// Custom plugins still go through the standard
/// `initialize`/`initialized`/`health/check` lifecycle but the host treats
/// their domain methods opaquely. Custom plugins are typically invoked via
/// the `animus.plugin.call` MCP tool.
pub const PLUGIN_KIND_CUSTOM: &str = "custom";

/// Strongly typed enumeration of plugin roles.
///
/// The set of well-known kinds is captured here so callers can pattern-match
/// instead of comparing magic strings. New plugin roles can be added by the
/// host without breaking older binaries: an unknown wire value parses as
/// [`PluginKind::Other`], and round-trips byte-for-byte through serde.
///
/// The string forms match the `PLUGIN_KIND_*` constants in this module; the
/// constants remain available for code that needs the literal wire form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(from = "String", into = "String")]
#[schemars(
    with = "String",
    description = "Plugin role kind. Wire representation is a snake_case string; unknown values round-trip via Other."
)]
#[non_exhaustive]
pub enum PluginKind {
    /// LLM provider plugin. See [`PLUGIN_KIND_PROVIDER`].
    Provider,
    /// Subject backend plugin. See [`PLUGIN_KIND_SUBJECT_BACKEND`].
    SubjectBackend,
    /// Legacy task backend alias. See [`PLUGIN_KIND_TASK_BACKEND`].
    TaskBackend,
    /// Trigger backend plugin. See [`PLUGIN_KIND_TRIGGER_BACKEND`].
    TriggerBackend,
    /// Log storage backend plugin. See [`PLUGIN_KIND_LOG_STORAGE_BACKEND`].
    LogStorageBackend,
    /// Transport backend plugin. See [`PLUGIN_KIND_TRANSPORT_BACKEND`].
    TransportBackend,
    /// Web UI plugin. See [`PLUGIN_KIND_WEB_UI`].
    WebUi,
    /// Generic custom plugin. See [`PLUGIN_KIND_CUSTOM`].
    Custom,
    /// Any kind not understood by this crate version. Preserves the wire
    /// string so unknown roles round-trip and so hosts that recognize the
    /// role can still dispatch on the string.
    Other(String),
}

impl PluginKind {
    /// Return the canonical wire-string form of this kind.
    pub fn as_str(&self) -> &str {
        match self {
            PluginKind::Provider => PLUGIN_KIND_PROVIDER,
            PluginKind::SubjectBackend => PLUGIN_KIND_SUBJECT_BACKEND,
            PluginKind::TaskBackend => PLUGIN_KIND_TASK_BACKEND,
            PluginKind::TriggerBackend => PLUGIN_KIND_TRIGGER_BACKEND,
            PluginKind::LogStorageBackend => PLUGIN_KIND_LOG_STORAGE_BACKEND,
            PluginKind::TransportBackend => PLUGIN_KIND_TRANSPORT_BACKEND,
            PluginKind::WebUi => PLUGIN_KIND_WEB_UI,
            PluginKind::Custom => PLUGIN_KIND_CUSTOM,
            PluginKind::Other(value) => value.as_str(),
        }
    }

    /// `true` for variants this crate version recognizes natively.
    ///
    /// Returns `false` only for [`PluginKind::Other`]. Callers can use this
    /// to log a warning when the host is talking to a plugin that uses a
    /// kind the host doesn't model.
    pub fn is_known(&self) -> bool {
        !matches!(self, PluginKind::Other(_))
    }
}

impl std::fmt::Display for PluginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for PluginKind {
    fn from(value: String) -> Self {
        match value.as_str() {
            PLUGIN_KIND_PROVIDER => PluginKind::Provider,
            PLUGIN_KIND_SUBJECT_BACKEND => PluginKind::SubjectBackend,
            PLUGIN_KIND_TASK_BACKEND => PluginKind::TaskBackend,
            PLUGIN_KIND_TRIGGER_BACKEND => PluginKind::TriggerBackend,
            PLUGIN_KIND_LOG_STORAGE_BACKEND => PluginKind::LogStorageBackend,
            PLUGIN_KIND_TRANSPORT_BACKEND => PluginKind::TransportBackend,
            PLUGIN_KIND_WEB_UI => PluginKind::WebUi,
            PLUGIN_KIND_CUSTOM => PluginKind::Custom,
            _ => PluginKind::Other(value),
        }
    }
}

impl From<&str> for PluginKind {
    fn from(value: &str) -> Self {
        PluginKind::from(value.to_string())
    }
}

impl From<PluginKind> for String {
    fn from(kind: PluginKind) -> Self {
        kind.as_str().to_string()
    }
}

impl PartialEq<str> for PluginKind {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for PluginKind {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// Method name for the trigger-backend `trigger/watch` request.
pub const TRIGGER_METHOD_WATCH: &str = "trigger/watch";

/// Method name for the trigger-backend `trigger/event` notification.
pub const TRIGGER_METHOD_EVENT: &str = "trigger/event";

/// Method name for the trigger-backend `trigger/ack` notification.
pub const TRIGGER_METHOD_ACK: &str = "trigger/ack";

/// JSON-RPC 2.0 standard error codes plus Animus-specific extensions.
///
/// The `-32700`..`-32600` range follows the JSON-RPC 2.0 specification. The
/// `-32000`..`-32099` range is reserved by JSON-RPC 2.0 for implementation
/// errors; Animus uses it for protocol-level conditions that the host needs
/// to react to programmatically (e.g. graceful fallback when a plugin doesn't
/// support an optional method).
pub mod error_codes {
    /// Invalid JSON was received by the server.
    pub const PARSE_ERROR: i32 = -32700;
    /// The JSON sent is not a valid request object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist or is not available.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid method parameter(s).
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i32 = -32603;

    /// Animus extension: the plugin received a domain method before
    /// `initialize` completed.
    pub const PLUGIN_NOT_INITIALIZED: i32 = -32000;
    /// Animus extension: the plugin recognizes the method but does not
    /// implement it (e.g. a polling-only subject backend rejecting
    /// `subject/watch`). The host should fall back rather than fail.
    pub const METHOD_NOT_SUPPORTED: i32 = -32001;
    /// Animus extension: the host cancelled an in-flight request.
    pub const REQUEST_CANCELLED: i32 = -32002;
    /// Animus extension: a request timed out before completing.
    pub const TIMEOUT: i32 = -32003;
}

/// A JSON-RPC 2.0 request frame.
///
/// `id` is `Some` for requests that expect a response. Notifications use
/// [`RpcNotification`] instead and have no `id`. `params` is structurally
/// typed via [`Value`] so the runtime can dispatch to method-specific
/// deserializers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcRequest {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Request id. `None` indicates a notification (use [`RpcNotification`]
    /// instead in that case; this field exists to round-trip permissively).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// JSON-RPC method name (e.g. `"initialize"`, `"subject/list"`).
    pub method: String,
    /// Method parameters; structurally validated by the receiving handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl RpcRequest {
    /// Build a request with the given id, method, and optional params.
    pub fn new(id: impl Into<Value>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self { jsonrpc: "2.0".to_string(), id: Some(id.into()), method: method.into(), params }
    }
}

/// A JSON-RPC 2.0 notification frame.
///
/// Notifications are fire-and-forget — they have no `id` and the recipient
/// never replies. Server-streaming results from a single request id (e.g.
/// `subject/changed` watch events) are also delivered as notifications; in
/// that case the original request id is carried inside `params`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcNotification {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// JSON-RPC method name.
    pub method: String,
    /// Notification parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl RpcNotification {
    /// Build a notification with the given method and optional params.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self { jsonrpc: "2.0".to_string(), method: method.into(), params }
    }
}

/// A JSON-RPC 2.0 response frame.
///
/// Exactly one of `result` or `error` should be set. Use [`RpcResponse::ok`]
/// or [`RpcResponse::err`] to construct correctly-shaped responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcResponse {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Echoes the id of the originating request. `None` only when the request
    /// id could not be determined (e.g. parse error on the request frame).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Successful result. Mutually exclusive with `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error payload. Mutually exclusive with `result`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    /// Build a successful response carrying the given result value.
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self { jsonrpc: "2.0".to_string(), id, result: Some(result), error: None }
    }

    /// Build an error response carrying the given error payload.
    pub fn err(id: Option<Value>, error: RpcError) -> Self {
        Self { jsonrpc: "2.0".to_string(), id, result: None, error: Some(error) }
    }
}

/// JSON-RPC 2.0 error payload.
///
/// `code` is one of the constants in [`error_codes`] or an
/// implementation-specific value in the reserved JSON-RPC range. `data` is
/// optional structured detail that the host can surface in logs or pass back
/// to the originating CLI/MCP caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcError {
    /// Error code; see [`error_codes`].
    pub code: i32,
    /// Short human-readable description.
    pub message: String,
    /// Optional structured detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Identity of the host issuing the `initialize` call.
///
/// Plugins may log this for debugging or vary behavior based on the host
/// version (e.g. enabling features only available in newer hosts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostInfo {
    /// Conventionally `"animus"` for the official Animus daemon.
    pub name: String,
    /// Semver of the host.
    pub version: String,
}

/// Identity of the plugin returned in the `initialize` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginInfo {
    /// Plugin's published name (e.g. `"animus-subject-linear"`).
    pub name: String,
    /// Plugin's semver.
    pub version: String,
    /// One of the `PLUGIN_KIND_*` constants. Prefer
    /// [`PluginInfo::plugin_kind`] to read this as a typed [`PluginKind`].
    pub plugin_kind: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl PluginInfo {
    /// Typed view of [`PluginInfo::plugin_kind`].
    ///
    /// Unknown wire values land in [`PluginKind::Other`] so unrecognized
    /// roles round-trip without loss. Prefer this over comparing the raw
    /// string to the `PLUGIN_KIND_*` constants.
    pub fn kind(&self) -> PluginKind {
        PluginKind::from(self.plugin_kind.as_str())
    }
}

/// Capabilities the host advertises during the handshake.
///
/// Plugins may use these to enable optional features. The host promises to
/// honor any capability it advertises.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HostCapabilities {
    /// Host accepts server-streaming notifications carrying the original
    /// request id.
    #[serde(default)]
    pub streaming: bool,
    /// Host accepts `$/progress` notifications.
    #[serde(default)]
    pub progress: bool,
    /// Host may issue `$/cancelRequest` notifications to cancel in-flight
    /// requests.
    #[serde(default)]
    pub cancellation: bool,
}

/// Capabilities the plugin advertises during the handshake.
///
/// `methods` is the closed set of domain methods the plugin implements; the
/// host uses it to skip calls the plugin would reject anyway. `subject_kinds`
/// and `mcp_tools` are supplemental hints for subject-backend and
/// custom-plugin kinds respectively.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginCapabilities {
    /// Concrete methods the plugin implements (e.g. `["subject/list",
    /// "subject/get", "subject/update"]`).
    #[serde(default)]
    pub methods: Vec<String>,
    /// Plugin emits server-streaming notifications.
    #[serde(default)]
    pub streaming: bool,
    /// Plugin honors `$/progress` notifications.
    #[serde(default)]
    pub progress: bool,
    /// Plugin honors `$/cancelRequest` notifications.
    #[serde(default)]
    pub cancellation: bool,
    /// Optional projection names the plugin can serve (subject backends
    /// only). Hosts may request a projection by name in calls that opt into
    /// projected views.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projections: Vec<String>,
    /// Subject kinds the plugin can produce (subject backends only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject_kinds: Vec<String>,
    /// MCP tools exposed by the plugin (custom plugins only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_tools: Vec<McpTool>,
}

/// Description of an MCP tool exposed by a custom plugin.
///
/// Hosts that bridge MCP can re-expose these tools to MCP clients without
/// the plugin author writing MCP-specific code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpTool {
    /// MCP tool name.
    pub name: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the tool's input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
}

/// Parameters sent from host to plugin in the `initialize` request.
///
/// This is the first request the host sends after the plugin process starts.
/// The plugin should validate `protocol_version` and return an
/// [`InitializeResult`] or an error if the versions are incompatible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InitializeParams {
    /// Protocol version the host speaks. See [`PROTOCOL_VERSION`].
    pub protocol_version: String,
    /// Identity of the host.
    pub host_info: HostInfo,
    /// Capabilities the host promises to honor.
    pub capabilities: HostCapabilities,
}

/// Plugin's response to `initialize`.
///
/// The host inspects `protocol_version` for compatibility and stores
/// `capabilities` for the lifetime of the plugin connection so it can avoid
/// calling unsupported methods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InitializeResult {
    /// Protocol version the plugin speaks. See [`PROTOCOL_VERSION`].
    pub protocol_version: String,
    /// Identity of the plugin.
    pub plugin_info: PluginInfo,
    /// Capabilities the plugin advertises.
    pub capabilities: PluginCapabilities,
}

/// One-shot manifest emitted when a plugin is invoked with `--manifest`.
///
/// This is the discovery surface used by `animus plugin install` and similar
/// tooling that needs to know what a binary is before spawning it as a
/// long-running stdio child. The shape mirrors [`InitializeResult`] but is
/// flat for ease of static parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginManifest {
    /// Plugin name (matches [`PluginInfo::name`]).
    pub name: String,
    /// Plugin semver.
    pub version: String,
    /// One of the `PLUGIN_KIND_*` constants. Prefer
    /// [`PluginManifest::kind`] to read this as a typed [`PluginKind`].
    pub plugin_kind: String,
    /// Human-readable description.
    pub description: String,
    /// Protocol version the plugin was built against.
    pub protocol_version: String,
    /// Methods implemented by the plugin.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Environment variables the plugin needs the host to forward at spawn
    /// time.
    ///
    /// The plugin host clears the daemon's process environment before spawning
    /// a plugin (`env_clear()`) and only forwards a minimal universal shell
    /// allowlist (`PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_ALL`, `RUST_LOG`,
    /// `RUST_BACKTRACE`, `TZ`) plus the variables declared here. Plugins that
    /// need an `OPENAI_API_KEY`, `LINEAR_API_TOKEN`, etc. must list them in
    /// this field; otherwise they will be missing at runtime even though the
    /// daemon's environment had them set.
    ///
    /// Defaults to empty for back-compat: plugins built against earlier
    /// versions of the protocol crate simply opt into zero secrets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_required: Vec<EnvRequirement>,
    /// Author-supplied hint for the size of the host's notification broadcast
    /// channel for this plugin process.
    ///
    /// Plugin authors who know they emit bursts of notifications (e.g. a
    /// chatty streaming `agent/run` that fans out hundreds of `agent/output`
    /// frames before a slow subscriber catches up) can request a larger
    /// channel here. The host picks the channel capacity in priority order:
    ///
    /// 1. This manifest field (when set and non-zero).
    /// 2. `ANIMUS_PLUGIN_BROADCAST_CAPACITY` env override (when set and
    ///    parseable as a non-zero `usize`).
    /// 3. The host's compiled default (currently 256).
    ///
    /// Capacity is fixed for a given plugin process lifetime — the underlying
    /// `tokio::sync::broadcast` channel cannot be resized at runtime. To
    /// change the capacity, restart the plugin process so the host can pick
    /// up the new hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_buffer_size: Option<usize>,
}

impl PluginManifest {
    /// Typed view of [`PluginManifest::plugin_kind`].
    ///
    /// Unknown wire values land in [`PluginKind::Other`] so unrecognized
    /// roles round-trip without loss. Prefer this over comparing the raw
    /// string to the `PLUGIN_KIND_*` constants.
    pub fn kind(&self) -> PluginKind {
        PluginKind::from(self.plugin_kind.as_str())
    }
}

/// One environment variable a plugin asks the host to forward at spawn time.
///
/// The host treats `name` as the source of truth: only matching variables are
/// passed through the `env_clear()` boundary. `description` and `sensitive`
/// are informational hints surfaced in `animus plugin info` and the install
/// flow so operators can decide whether a plugin's secret requirements are
/// reasonable before granting it access.
///
/// When `required` is set, the host emits a warning at spawn time if the
/// variable isn't present in the daemon's own environment. The host never
/// refuses to spawn over a missing required var — that decision belongs to
/// the plugin itself, which sees the missing variable during its own startup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnvRequirement {
    /// Environment variable name (e.g. `"OPENAI_API_KEY"`).
    pub name: String,
    /// Optional human-readable explanation of what the variable is used for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Hint that this variable carries a secret. Informational only — does not
    /// change spawn behavior. Used to drive warnings in install output and
    /// `animus plugin info` listings.
    #[serde(default)]
    pub sensitive: bool,
    /// When `true`, the host emits a warning at spawn time if the variable is
    /// not set in the daemon's environment.
    #[serde(default)]
    pub required: bool,
}

/// Health status emitted by `health/check`.
///
/// Hosts surface this in `animus daemon health` and may use it to gate work
/// (e.g. drain in-flight subjects from a `Degraded` plugin before restart).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// Plugin is fully functional.
    Healthy,
    /// Plugin is operational but in a reduced state (e.g. stale cache,
    /// upstream rate-limited but recovering).
    Degraded,
    /// Plugin is non-functional. The host may restart or quarantine it.
    Unhealthy,
}

/// Response to `health/check`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HealthCheckResult {
    /// Overall status.
    pub status: HealthStatus,
    /// Milliseconds since the plugin process started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_ms: Option<u64>,
    /// Resident-set memory usage in bytes, if cheap to determine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_usage_bytes: Option<u64>,
    /// Most recent error message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Parameters sent from host to plugin in the `trigger/watch` request.
///
/// Trigger backend plugins receive this once during startup. After replying
/// to the request the plugin emits `trigger/event` notifications whenever it
/// observes something the host should react to. The plugin keeps watching
/// until it receives a `shutdown` request or its stdio closes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerWatchParams {
    /// Optional resume cursor from a previous run; semantics are
    /// plugin-defined. Plugins should ignore it if unrecognized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Value>,
    /// Plugin-specific configuration forwarded from project workflow YAML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
}

/// A trigger event emitted by a trigger backend plugin.
///
/// Plugins deliver these as `trigger/event` JSON-RPC notifications. The host
/// routes the event to the matching trigger configuration; what the host
/// does next depends on `action_hint` and `subject_id`:
///
/// - `subject_id` is set → the host resolves the subject (via the configured
///   subject backend) and may kick the subject's assigned workflow.
/// - `action_hint` is `Some(TriggerActionHint::CreateTask)` → the host creates
///   a new task with `payload` as input context.
/// - Otherwise the host enqueues the event against the trigger's
///   `workflow_ref` (if configured) using the existing webhook dispatch path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerEvent {
    /// Unique event id assigned by the plugin. Used by the host to send back
    /// `trigger/ack`. Plugins should make this stable across restarts when
    /// possible so duplicate deliveries can be deduplicated.
    pub event_id: String,
    /// Logical trigger id this event belongs to. Matches the `id` of a
    /// `WorkflowTrigger` in the project's workflow YAML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<String>,
    /// Optional subject the event refers to (e.g. a Linear issue id). When
    /// set, the host may resolve the subject via its configured subject
    /// backend and kick the subject's assigned workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    /// Optional subject kind for `subject_id` (e.g. `"issue"`, `"task"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_kind: Option<String>,
    /// Optional hint for what the host should do. Plugins may omit this and
    /// let the host fall back to the trigger config's `workflow_ref`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_hint: Option<TriggerActionHint>,
    /// Event payload. Forwarded to the spawned workflow as input.
    #[serde(default)]
    pub payload: Value,
}

/// Suggestion from a trigger backend plugin for what the host should do with
/// an incoming event.
///
/// The host is free to ignore the hint when its trigger configuration has a
/// more specific instruction (e.g. an explicit `workflow_ref`). Unknown wire
/// values land in [`TriggerActionHint::Other`] so older hosts can still
/// forward events from newer plugins without crashing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(from = "String", into = "String")]
#[schemars(
    with = "String",
    description = "Trigger action hint. Wire representation is a snake_case string; unknown values round-trip via Other."
)]
#[non_exhaustive]
pub enum TriggerActionHint {
    /// Create a new task with the event payload as initial context.
    CreateTask,
    /// Dispatch the trigger's configured workflow against the event payload.
    RunWorkflow,
    /// Any hint not understood by this crate version. Preserves the wire
    /// string for forwarding.
    Other(String),
}

impl TriggerActionHint {
    /// Canonical wire-string form of this hint.
    pub fn as_str(&self) -> &str {
        match self {
            TriggerActionHint::CreateTask => "create_task",
            TriggerActionHint::RunWorkflow => "run_workflow",
            TriggerActionHint::Other(value) => value.as_str(),
        }
    }

    /// `true` for variants this crate version recognizes natively.
    pub fn is_known(&self) -> bool {
        !matches!(self, TriggerActionHint::Other(_))
    }
}

impl std::fmt::Display for TriggerActionHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for TriggerActionHint {
    fn from(value: String) -> Self {
        match value.as_str() {
            "create_task" => TriggerActionHint::CreateTask,
            "run_workflow" => TriggerActionHint::RunWorkflow,
            _ => TriggerActionHint::Other(value),
        }
    }
}

impl From<&str> for TriggerActionHint {
    fn from(value: &str) -> Self {
        TriggerActionHint::from(value.to_string())
    }
}

impl From<TriggerActionHint> for String {
    fn from(hint: TriggerActionHint) -> Self {
        hint.as_str().to_string()
    }
}

/// Parameters sent from host to plugin in the `trigger/ack` notification.
///
/// The host emits this after it has accepted an event for processing. Plugins
/// use it to persist a cursor or trim a server-side queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerAckParams {
    /// The `event_id` being acknowledged.
    pub event_id: String,
    /// Optional status the host wants to report. See [`TriggerAckStatus`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TriggerAckStatus>,
}

/// Host-reported disposition of a single `trigger/event`.
///
/// Plugins may key on the status to update local state (e.g. trim a queue,
/// advance a cursor only on `Dispatched`). Unknown wire values land in
/// [`TriggerAckStatus::Other`] so newer hosts can introduce additional
/// statuses without breaking older plugins.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(from = "String", into = "String")]
#[schemars(
    with = "String",
    description = "Trigger ack status. Wire representation is a snake_case string; unknown values round-trip via Other."
)]
#[non_exhaustive]
pub enum TriggerAckStatus {
    /// Host accepted the event and started the configured workflow.
    Dispatched,
    /// Host queued the event for later dispatch.
    Queued,
    /// Host did not find a matching trigger configuration for the event.
    Unmatched,
    /// Host intentionally skipped the event (e.g. dedupe or filter rule).
    Skipped,
    /// Host attempted to dispatch the event but the dispatch itself failed.
    Failed,
    /// Host is shutting down and is acknowledging the event without
    /// dispatching it.
    Shutdown,
    /// Any status not understood by this crate version.
    Other(String),
}

impl TriggerAckStatus {
    /// Canonical wire-string form of this status.
    pub fn as_str(&self) -> &str {
        match self {
            TriggerAckStatus::Dispatched => "dispatched",
            TriggerAckStatus::Queued => "queued",
            TriggerAckStatus::Unmatched => "unmatched",
            TriggerAckStatus::Skipped => "skipped",
            TriggerAckStatus::Failed => "failed",
            TriggerAckStatus::Shutdown => "shutdown",
            TriggerAckStatus::Other(value) => value.as_str(),
        }
    }

    /// `true` for variants this crate version recognizes natively.
    pub fn is_known(&self) -> bool {
        !matches!(self, TriggerAckStatus::Other(_))
    }
}

impl std::fmt::Display for TriggerAckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for TriggerAckStatus {
    fn from(value: String) -> Self {
        match value.as_str() {
            "dispatched" => TriggerAckStatus::Dispatched,
            "queued" => TriggerAckStatus::Queued,
            "unmatched" => TriggerAckStatus::Unmatched,
            "skipped" => TriggerAckStatus::Skipped,
            "failed" => TriggerAckStatus::Failed,
            "shutdown" => TriggerAckStatus::Shutdown,
            _ => TriggerAckStatus::Other(value),
        }
    }
}

impl From<&str> for TriggerAckStatus {
    fn from(value: &str) -> Self {
        TriggerAckStatus::from(value.to_string())
    }
}

impl From<TriggerAckStatus> for String {
    fn from(status: TriggerAckStatus) -> Self {
        status.as_str().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_uses_json_rpc_2() {
        let request = RpcRequest::new(1, "initialize", None);
        assert_eq!(request.jsonrpc, "2.0");
        assert_eq!(request.id, Some(serde_json::json!(1)));
        assert_eq!(request.method, "initialize");
    }

    #[test]
    fn response_ok_sets_result_and_clears_error() {
        let response = RpcResponse::ok(Some(serde_json::json!(1)), serde_json::json!({"ok": true}));
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    #[test]
    fn response_err_sets_error_and_clears_result() {
        let response = RpcResponse::err(
            Some(serde_json::json!(1)),
            RpcError { code: error_codes::METHOD_NOT_FOUND, message: "nope".into(), data: None },
        );
        assert!(response.error.is_some());
        assert!(response.result.is_none());
    }

    #[test]
    fn manifest_round_trips_unknown_plugin_kind() {
        let value = serde_json::json!({
            "name": "linear",
            "version": "0.1.0",
            "plugin_kind": "ticket_backend",
            "description": "external tickets",
            "protocol_version": "1.0.0",
            "capabilities": ["ticket/get"]
        });
        let manifest: PluginManifest = serde_json::from_value(value).expect("manifest should parse");
        assert_eq!(manifest.plugin_kind, "ticket_backend");
        assert_eq!(manifest.kind(), PluginKind::Other("ticket_backend".to_string()));
        assert!(!manifest.kind().is_known(), "ticket_backend is not a built-in role");
        assert!(manifest.env_required.is_empty(), "env_required must default to empty for back-compat");
    }

    #[test]
    fn manifest_env_required_round_trips() {
        let value = serde_json::json!({
            "name": "animus-provider-claude",
            "version": "0.1.0",
            "plugin_kind": "provider",
            "description": "Claude provider",
            "protocol_version": "1.0.0",
            "capabilities": ["agent/run"],
            "env_required": [
                { "name": "ANTHROPIC_API_KEY", "description": "Anthropic API token", "sensitive": true, "required": true },
                { "name": "ANTHROPIC_BASE_URL" }
            ]
        });
        let manifest: PluginManifest = serde_json::from_value(value).expect("manifest should parse");
        assert_eq!(manifest.env_required.len(), 2);
        assert_eq!(manifest.env_required[0].name, "ANTHROPIC_API_KEY");
        assert!(manifest.env_required[0].sensitive);
        assert!(manifest.env_required[0].required);
        assert_eq!(manifest.env_required[1].name, "ANTHROPIC_BASE_URL");
        assert!(!manifest.env_required[1].sensitive);
        assert!(!manifest.env_required[1].required);
    }

    #[test]
    fn manifest_serializes_without_env_required_when_empty() {
        let manifest = PluginManifest {
            name: "x".to_string(),
            version: "0.1.0".to_string(),
            plugin_kind: PluginKind::Custom.to_string(),
            description: "x".to_string(),
            protocol_version: "1.0.0".to_string(),
            capabilities: vec![],
            env_required: vec![],
            notification_buffer_size: None,
        };
        let value = serde_json::to_value(&manifest).unwrap();
        assert!(value.get("env_required").is_none(), "empty env_required must not be serialized for back-compat");
        assert!(
            value.get("notification_buffer_size").is_none(),
            "unset notification_buffer_size must not be serialized for back-compat"
        );
        assert_eq!(value.get("plugin_kind"), Some(&serde_json::json!("custom")));
        assert_eq!(manifest.kind(), PluginKind::Custom);
    }

    #[test]
    fn manifest_notification_buffer_size_round_trips() {
        let value = serde_json::json!({
            "name": "animus-provider-chatty",
            "version": "0.1.0",
            "plugin_kind": "provider",
            "description": "Chatty provider",
            "protocol_version": "1.0.0",
            "capabilities": ["agent/run"],
            "notification_buffer_size": 1024
        });
        let manifest: PluginManifest = serde_json::from_value(value).expect("manifest should parse");
        assert_eq!(manifest.notification_buffer_size, Some(1024));
    }

    #[test]
    fn health_status_serializes_snake_case() {
        let v = serde_json::to_value(HealthStatus::Degraded).unwrap();
        assert_eq!(v, serde_json::json!("degraded"));
    }

    #[test]
    fn trigger_event_round_trips_minimum_fields() {
        let event = TriggerEvent {
            event_id: "evt-1".to_string(),
            trigger_id: Some("on-slack-message".to_string()),
            subject_id: None,
            subject_kind: None,
            action_hint: None,
            payload: serde_json::json!({ "text": "hello" }),
        };
        let encoded = serde_json::to_value(&event).unwrap();
        let decoded: TriggerEvent = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn plugin_kind_round_trips_well_known_variants() {
        for (variant, wire) in [
            (PluginKind::Provider, "provider"),
            (PluginKind::SubjectBackend, "subject_backend"),
            (PluginKind::TaskBackend, "task_backend"),
            (PluginKind::TriggerBackend, "trigger_backend"),
            (PluginKind::LogStorageBackend, "log_storage_backend"),
            (PluginKind::TransportBackend, "transport_backend"),
            (PluginKind::WebUi, "web_ui"),
            (PluginKind::Custom, "custom"),
        ] {
            assert!(variant.is_known(), "{variant:?} should be known");
            assert_eq!(variant.as_str(), wire);
            let encoded = serde_json::to_value(&variant).unwrap();
            assert_eq!(encoded, serde_json::Value::String(wire.to_string()));
            let decoded: PluginKind = serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, variant);
        }
    }

    #[test]
    fn plugin_kind_round_trips_unknown_variant_byte_for_byte() {
        let raw = serde_json::json!("ticket_backend");
        let decoded: PluginKind = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(decoded, PluginKind::Other("ticket_backend".to_string()));
        assert!(!decoded.is_known());
        let encoded = serde_json::to_value(&decoded).unwrap();
        assert_eq!(encoded, raw, "unknown plugin_kind must round-trip byte-for-byte");
    }

    #[test]
    fn trigger_action_hint_round_trips_known_and_unknown() {
        let known = TriggerActionHint::CreateTask;
        assert!(known.is_known());
        let encoded = serde_json::to_value(&known).unwrap();
        assert_eq!(encoded, serde_json::json!("create_task"));
        let decoded: TriggerActionHint = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, known);

        let raw = serde_json::json!("publish_release");
        let unknown: TriggerActionHint = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(unknown, TriggerActionHint::Other("publish_release".to_string()));
        assert!(!unknown.is_known());
        let reencoded = serde_json::to_value(&unknown).unwrap();
        assert_eq!(reencoded, raw, "unknown action_hint must round-trip byte-for-byte");
    }

    #[test]
    fn trigger_ack_status_round_trips_all_known_variants() {
        for (status, wire) in [
            (TriggerAckStatus::Dispatched, "dispatched"),
            (TriggerAckStatus::Queued, "queued"),
            (TriggerAckStatus::Unmatched, "unmatched"),
            (TriggerAckStatus::Skipped, "skipped"),
            (TriggerAckStatus::Failed, "failed"),
            (TriggerAckStatus::Shutdown, "shutdown"),
        ] {
            assert!(status.is_known(), "{status:?} should be known");
            let encoded = serde_json::to_value(&status).unwrap();
            assert_eq!(encoded, serde_json::Value::String(wire.to_string()));
            let decoded: TriggerAckStatus = serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, status);
        }
        let raw = serde_json::json!("rejected");
        let unknown: TriggerAckStatus = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(unknown, TriggerAckStatus::Other("rejected".to_string()));
        assert!(!unknown.is_known());
    }

    #[test]
    fn trigger_watch_params_default_is_empty() {
        let params = TriggerWatchParams::default();
        let encoded = serde_json::to_value(&params).unwrap();
        assert_eq!(encoded, serde_json::json!({}));
    }
}
