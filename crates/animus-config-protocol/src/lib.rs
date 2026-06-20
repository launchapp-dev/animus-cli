//! Wire types for the Animus `config_source` plugin role.
//!
//! Today the kernel hardcodes exactly one way to learn what workflows, agents,
//! phases, schedules, and triggers exist: it scans `.animus/workflows.yaml` +
//! `.animus/workflows/*.yaml`, interpolates env/secrets, and parses the YAML
//! into a canonical config model. This crate defines the language-neutral wire
//! contract that makes the *sourcing* of that model a plugin role, so the
//! kernel can become source-agnostic (YAML on disk, Postgres metadata tables,
//! a remote API, ...).
//!
//! # The boundary: plugins source, the kernel compiles
//!
//! A `config_source` plugin parses / reads / fetches config from somewhere and
//! returns ONE normalized canonical model (today's parsed `WorkflowConfig`,
//! schema `animus.workflow-config.v2`). The kernel keeps the heavy compiler —
//! pack-overlay merge, agent-runtime derivation, state-machine compilation,
//! validation, and caching. The plugin boundary sits at the *parsed canonical
//! model*, exactly where the in-tree YAML parser hands off to
//! `merge_yaml_into_config` today. The compiler is NOT duplicated per plugin.
//!
//! See `docs/RFC-v0.6-config-source-plugin.md` (LaunchApp portal) for the full
//! design and the open questions this crate's shape leaves for the kernel
//! rewiring.
//!
//! # The canonical model is carried as opaque JSON (for now)
//!
//! [`ConfigModel::config`] is a [`serde_json::Value`] whose shape is the
//! kernel's `WorkflowConfig` (schema `animus.workflow-config.v2`). It is
//! carried as JSON rather than as a strongly typed mirror because:
//!
//! 1. RFC open question #1 is unresolved — whether to expose `WorkflowConfig`
//!    verbatim or define a slimmer authored-subset `ConfigModel`. Carrying it
//!    as opaque JSON keyed by [`ConfigModel::schema`] / [`ConfigModel::version`]
//!    lets the contract stabilize without prematurely forking the type.
//! 2. The kernel's `WorkflowConfig` and its ~40 nested types do NOT derive
//!    `schemars::JsonSchema` today. Mirroring them here (or adding `JsonSchema`
//!    across the kernel tree) is invasive kernel work, deliberately out of
//!    scope for this additive protocol-crate slice.
//!
//! The kernel deserializes [`ConfigModel::config`] into its internal
//! `WorkflowConfig` after a [`ConfigModel::schema`] / [`ConfigModel::version`]
//! compatibility check. The exported JSON Schema therefore precisely describes
//! the protocol envelope (requests, responses, notifications, cache token) and
//! describes the embedded config as an opaque object tagged by schema id —
//! which is exactly the versioning contract the kernel enforces.
//!
//! # Method family
//!
//! Modeled on the `subject/*` family in `animus-subject-protocol`:
//!
//! - [`METHOD_CONFIG_LOAD`] (`config/load`) — required. Returns the full
//!   canonical [`ConfigModel`] plus a [`CacheToken`] for the current
//!   `repo-scope` / project root.
//! - [`NOTIFICATION_CONFIG_CHANGED`] (`config/changed`) — optional, gated on
//!   the [`CAPABILITY_CONFIG_WATCH`] flag. Server→host notification that the
//!   canonical model may have changed; the host responds by re-issuing
//!   `config/load` and recompiling. Mirrors `subject/changed`.
//! - [`METHOD_CONFIG_VALIDATE`] (`config/validate`) — optional. A plugin-side
//!   syntactic pre-check (e.g. YAML diagnostics with file/line). The kernel
//!   still runs the authoritative validator; this is for better error locality
//!   at the source.

// NOTE: `missing_docs` is intentionally NOT warned crate-wide. The wire types
// below are fully documented, but the canonical config-model TYPES and YAML
// PARSER that now live in this crate (moved from the kernel) carry their docs
// at the item level where present and are otherwise self-describing; warning on
// every nested config field would be noise.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// =====================================================================
// Canonical config model TYPES + standardized YAML PARSER
//
// `animus-config-protocol` is the canonical home for BOTH the `WorkflowConfig`
// types and the standardized YAML parser. The kernel (`orchestrator-config`)
// and the `animus-config-yaml` plugin are both implementations that depend on
// this crate. The kernel re-exports these so its ~hundreds of internal
// `crate::workflow_config::types::*` reference sites keep compiling; the plugin
// depends ONLY on this crate (plus the plugin protocol/runtime).
// =====================================================================

pub mod agent_types;
pub mod builtins;
pub mod env_interp;
pub mod overlay;
pub mod parse;
pub mod workflow_types;
pub mod yaml_diagnostic;
pub mod yaml_parser;
pub mod yaml_types;

/// `crate::types::*` compatibility alias for the two `protocol::orchestrator`
/// enums the moved config types reference (`PhaseEvidenceKind`,
/// `WorkflowDecisionRisk`). Mirrors the kernel's old `orchestrator_config::types`
/// module so moved code paths keep resolving `crate::types::*`.
pub mod types {
    pub use protocol::orchestrator::{PhaseEvidenceKind, WorkflowDecisionRisk};
}

/// `crate::workflow::*` compatibility alias carrying the checkpoint-retention
/// default the moved `WorkflowConfig` types reference. Mirrors the kernel's
/// `orchestrator_config::workflow` module.
pub mod workflow {
    /// Default number of checkpoints retained per phase. Mirrors
    /// `orchestrator_config::DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE`.
    pub const DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE: usize = 3;
}

/// Compatibility alias so moved code that referenced
/// `crate::workflow_config::WorktreeConfig` keeps resolving inside this crate.
pub mod workflow_config {
    pub use crate::workflow_types::*;
}

/// Re-export of `crate::PhaseExecutionDefinition` (the kernel referenced this
/// type as `crate::PhaseExecutionDefinition` via its top-level `pub use
/// agent_runtime_config::*`).
pub use agent_types::PhaseExecutionDefinition;

// =====================================================================
// Plugin kind
// =====================================================================

/// Plugin kind for `config_source` plugins.
///
/// Config source plugins implement the `config/*` method family —
/// [`METHOD_CONFIG_LOAD`] (required), [`NOTIFICATION_CONFIG_CHANGED`]
/// (optional, gated on [`CAPABILITY_CONFIG_WATCH`]), and
/// [`METHOD_CONFIG_VALIDATE`] (optional). They source the canonical config
/// model; the kernel compiles it.
pub const PLUGIN_KIND_CONFIG_SOURCE: &str = "config_source";

// =====================================================================
// Method-name constants (the JSON-RPC wire methods)
// =====================================================================

/// `config/load` — return the full canonical [`ConfigModel`] for the current
/// `repo-scope` / project root. The only *required* method of the role.
pub const METHOD_CONFIG_LOAD: &str = "config/load";

/// `config/validate` — optional plugin-side syntactic pre-check returning
/// structured diagnostics. Backends that do not support it MUST respond with
/// `animus_plugin_protocol::error_codes::METHOD_NOT_SUPPORTED`.
pub const METHOD_CONFIG_VALIDATE: &str = "config/validate";

/// `config/changed` — notification emitted by a config source that advertises
/// [`CAPABILITY_CONFIG_WATCH`] when the canonical model may have changed (file
/// mtime bump, Postgres `LISTEN/NOTIFY`, API webhook/poll). The host responds
/// by re-issuing `config/load` and recompiling.
pub const NOTIFICATION_CONFIG_CHANGED: &str = "config/changed";

// =====================================================================
// Capabilities
// =====================================================================

/// Capability flag a config source advertises when it can observe changes to
/// its underlying source and emit [`NOTIFICATION_CONFIG_CHANGED`].
///
/// A plugin that lacks this capability degrades to the host's interval /
/// manual reload path with no behavioral regression — mirroring the
/// `subject/watch` `MethodNotSupported` fallback contract.
pub const CAPABILITY_CONFIG_WATCH: &str = "config_watch";

// =====================================================================
// Canonical model schema identity
// =====================================================================

/// Schema id of the canonical config model carried in [`ConfigModel::config`].
///
/// This is the kernel's `WorkflowConfig` schema (`animus.workflow-config.v2`).
/// The kernel rejects a [`ConfigModel`] whose [`ConfigModel::schema`] it does
/// not recognize. Kept as a single source of truth so plugin authors and the
/// kernel agree on the contract surface.
pub const CONFIG_MODEL_SCHEMA_ID: &str = "animus.workflow-config.v2";

/// Current version of the canonical config model wire contract.
///
/// Tracks `WorkflowConfig::version` (the `version: u32` field on the kernel
/// struct). Carried in [`ConfigModel::version`] so the kernel can refuse a
/// model produced against a future, incompatible schema.
pub const CONFIG_MODEL_VERSION: u32 = 2;

// =====================================================================
// Canonical model envelope
// =====================================================================

/// The canonical config model a `config_source` returns from `config/load`.
///
/// This is the normalized, *pre-derivation* shape — the parsed canonical model
/// before the kernel derives `agent-runtime-config.v2` / `state-machines.v1`
/// or merges pack overlays. It is the superset of what the kernel's
/// `WorkflowConfig` already carries: `workflows`, `phase_catalog`,
/// `phase_definitions`, `agent_profiles`, `agent_channels`, `mcp_servers`,
/// `phase_mcp_bindings`, `tools` / `tools_allowlist`, `schedules`, `triggers`,
/// `daemon`, `secrets`, `default_workflow_ref`.
///
/// The [`Self::config`] payload is carried as opaque JSON keyed by
/// [`Self::schema`] / [`Self::version`] (see the crate docs for why). The
/// kernel deserializes it into its internal `WorkflowConfig` after a schema
/// compatibility check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigModel {
    /// Schema id of the embedded [`Self::config`] payload. The kernel rejects
    /// a value it does not recognize. Expected to equal
    /// [`CONFIG_MODEL_SCHEMA_ID`].
    pub schema: String,

    /// Version of the embedded config model. Expected to equal
    /// [`CONFIG_MODEL_VERSION`]; the kernel refuses a higher, incompatible
    /// version.
    pub version: u32,

    /// The canonical config model, shaped as the kernel's `WorkflowConfig`
    /// (schema `animus.workflow-config.v2`). Carried as opaque JSON — see the
    /// crate docs — and deserialized kernel-side after the schema check.
    ///
    /// The exported JSON Schema describes this as an arbitrary JSON value
    /// (schemars represents `serde_json::Value` as the permissive `true`
    /// schema); the real shape is pinned by [`Self::schema`] /
    /// [`Self::version`], which is the versioning contract the kernel enforces.
    pub config: Value,
}

impl ConfigModel {
    /// Construct a [`ConfigModel`] tagged with the current schema id and
    /// version, wrapping an already-serialized canonical config payload.
    pub fn new(config: Value) -> Self {
        Self { schema: CONFIG_MODEL_SCHEMA_ID.to_string(), version: CONFIG_MODEL_VERSION, config }
    }

    /// True when the carried [`Self::schema`] / [`Self::version`] match the
    /// contract this crate version implements. The kernel uses this as the
    /// admit check before deserializing [`Self::config`] into `WorkflowConfig`.
    pub fn is_compatible(&self) -> bool {
        self.schema == CONFIG_MODEL_SCHEMA_ID && self.version <= CONFIG_MODEL_VERSION
    }
}

// =====================================================================
// Cache token
// =====================================================================

/// Opaque cache key a config source returns so the kernel cache stays correct
/// for non-file sources.
///
/// The in-tree YAML cache keys on file content + mtime. For Postgres / API
/// sources the plugin instead returns a [`CacheToken`]: an opaque version
/// string (a Postgres `max(updated_at)`, an ETag, a content hash, ...) plus an
/// [`Self::external_inputs`] flag that reproduces today's
/// `sources_have_external_inputs` cache-bypass hazard (inputs like `${VAR}` /
/// `${secret.X}` / `system_prompt_file:` that change without mutating the
/// source bytes). When [`Self::external_inputs`] is true the kernel bypasses
/// its disk cache for that load, matching current YAML behavior.
///
/// RFC open question #5 (cache-token contract) is the reason this is a small
/// structured shape rather than a bare opaque string: it must cover the
/// "external inputs changed but source bytes didn't" case explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CacheToken {
    /// Opaque version identifier for the loaded model. The kernel keys its
    /// existing workflow cache on this value instead of file bytes. Two loads
    /// that produce the same canonical model SHOULD return the same version.
    pub version: String,

    /// When true, the model embeds inputs not captured by [`Self::version`]
    /// (env-var / secret interpolation, externally referenced prompt files,
    /// ...) that can change without changing the version. The kernel bypasses
    /// its disk cache for this load — preserving today's
    /// `sources_have_external_inputs` semantics.
    #[serde(default)]
    pub external_inputs: bool,
}

impl CacheToken {
    /// A token with the given version string and no external inputs.
    pub fn version(version: impl Into<String>) -> Self {
        Self { version: version.into(), external_inputs: false }
    }

    /// A token that forces the kernel to bypass its disk cache for this load.
    pub fn external(version: impl Into<String>) -> Self {
        Self { version: version.into(), external_inputs: true }
    }
}

// =====================================================================
// config/load
// =====================================================================

/// Parameters for [`METHOD_CONFIG_LOAD`].
///
/// Carries the project context the in-tree YAML scan reads from today: the
/// project root (where `.animus/` lives) and the `repo-scope` the kernel uses
/// to locate scoped runtime state and keychain entries. A Postgres/API source
/// uses `repo_scope` to select the right rows; the YAML source uses
/// `project_root` to find the overlay files.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ConfigLoadRequest {
    /// Absolute path to the project root (the directory whose `.animus/`
    /// carries the config). The YAML source scans `.animus/workflows.yaml` +
    /// `.animus/workflows/*.yaml` under this root.
    pub project_root: String,

    /// The `repo-scope` identifier the kernel computed for this project. Used
    /// by non-file sources to scope their query and by the host secret path to
    /// resolve scoped credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_scope: Option<String>,
}

/// Response for [`METHOD_CONFIG_LOAD`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigLoadResponse {
    /// The canonical config model the kernel will compile.
    pub config: ConfigModel,

    /// Cache key for this load. The kernel keys its workflow cache on
    /// [`CacheToken::version`] and bypasses the cache when
    /// [`CacheToken::external_inputs`] is set.
    pub cache_token: CacheToken,
}

// =====================================================================
// config/changed
// =====================================================================

/// Notification payload for [`NOTIFICATION_CONFIG_CHANGED`].
///
/// Emitted by a config source advertising [`CAPABILITY_CONFIG_WATCH`] when the
/// canonical model may have changed. The host reacts by re-issuing
/// [`METHOD_CONFIG_LOAD`] and recompiling. Modeled on `subject/changed`.
///
/// The payload is intentionally minimal: it signals *that* a change occurred,
/// not the new model itself, so the host always recompiles through one code
/// path (`config/load` → compile) regardless of source. An optional
/// [`Self::version`] lets the host short-circuit a reload when the advertised
/// version matches what it already compiled.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ConfigChangedEvent {
    /// Opaque new version, if the source knows it. When present and equal to
    /// the host's last-compiled [`CacheToken::version`], the host MAY skip the
    /// reload. When absent, the host always reloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

// =====================================================================
// config/validate (optional)
// =====================================================================

/// Parameters for [`METHOD_CONFIG_VALIDATE`].
///
/// Identical context to [`ConfigLoadRequest`]: a plugin-side syntactic
/// pre-check runs against the same project scope it would `config/load` from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ConfigValidateRequest {
    /// Absolute path to the project root (see [`ConfigLoadRequest::project_root`]).
    pub project_root: String,

    /// The `repo-scope` identifier (see [`ConfigLoadRequest::repo_scope`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_scope: Option<String>,
}

/// Severity of a [`ConfigDiagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// A fatal problem that prevents producing a canonical model.
    Error,
    /// A non-fatal advisory (e.g. an unresolved skill reference).
    Warning,
}

/// A single structured diagnostic from [`METHOD_CONFIG_VALIDATE`].
///
/// Preserves the error locality of today's `yaml_diagnostic.rs` (file path +
/// line/column) so source-level mistakes surface where the author can fix
/// them, even though the kernel still runs the authoritative validator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigDiagnostic {
    /// How serious the diagnostic is.
    pub severity: DiagnosticSeverity,

    /// Human-readable message.
    pub message: String,

    /// Source file the diagnostic refers to, if applicable (e.g. a YAML
    /// overlay path). Absent for non-file sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// 1-based line number within [`Self::file`], if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,

    /// 1-based column number within [`Self::file`], if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// Response for [`METHOD_CONFIG_VALIDATE`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ConfigValidateResponse {
    /// Structured diagnostics. An empty list (and no [`DiagnosticSeverity::Error`]
    /// entries) means the source pre-check passed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ConfigDiagnostic>,
}

impl ConfigValidateResponse {
    /// True when no [`DiagnosticSeverity::Error`] diagnostics are present.
    pub fn is_ok(&self) -> bool {
        !self.diagnostics.iter().any(|d| d.severity == DiagnosticSeverity::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_model_compat_check() {
        let model = ConfigModel::new(serde_json::json!({ "schema": CONFIG_MODEL_SCHEMA_ID }));
        assert!(model.is_compatible());

        let future = ConfigModel { schema: CONFIG_MODEL_SCHEMA_ID.to_string(), version: 99, config: Value::Null };
        assert!(!future.is_compatible());

        let foreign = ConfigModel { schema: "something.else".to_string(), version: 2, config: Value::Null };
        assert!(!foreign.is_compatible());
    }

    #[test]
    fn load_response_round_trips() {
        let resp = ConfigLoadResponse {
            config: ConfigModel::new(serde_json::json!({ "workflows": [] })),
            cache_token: CacheToken::version("etag-123"),
        };
        let v = serde_json::to_value(&resp).expect("serialize");
        let back: ConfigLoadResponse = serde_json::from_value(v).expect("deserialize");
        assert_eq!(resp, back);
        assert!(!back.cache_token.external_inputs);
    }

    #[test]
    fn external_inputs_token_defaults_false_when_absent() {
        let token: CacheToken = serde_json::from_value(serde_json::json!({ "version": "x" })).expect("deserialize");
        assert!(!token.external_inputs);
    }

    #[test]
    fn validate_response_ok_when_only_warnings() {
        let resp = ConfigValidateResponse {
            diagnostics: vec![ConfigDiagnostic {
                severity: DiagnosticSeverity::Warning,
                message: "unresolved skill".to_string(),
                file: None,
                line: None,
                column: None,
            }],
        };
        assert!(resp.is_ok());
    }
}
