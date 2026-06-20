use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const AGENT_RUNTIME_CONFIG_SCHEMA_ID: &str = "animus.agent-runtime-config.v2";
pub const AGENT_RUNTIME_CONFIG_VERSION: u32 = 2;
pub const AGENT_RUNTIME_CONFIG_FILE_NAME: &str = "agent-runtime-config.v2.json";
const BUILTIN_AGENT_RUNTIME_CONFIG_JSON: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/agent-runtime-config.v2.json"));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseFieldDefinition {
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "enum", skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<PhaseFieldDefinition>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, PhaseFieldDefinition>,
}

impl PhaseFieldDefinition {
    pub fn has_nested_fields(&self) -> bool {
        !self.fields.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseOutputContract {
    pub kind: String,
    #[serde(default)]
    pub required_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, PhaseFieldDefinition>,
}

impl PhaseOutputContract {
    pub fn requires_field(&self, field: &str) -> bool {
        self.required_fields.iter().any(|candidate| candidate.eq_ignore_ascii_case(field))
            || self.fields.iter().any(|(name, definition)| definition.required && name.eq_ignore_ascii_case(field))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseDecisionContract {
    #[serde(default)]
    pub required_evidence: Vec<crate::types::PhaseEvidenceKind>,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f32,
    #[serde(default = "default_max_risk")]
    pub max_risk: crate::types::WorkflowDecisionRisk,
    #[serde(default = "default_true")]
    pub allow_missing_decision: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_json_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, PhaseFieldDefinition>,
}

pub const DEFAULT_MAX_REWORK_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackoffConfig {
    pub initial_secs: u64,
    #[serde(default = "default_backoff_factor")]
    pub factor: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_secs: Option<u64>,
}

impl BackoffConfig {
    pub fn delay_for_attempt(&self, attempt: u32) -> u64 {
        if attempt == 0 {
            return 0;
        }
        let raw = self.initial_secs as f64 * self.factor.powi(attempt.saturating_sub(1) as i32);
        let clamped = match self.max_secs {
            Some(max) => raw.min(max as f64),
            None => raw,
        };
        clamped as u64
    }
}

fn default_backoff_factor() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseRetryConfig {
    #[serde(default = "default_max_rework_attempts")]
    pub max_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<BackoffConfig>,
}

impl Default for PhaseRetryConfig {
    fn default() -> Self {
        Self { max_attempts: DEFAULT_MAX_REWORK_ATTEMPTS, backoff: None }
    }
}

fn default_max_rework_attempts() -> u32 {
    DEFAULT_MAX_REWORK_ATTEMPTS
}

fn default_min_confidence() -> f32 {
    0.6
}
fn default_max_risk() -> crate::types::WorkflowDecisionRisk {
    crate::types::WorkflowDecisionRisk::Medium
}
fn default_true() -> bool {
    true
}

fn validate_phase_field_definition(path: String, field: &PhaseFieldDefinition) -> Result<()> {
    let field_type = field.field_type.trim();
    if field_type.is_empty() {
        return Err(anyhow!("{path}.type must not be empty"));
    }

    match field_type {
        "string" | "number" | "integer" | "boolean" | "array" | "object" | "null" => {}
        other => {
            return Err(anyhow!(
                "{path}.type must be one of string, number, integer, boolean, array, object, null (got '{}')",
                other
            ));
        }
    }

    if field.enum_values.iter().any(|value| value.trim().is_empty()) {
        return Err(anyhow!("{path}.enum must not contain empty values"));
    }

    if field_type != "array" && field.items.is_some() {
        return Err(anyhow!("{path}.items is only allowed when type='array'"));
    }

    if field_type != "object" && field.has_nested_fields() {
        return Err(anyhow!("{path}.fields is only allowed when type='object'"));
    }

    if let Some(items) = field.items.as_ref() {
        validate_phase_field_definition(format!("{path}.items"), items)?;
    }

    for (nested_name, nested_field) in &field.fields {
        if nested_name.trim().is_empty() {
            return Err(anyhow!("{path}.fields must not contain empty field names"));
        }
        validate_phase_field_definition(format!("{path}.fields['{}']", nested_name), nested_field)?;
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseExecutionMode {
    Agent,
    Command,
    Manual,
}

impl std::fmt::Display for PhaseExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhaseExecutionMode::Agent => write!(f, "agent"),
            PhaseExecutionMode::Command => write!(f, "command"),
            PhaseExecutionMode::Manual => write!(f, "manual"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandCwdMode {
    ProjectRoot,
    TaskRoot,
    Path,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentRuntimeOverrides {
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub tool_profile: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    /// Optional explicit tool overrides for each fallback model.
    /// When non-empty, `fallback_tools[i]` is used for `fallback_models[i]`.
    /// If shorter than `fallback_models`, missing entries are auto-derived.
    #[serde(default)]
    pub fallback_tools: Vec<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Provider permission/approval mode forwarded verbatim to the spawned
    /// CLI (claude `--permission-mode`, codex `-c approval_policy`, gemini
    /// approval mode). Values are provider-specific; see
    /// [`KNOWN_PERMISSION_MODES`].
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub web_search: Option<bool>,
    #[serde(default)]
    pub network_access: Option<bool>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_attempts: Option<usize>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub codex_config_overrides: Vec<String>,
    #[serde(default)]
    pub max_continuations: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentToolPolicy {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl AgentToolPolicy {
    pub fn is_tool_permitted(&self, tool_name: &str) -> bool {
        let allowed = if self.allow.is_empty() { true } else { self.allow.iter().any(|p| glob_match(p, tool_name)) };

        if !allowed {
            return false;
        }

        if self.deny.is_empty() {
            return true;
        }

        !self.deny.iter().any(|p| glob_match(p, tool_name))
    }
}

/// Author-controlled harness-hook configuration for an agent profile.
///
/// Two complementary, deliberately-constrained authoring surfaces:
///
/// * `policy_rules` — guardrail rules ([`protocol::hook_policy::HookPolicyRule`])
///   that merge into the compiled `animus-policy.json` alongside the
///   kernel/tool_policy-derived rules. Pure data evaluated by the kernel's
///   severity-ordered evaluator (`Deny` > `Ask` > `Allow` > `Defer`), so an
///   author rule can only ever *add* restriction — an author `allow` can never
///   weaken a kernel/tool_policy `deny` for the same call (deny always wins
///   regardless of rule source or order). Always safe: the rule never runs
///   shell, it only expresses a decision the kernel evaluator applies.
///
/// * `observers` — additional harness events the author wants routed to the
///   Animus hook spine for observability/automation. **Constrained for
///   safety**: an observer entry can NOT run arbitrary shell. It only names
///   harness events; the kernel generates the command, which is always the
///   validated `animus-hook emit` invocation (the same binary the kernel
///   wires for its own observability hooks). This is option (a) from the
///   trust model — author picks events + a built-in action, never a raw
///   command — so an author-controlled profile can widen *observation* but
///   cannot smuggle an arbitrary payload into the agent's session.
///
/// Both fields default empty, are skipped on serialization when empty, and a
/// config without a `hooks` block loads unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AgentHooksConfig {
    /// Author-supplied guardrail rules merged into the compiled hook policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_rules: Vec<protocol::hook_policy::HookPolicyRule>,
    /// Harness events the author wants additionally routed to `animus-hook`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observers: Vec<AgentHookObserver>,
}

impl AgentHooksConfig {
    pub fn is_empty(&self) -> bool {
        self.policy_rules.is_empty() && self.observers.is_empty()
    }
}

/// A single author-requested observability hook. Constrained to a named
/// built-in action over a set of harness events — never an arbitrary command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentHookObserver {
    /// Harness event names to additionally route through the Animus hook spine
    /// (e.g. `PostToolUse`, `Stop`, `SessionEnd`). Empty is rejected by
    /// config validation — an observer with no events is meaningless.
    #[serde(default)]
    pub events: Vec<String>,
    /// The built-in action. Only [`AgentHookAction::Record`] exists this wave;
    /// it routes the named events to `animus-hook emit` (observability only,
    /// never a gate). The enum exists so future safe built-ins can be added
    /// without ever admitting arbitrary shell.
    #[serde(default)]
    pub action: AgentHookAction,
}

/// Named, kernel-controlled observer action. Deliberately a closed enum: an
/// author can only select a built-in behavior, never supply a command line.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentHookAction {
    /// Route the event to `animus-hook emit` for recording (no policy gate).
    #[default]
    Record,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicyDefault {
    /// Manual mode: escalate to a pending human interaction (the default).
    #[default]
    Ask,
    /// "Dangerous" / approve-everything mode: auto-allow without escalating.
    Allow,
    /// Auto-deny without escalating (fail closed).
    Deny,
    /// Auto-approve mode backed by an LLM: a judge model reads the tool call
    /// (and best-effort run context) and returns allow/deny. Configured via
    /// [`ApprovalPolicy::evaluator_model`] / [`ApprovalPolicy::evaluator_instructions`].
    /// The caller falls back to manual escalation (`Ask`) if the evaluator is
    /// unavailable or errors, so an LLM outage never silently auto-allows.
    Llm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicyDecision {
    Ask,
    Allow,
    Deny,
    /// Defer to the configured LLM evaluator (see [`ApprovalPolicyDefault::Llm`]).
    Evaluate,
}

/// Per-agent policy for `animus.agent.request_approval` escalations.
///
/// Patterns in `auto_allow` / `auto_deny` are matched against the request's
/// `tool_name` when present, otherwise against its `action` string, using the
/// same `*`-wildcard glob semantics as [`AgentToolPolicy`] (a bare prefix like
/// `git.` only matches with an explicit trailing `*`, e.g. `git.*`). `auto_deny`
/// is checked first and wins on overlap (fail closed). When neither list
/// matches, `default` applies: `ask` escalates to a pending human interaction,
/// `allow` / `deny` short-circuit without one.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ApprovalPolicy {
    #[serde(default)]
    pub auto_allow: Vec<String>,
    #[serde(default)]
    pub auto_deny: Vec<String>,
    #[serde(default)]
    pub default: ApprovalPolicyDefault,
    /// Model id for the LLM judge when `default: llm`. When unset the kernel
    /// falls back to the agent's own model / the compiled default. Ignored
    /// unless the default (or a future per-pattern rule) selects LLM mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_model: Option<String>,
    /// Extra, operator-authored guidance appended to the judge's system prompt
    /// (e.g. "deny anything that touches production or deletes data"). The base
    /// judge rubric is built in to the kernel; this narrows it per agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_instructions: Option<String>,
}

impl ApprovalPolicy {
    pub fn evaluate(&self, subject: &str) -> ApprovalPolicyDecision {
        if self.auto_deny.iter().any(|pattern| glob_match(pattern, subject)) {
            return ApprovalPolicyDecision::Deny;
        }
        if self.auto_allow.iter().any(|pattern| glob_match(pattern, subject)) {
            return ApprovalPolicyDecision::Allow;
        }
        match self.default {
            ApprovalPolicyDefault::Ask => ApprovalPolicyDecision::Ask,
            ApprovalPolicyDefault::Allow => ApprovalPolicyDecision::Allow,
            ApprovalPolicyDefault::Deny => ApprovalPolicyDecision::Deny,
            ApprovalPolicyDefault::Llm => ApprovalPolicyDecision::Evaluate,
        }
    }
}

fn glob_match(pattern: &str, value: &str) -> bool {
    let pat = pattern.as_bytes();
    let val = value.as_bytes();
    glob_match_inner(pat, val)
}

fn glob_match_inner(pat: &[u8], val: &[u8]) -> bool {
    match (pat.first(), val.first()) {
        (None, None) => true,
        (Some(b'*'), _) => glob_match_inner(&pat[1..], val) || (!val.is_empty() && glob_match_inner(pat, &val[1..])),
        (Some(&p), Some(&v)) if p == v => glob_match_inner(&pat[1..], &val[1..]),
        _ => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AgentMcpServerSource {
    #[default]
    Builtin,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentMcpServerConfig {
    #[serde(default)]
    pub source: AgentMcpServerSource,
    #[serde(default)]
    pub tool_policy: AgentToolPolicy,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Recognized agent capability flags.
///
/// `capabilities` on an [`AgentProfile`] is an open-ended `BTreeMap<String, bool>`, but the
/// orchestrator gives special meaning to a few well-known keys. Workflow YAML authors should
/// prefer these names when expressing intent:
///
/// - `memory` — when `true`, the daemon injects the project-scoped memory MCP server
///   (`animus.memory.*` tools) into the agent's runtime contract so the spawned CLI can read and
///   write its own memory document. When `false` or absent, the memory MCP server is omitted.
///   Retention is bounded by [`AgentMemoryConfig::max_entries`] (FIFO, default
///   [`DEFAULT_AGENT_MEMORY_MAX_ENTRIES`]).
/// - `planning`, `queue_management`, `scheduling` — surfaced on engineering-manager personas
///   for prompt rendering and dispatch heuristics.
/// - `requirements_authoring`, `acceptance_validation` — product-owner persona signals.
/// - `implementation`, `testing`, `code_review` — software-engineer persona signals.
///
/// Unknown keys are preserved verbatim and exposed to prompt templates and downstream tools,
/// but receive no special handling by the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentCapabilities {
    #[serde(flatten)]
    pub flags: BTreeMap<String, bool>,
}

/// Capability key that gates exposure of the project-scoped memory MCP server to a spawned agent.
pub const AGENT_CAPABILITY_MEMORY: &str = "memory";

/// Returns true if the agent profile has the `memory` capability flag explicitly enabled.
///
/// Used by the workflow runner to decide whether to inject the memory MCP server into the
/// spawned agent's runtime contract. See [`AgentCapabilities`] for the catalog of recognized
/// capability keys.
pub fn agent_memory_capability_enabled(profile: &AgentProfile) -> bool {
    profile.capabilities.get(AGENT_CAPABILITY_MEMORY).copied().unwrap_or(false)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentPersonaConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub traits: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub customizations: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentMemoryWritePolicy {
    #[default]
    Explicit,
    PhaseSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentMemoryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_chars: Option<usize>,
    /// FIFO retention cap on stored memory entries. When set, an append that
    /// would push the document past this many entries trims the oldest
    /// entries first (front of the vector). `None` falls back to
    /// [`DEFAULT_AGENT_MEMORY_MAX_ENTRIES`] at the store layer so memory can
    /// never grow unbounded. A value of `0` is rejected by config validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entries: Option<usize>,
    #[serde(default)]
    pub write_policy: AgentMemoryWritePolicy,
}

/// Default FIFO retention cap applied by the agent-memory store when an agent
/// profile does not set [`AgentMemoryConfig::max_entries`]. Generous enough
/// that ordinary multi-phase coordination never trims, while still bounding
/// unbounded growth across long-lived projects.
pub const DEFAULT_AGENT_MEMORY_MAX_ENTRIES: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentCommunicationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub can_message: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_chars: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentProjectOverrides {
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<AgentPersonaConfig>,
    #[serde(default)]
    pub memory: AgentMemoryConfig,
    #[serde(default)]
    pub communication: AgentCommunicationConfig,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub tool_policy: AgentToolPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ApprovalPolicy>,
    /// Optional author-controlled harness-hook configuration (guardrail policy
    /// rules + constrained observers). Additive: a profile without a `hooks`
    /// block loads unchanged.
    #[serde(default, skip_serializing_if = "AgentHooksConfig::is_empty")]
    pub hooks: AgentHooksConfig,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_server_configs: Option<BTreeMap<String, AgentMcpServerConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_capabilities: Option<AgentCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_overrides: Option<BTreeMap<String, AgentProjectOverrides>>,
    /// Named model references from the top-level `models:` registry.
    /// First entry is the primary model, remaining entries are fallbacks.
    /// During compilation, these are expanded into `model` + `fallback_models`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub tool_profile: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    /// Optional explicit tool overrides for each fallback model.
    /// When non-empty, `fallback_tools[i]` is used for `fallback_models[i]`.
    /// If shorter than `fallback_models`, missing entries are auto-derived.
    #[serde(default)]
    pub fallback_tools: Vec<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Provider permission/approval mode forwarded verbatim to the spawned
    /// CLI (claude `--permission-mode`, codex `-c approval_policy`, gemini
    /// approval mode). Values are provider-specific; see
    /// [`KNOWN_PERMISSION_MODES`].
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub web_search: Option<bool>,
    #[serde(default)]
    pub network_access: Option<bool>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_attempts: Option<usize>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub codex_config_overrides: Vec<String>,
    #[serde(default)]
    pub max_continuations: Option<usize>,
}

/// Presence-aware overlay shape for [`AgentProfile`]. Every field is
/// `Option`-wrapped so a YAML/JSON overlay that explicitly sets a field to
/// its default value (e.g. `memory: { enabled: false }` or `mcp_servers: []`)
/// still wins over the base profile, while an omitted field inherits the
/// base value. Serialization skips absent fields, and deserialization
/// accepts the exact same input shape as [`AgentProfile`].
///
/// TODO(codex-p2): fields that are already `Option` on [`AgentProfile`]
/// (e.g. `model`, `system_prompt_file`) cannot be reset to `None` by an
/// overlay — `field: null` deserializes identically to an omitted key, so
/// the merge inherits the base value. Distinguishing explicit null would
/// need a double-`Option` (or sentinel) wrapper on those fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentProfileOverlay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<AgentPersonaConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<AgentMemoryConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub communication: Option<AgentCommunicationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy: Option<AgentToolPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<AgentHooksConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<BTreeMap<String, bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_server_configs: Option<BTreeMap<String, AgentMcpServerConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_capabilities: Option<AgentCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_overrides: Option<BTreeMap<String, AgentProjectOverrides>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_access: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_config_overrides: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_continuations: Option<usize>,
}

impl AgentProfileOverlay {
    /// Materialize a full [`AgentProfile`] from this overlay alone, filling
    /// absent fields with their defaults. Used when an overlay introduces an
    /// agent that has no base profile to merge onto.
    pub fn to_profile(&self) -> AgentProfile {
        let mut profile = AgentProfile::default();
        merge_agent_profile(&mut profile, self);
        profile
    }

    /// Layer `overlay` onto `self` field by field: every field `overlay`
    /// declares (even explicitly at its default) wins, every omitted field
    /// keeps `self`'s value. Used when multiple workflow YAML files or pack
    /// overlays define the same agent id.
    pub fn merge_from(&mut self, overlay: &AgentProfileOverlay) {
        macro_rules! take_declared {
            ($($field:ident),+ $(,)?) => {
                $(
                    if overlay.$field.is_some() {
                        self.$field = overlay.$field.clone();
                    }
                )+
            };
        }
        take_declared!(
            name,
            description,
            system_prompt,
            system_prompt_file,
            role,
            persona,
            memory,
            communication,
            mcp_servers,
            tool_policy,
            approval_policy,
            hooks,
            skills,
            capabilities,
            mcp_server_configs,
            structured_capabilities,
            project_overrides,
            models,
            tool,
            tool_profile,
            model,
            fallback_models,
            fallback_tools,
            reasoning_effort,
            permission_mode,
            web_search,
            network_access,
            timeout_secs,
            max_attempts,
            extra_args,
            codex_config_overrides,
            max_continuations,
        );
    }
}

impl From<AgentProfile> for AgentProfileOverlay {
    fn from(profile: AgentProfile) -> Self {
        Self {
            name: profile.name,
            description: Some(profile.description),
            system_prompt: Some(profile.system_prompt),
            system_prompt_file: profile.system_prompt_file,
            role: profile.role,
            persona: profile.persona,
            memory: Some(profile.memory),
            communication: Some(profile.communication),
            mcp_servers: Some(profile.mcp_servers),
            tool_policy: Some(profile.tool_policy),
            approval_policy: profile.approval_policy,
            hooks: Some(profile.hooks),
            skills: Some(profile.skills),
            capabilities: Some(profile.capabilities),
            mcp_server_configs: profile.mcp_server_configs,
            structured_capabilities: profile.structured_capabilities,
            project_overrides: profile.project_overrides,
            models: Some(profile.models),
            tool: profile.tool,
            tool_profile: profile.tool_profile,
            model: profile.model,
            fallback_models: Some(profile.fallback_models),
            fallback_tools: Some(profile.fallback_tools),
            reasoning_effort: profile.reasoning_effort,
            permission_mode: profile.permission_mode,
            web_search: profile.web_search,
            network_access: profile.network_access,
            timeout_secs: profile.timeout_secs,
            max_attempts: profile.max_attempts,
            extra_args: Some(profile.extra_args),
            codex_config_overrides: Some(profile.codex_config_overrides),
            max_continuations: profile.max_continuations,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseCommandDefinition {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_command_cwd_mode")]
    pub cwd_mode: CommandCwdMode,
    #[serde(default)]
    pub cwd_path: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default = "default_success_exit_codes")]
    pub success_exit_codes: Vec<i32>,
    #[serde(default)]
    pub parse_json_output: bool,
    #[serde(default)]
    pub expected_result_kind: Option<String>,
    #[serde(default)]
    pub expected_schema: Option<Value>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub failure_pattern: Option<String>,
    #[serde(default)]
    pub excerpt_max_chars: Option<usize>,
    #[serde(default)]
    pub on_success_verdict: Option<String>,
    #[serde(default)]
    pub on_failure_verdict: Option<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub failure_risk: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseManualDefinition {
    pub instructions: String,
    #[serde(default)]
    pub approval_note_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Idempotency {
    Idempotent,
    Sideeffecting,
    #[default]
    Unknown,
}

/// Kind of eval check. See `EvalCheck` for the structured contract per kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalKind {
    /// Run a shell command in the phase's working directory. Pass on exit code match.
    Command,
    /// Dispatch a one-shot agent call. Pass when the response begins with "PASS".
    LlmJudge,
}

/// What to do when an eval gate fails. `Rework` re-executes the phase (up to
/// `max_reworks`) with the eval failure context injected into the next prompt;
/// `Block` pauses the workflow and emits a manual gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalOnFail {
    Rework,
    #[default]
    Block,
}

pub(crate) fn default_eval_pass_threshold() -> f32 {
    1.0
}

pub(crate) fn default_eval_expected_exit() -> i32 {
    0
}

/// A single eval check declared on a phase's `evals.checks` list. The `kind`
/// discriminates which fields apply: `command` checks require `command` (+
/// optional `args`, `working_dir`, `timeout_secs`, `expected_exit`);
/// `llm_judge` checks require `agent` and `prompt`. Cross-kind field
/// requirements are enforced by the validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCheck {
    pub id: String,
    pub kind: EvalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default = "default_eval_expected_exit")]
    pub expected_exit: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// Eval gate declared on a phase. Runs after the phase produces an
/// `advance` decision; pass rate is the fraction of `checks` that passed.
/// If `pass_rate >= pass_threshold` the phase advances; otherwise
/// `on_fail` controls whether to rework (up to `max_reworks`) or block.
///
/// Per the v0.5.5 basic-eval-framework contract:
/// - `pass_threshold` defaults to `1.0` (all checks must pass);
/// - `on_fail` defaults to `block` (no automatic rework);
/// - `max_reworks` defaults to `0` and is only honoured when
///   `on_fail = rework`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalsConfig {
    #[serde(default = "default_eval_pass_threshold")]
    pub pass_threshold: f32,
    #[serde(default)]
    pub on_fail: EvalOnFail,
    #[serde(default)]
    pub max_reworks: u32,
    #[serde(default)]
    pub checks: Vec<EvalCheck>,
}

impl Default for EvalsConfig {
    fn default() -> Self {
        Self {
            pass_threshold: default_eval_pass_threshold(),
            on_fail: EvalOnFail::default(),
            max_reworks: 0,
            checks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseExecutionDefinition {
    pub mode: PhaseExecutionMode,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub directive: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub runtime: Option<AgentRuntimeOverrides>,
    #[serde(default)]
    pub capabilities: Option<protocol::PhaseCapabilities>,
    #[serde(default)]
    pub output_contract: Option<PhaseOutputContract>,
    #[serde(default)]
    pub output_json_schema: Option<Value>,
    #[serde(default)]
    pub decision_contract: Option<PhaseDecisionContract>,
    #[serde(default)]
    pub retry: Option<PhaseRetryConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default)]
    pub command: Option<PhaseCommandDefinition>,
    #[serde(default)]
    pub manual: Option<PhaseManualDefinition>,
    #[serde(default)]
    pub default_tool: Option<String>,
    #[serde(default)]
    pub idempotency: Idempotency,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<crate::workflow_config::WorktreeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evals: Option<EvalsConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliToolConfig {
    #[serde(default)]
    pub executable: Option<String>,
    #[serde(default)]
    pub supports_file_editing: Option<bool>,
    #[serde(default)]
    pub supports_streaming: Option<bool>,
    #[serde(default)]
    pub supports_tool_use: Option<bool>,
    #[serde(default)]
    pub supports_vision: Option<bool>,
    #[serde(default)]
    pub supports_long_context: Option<bool>,
    #[serde(default)]
    pub max_context_tokens: Option<usize>,
    #[serde(default)]
    pub supports_mcp: Option<bool>,
    #[serde(default)]
    pub read_only_flag: Option<String>,
    #[serde(default)]
    pub response_schema_flag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRuntimeConfig {
    pub schema: String,
    pub version: u32,
    #[serde(default)]
    pub tools_allowlist: Vec<String>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentProfile>,
    #[serde(default)]
    pub phases: BTreeMap<String, PhaseExecutionDefinition>,
    #[serde(default)]
    pub cli_tools: BTreeMap<String, CliToolConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentRuntimeOverlay {
    #[serde(default)]
    pub tools_allowlist: Vec<String>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentProfileOverlay>,
    #[serde(default)]
    pub phases: BTreeMap<String, PhaseExecutionDefinition>,
    #[serde(default)]
    pub cli_tools: BTreeMap<String, CliToolConfig>,
}

impl Default for AgentRuntimeConfig {
    fn default() -> Self {
        builtin_agent_runtime_config()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeSource {
    WorkflowYaml,
    Builtin,
    BuiltinFallback,
}

impl AgentRuntimeSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WorkflowYaml => "workflow_yaml",
            Self::Builtin => "builtin",
            Self::BuiltinFallback => "builtin_fallback",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRuntimeMetadata {
    pub schema: String,
    pub version: u32,
    pub hash: String,
    pub source: AgentRuntimeSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedAgentRuntimeConfig {
    pub config: AgentRuntimeConfig,
    pub metadata: AgentRuntimeMetadata,
    pub path: PathBuf,
}

fn default_command_cwd_mode() -> CommandCwdMode {
    CommandCwdMode::TaskRoot
}

fn default_success_exit_codes() -> Vec<i32> {
    vec![0]
}

fn lookup_case_insensitive<'a, T>(map: &'a BTreeMap<String, T>, key: &str) -> Option<&'a T> {
    map.get(key)
        .or_else(|| map.iter().find(|(candidate, _)| candidate.eq_ignore_ascii_case(key)).map(|(_, value)| value))
}

fn trim_nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|candidate| !candidate.is_empty())
}

fn normalized_nonempty_values(values: &[String]) -> Vec<String> {
    values.iter().map(String::as_str).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned).collect()
}

impl AgentRuntimeConfig {
    pub fn phase_capabilities(&self, phase_id: &str) -> protocol::PhaseCapabilities {
        self.phase_execution(phase_id)
            .and_then(|def| def.capabilities.clone())
            .unwrap_or_default()
            .merge_with_defaults(phase_id)
    }

    pub fn has_phase_definition(&self, phase_id: &str) -> bool {
        self.phase_execution(phase_id).is_some()
    }

    pub fn phase_execution(&self, phase_id: &str) -> Option<&PhaseExecutionDefinition> {
        lookup_case_insensitive(&self.phases, phase_id).or_else(|| lookup_case_insensitive(&self.phases, "default"))
    }

    pub fn phase_mode(&self, phase_id: &str) -> Option<PhaseExecutionMode> {
        self.phase_execution(phase_id).map(|definition| definition.mode.clone())
    }

    pub fn phase_agent_id(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(self.phase_execution(phase_id).and_then(|definition| definition.agent_id.as_deref()))
    }

    pub fn agent_profile(&self, agent_id: &str) -> Option<&AgentProfile> {
        lookup_case_insensitive(&self.agents, agent_id)
    }

    pub fn phase_agent_profile(&self, phase_id: &str) -> Option<&AgentProfile> {
        self.phase_agent_id(phase_id).and_then(|agent_id| self.agent_profile(agent_id))
    }

    pub fn phase_system_prompt(&self, phase_id: &str) -> Option<&str> {
        if let Some(prompt) = self
            .phase_execution(phase_id)
            .and_then(|def| def.system_prompt.as_deref())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            return Some(prompt);
        }
        self.phase_agent_profile(phase_id).map(|profile| profile.system_prompt.trim()).filter(|value| !value.is_empty())
    }

    pub fn phase_tool_override(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(
            self.phase_execution(phase_id)
                .and_then(|definition| definition.runtime.as_ref())
                .and_then(|runtime| runtime.tool.as_deref()),
        )
        .or_else(|| trim_nonempty(self.phase_agent_profile(phase_id).and_then(|profile| profile.tool.as_deref())))
    }

    pub fn phase_model_override(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(
            self.phase_execution(phase_id)
                .and_then(|definition| definition.runtime.as_ref())
                .and_then(|runtime| runtime.model.as_deref()),
        )
        .or_else(|| trim_nonempty(self.phase_agent_profile(phase_id).and_then(|profile| profile.model.as_deref())))
    }

    pub fn phase_tool_profile(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(
            self.phase_execution(phase_id)
                .and_then(|definition| definition.runtime.as_ref())
                .and_then(|runtime| runtime.tool_profile.as_deref()),
        )
        .or_else(|| {
            trim_nonempty(self.phase_agent_profile(phase_id).and_then(|profile| profile.tool_profile.as_deref()))
        })
    }

    pub fn phase_fallback_models(&self, phase_id: &str) -> Vec<String> {
        if let Some(runtime_models) = self
            .phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .map(|runtime| runtime.fallback_models.clone())
            .filter(|models| !models.is_empty())
        {
            return runtime_models;
        }

        self.phase_agent_profile(phase_id)
            .map(|profile| {
                profile
                    .fallback_models
                    .iter()
                    .map(String::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn phase_fallback_tools(&self, phase_id: &str) -> Vec<String> {
        if let Some(runtime_tools) = self
            .phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .map(|runtime| runtime.fallback_tools.clone())
            .filter(|tools| !tools.is_empty())
        {
            return runtime_tools;
        }

        self.phase_agent_profile(phase_id)
            .map(|profile| {
                profile
                    .fallback_tools
                    .iter()
                    .map(String::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn phase_reasoning_effort(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(
            self.phase_execution(phase_id)
                .and_then(|definition| definition.runtime.as_ref())
                .and_then(|runtime| runtime.reasoning_effort.as_deref()),
        )
        .or_else(|| {
            trim_nonempty(self.phase_agent_profile(phase_id).and_then(|profile| profile.reasoning_effort.as_deref()))
        })
    }

    pub fn phase_permission_mode(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(
            self.phase_execution(phase_id)
                .and_then(|definition| definition.runtime.as_ref())
                .and_then(|runtime| runtime.permission_mode.as_deref()),
        )
        .or_else(|| {
            trim_nonempty(self.phase_agent_profile(phase_id).and_then(|profile| profile.permission_mode.as_deref()))
        })
        // The field serializes through the compiled agent runtime config and
        // deserializes into
        // `animus-runtime-shared::WorkflowPhaseRuntimeSettings::permission_mode`.
        // The workflow-runner-side consumption lives out-of-tree
        // (launchapp-dev/animus-workflow-runner-default) and maps it onto
        // session requests starting with its next release.
    }

    pub fn phase_web_search(&self, phase_id: &str) -> Option<bool> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .and_then(|runtime| runtime.web_search)
            .or_else(|| self.phase_agent_profile(phase_id).and_then(|profile| profile.web_search))
    }

    pub fn phase_network_access(&self, phase_id: &str) -> Option<bool> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .and_then(|runtime| runtime.network_access)
            .or_else(|| self.phase_agent_profile(phase_id).and_then(|profile| profile.network_access))
    }

    pub fn phase_timeout_secs(&self, phase_id: &str) -> Option<u64> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .and_then(|runtime| runtime.timeout_secs)
            .or_else(|| self.phase_agent_profile(phase_id).and_then(|profile| profile.timeout_secs))
    }

    pub fn phase_max_attempts(&self, phase_id: &str) -> Option<usize> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .and_then(|runtime| runtime.max_attempts)
            .or_else(|| self.phase_agent_profile(phase_id).and_then(|profile| profile.max_attempts))
    }

    pub fn phase_max_continuations(&self, phase_id: &str) -> Option<usize> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .and_then(|runtime| runtime.max_continuations)
            .or_else(|| self.phase_agent_profile(phase_id).and_then(|profile| profile.max_continuations))
    }

    pub fn phase_extra_args(&self, phase_id: &str) -> Vec<String> {
        if let Some(args) = self
            .phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .map(|runtime| normalized_nonempty_values(&runtime.extra_args))
            .filter(|args| !args.is_empty())
        {
            return args;
        }

        self.phase_agent_profile(phase_id)
            .map(|profile| normalized_nonempty_values(&profile.extra_args))
            .unwrap_or_default()
    }

    pub fn phase_codex_config_overrides(&self, phase_id: &str) -> Vec<String> {
        if let Some(overrides) = self
            .phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .map(|runtime| normalized_nonempty_values(&runtime.codex_config_overrides))
            .filter(|overrides| !overrides.is_empty())
        {
            return overrides;
        }

        self.phase_agent_profile(phase_id)
            .map(|profile| normalized_nonempty_values(&profile.codex_config_overrides))
            .unwrap_or_default()
    }

    pub fn phase_output_json_schema(&self, phase_id: &str) -> Option<&Value> {
        self.phase_execution(phase_id).and_then(|definition| definition.output_json_schema.as_ref())
    }

    pub fn phase_directive(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(self.phase_execution(phase_id).and_then(|definition| definition.directive.as_deref()))
    }

    pub fn phase_output_contract(&self, phase_id: &str) -> Option<&PhaseOutputContract> {
        self.phase_execution(phase_id).and_then(|definition| definition.output_contract.as_ref())
    }

    pub fn phase_decision_contract(&self, phase_id: &str) -> Option<&PhaseDecisionContract> {
        self.phase_execution(phase_id).and_then(|def| def.decision_contract.as_ref())
    }

    pub fn phase_command(&self, phase_id: &str) -> Option<&PhaseCommandDefinition> {
        self.phase_execution(phase_id).and_then(|definition| definition.command.as_ref())
    }

    pub fn phase_evals(&self, phase_id: &str) -> Option<&EvalsConfig> {
        self.phase_execution(phase_id).and_then(|definition| definition.evals.as_ref())
    }

    pub fn is_structured_output_phase(&self, phase_id: &str) -> bool {
        let trimmed_phase_id = phase_id.trim();
        if trimmed_phase_id.is_empty() {
            return false;
        }

        if self.phase_execution(trimmed_phase_id).is_some_and(|definition| {
            definition.output_contract.is_some()
                || definition.output_json_schema.is_some()
                || definition.decision_contract.is_some()
        }) {
            return true;
        }

        let normalized = trimmed_phase_id.to_ascii_lowercase();
        matches!(
            normalized.as_str(),
            "review"
                | "manual-review"
                | "code-review"
                | "security-audit"
                | "po-review"
                | "em-review"
                | "rework-review"
                | "task-generation"
                | "mockup"
        ) || normalized.contains("review")
            || normalized.contains("audit")
    }
}

pub fn builtin_agent_runtime_config() -> AgentRuntimeConfig {
    static BUILTIN_CONFIG: OnceLock<AgentRuntimeConfig> = OnceLock::new();
    BUILTIN_CONFIG
        .get_or_init(|| match serde_json::from_str::<AgentRuntimeConfig>(BUILTIN_AGENT_RUNTIME_CONFIG_JSON) {
            Ok(config) if validate_agent_runtime_config(&config).is_ok() => config,
            _ => hardcoded_builtin_agent_runtime_config(),
        })
        .clone()
}

fn hardcoded_builtin_agent_runtime_config() -> AgentRuntimeConfig {
    let implementation_output_contract = PhaseOutputContract {
        kind: "implementation_result".to_string(),
        required_fields: vec!["commit_message".to_string()],
        fields: BTreeMap::new(),
    };
    let swe_mcp_servers = vec!["animus".to_string()];
    let swe_tool_policy = AgentToolPolicy {
        allow: vec![
            "task.*".to_string(),
            "workflow.*".to_string(),
            "output.*".to_string(),
            "history.*".to_string(),
            "errors.*".to_string(),
        ],
        deny: vec!["project.remove".to_string(), "daemon.stop".to_string(), "requirements.delete".to_string()],
    };
    let swe_capabilities = BTreeMap::from([
        ("planning".to_string(), false),
        ("queue_management".to_string(), false),
        ("scheduling".to_string(), false),
        ("requirements_authoring".to_string(), false),
        ("acceptance_validation".to_string(), false),
        ("implementation".to_string(), true),
        ("testing".to_string(), true),
        ("code_review".to_string(), true),
    ]);

    AgentRuntimeConfig {
        schema: AGENT_RUNTIME_CONFIG_SCHEMA_ID.to_string(),
        version: AGENT_RUNTIME_CONFIG_VERSION,
        tools_allowlist: vec![
            "cargo".to_string(),
            "npm".to_string(),
            "pnpm".to_string(),
            "yarn".to_string(),
            "bun".to_string(),
            "pytest".to_string(),
            "go".to_string(),
            "bash".to_string(),
            "sh".to_string(),
            "make".to_string(),
            "just".to_string(),
        ],
        agents: BTreeMap::from([
            (
                "default".to_string(),
                AgentProfile {
                    name: None,
                    description: "Default workflow phase agent profile".to_string(),
                    system_prompt: "You are the workflow phase execution agent. Produce deterministic, repository-safe outputs and keep changes scoped to the active phase.".to_string(),
                    system_prompt_file: None,
                    role: None,
                    persona: None,
                    memory: AgentMemoryConfig::default(),
                    communication: AgentCommunicationConfig::default(),
                    mcp_servers: Vec::new(),
                    tool_policy: AgentToolPolicy::default(),
                    skills: vec![],
                    capabilities: BTreeMap::new(),
                    tool: None,
                    tool_profile: None,
                    model: None,
                    fallback_models: vec![],
                    models: vec![],
                    fallback_tools: vec![],
                    reasoning_effort: None,
                    permission_mode: None,
                    web_search: None,
                    network_access: None,
                    timeout_secs: None,
                    max_attempts: None,
                    extra_args: vec![],
                    codex_config_overrides: vec![],
                    max_continuations: None,
                    approval_policy: None,
                    hooks: AgentHooksConfig::default(),
                    mcp_server_configs: None,
                    structured_capabilities: None,
                    project_overrides: None,
                },
            ),
            (
                "implementation".to_string(),
                AgentProfile {
                    name: None,
                    description: "Compatibility alias for the software engineer persona.".to_string(),
                    system_prompt: "You are the software engineer execution agent. Implement production-ready code changes, add or update tests, and perform rigorous code review while keeping edits minimal and verifiable.".to_string(),
                    system_prompt_file: None,
                    role: Some("software_engineer".to_string()),
                    persona: None,
                    memory: AgentMemoryConfig::default(),
                    communication: AgentCommunicationConfig::default(),
                    mcp_servers: swe_mcp_servers.clone(),
                    tool_policy: swe_tool_policy.clone(),
                    skills: vec![
                        "implementation".to_string(),
                        "testing".to_string(),
                        "code-review".to_string(),
                        "debugging".to_string(),
                    ],
                    capabilities: swe_capabilities.clone(),
                    tool: None,
                    tool_profile: None,
                    model: None,
                    fallback_models: vec![],
                    models: vec![],
                    fallback_tools: vec![],
                    reasoning_effort: None,
                    permission_mode: None,
                    web_search: None,
                    network_access: None,
                    timeout_secs: None,
                    max_attempts: None,
                    extra_args: vec![],
                    codex_config_overrides: vec![],
                    max_continuations: None,
                    approval_policy: None,
                    hooks: AgentHooksConfig::default(),
                    mcp_server_configs: None,
                    structured_capabilities: None,
                    project_overrides: None,
                },
            ),
            (
                "em".to_string(),
                AgentProfile {
                    name: None,
                    description: "Engineering Manager persona for prioritization, queue management, and scheduling.".to_string(),
                    system_prompt: "You are the Engineering Manager agent. Prioritize work, manage queue health, sequence delivery safely, and keep execution plans realistic and dependency-aware.".to_string(),
                    system_prompt_file: None,
                    role: Some("engineering_manager".to_string()),
                    persona: None,
                    memory: AgentMemoryConfig::default(),
                    communication: AgentCommunicationConfig::default(),
                    mcp_servers: vec!["animus".to_string()],
                    tool_policy: AgentToolPolicy {
                        allow: vec![
                            "task.*".to_string(),
                            "workflow.*".to_string(),
                            "history.*".to_string(),
                        ],
                        deny: vec![
                            "task.delete".to_string(),
                            "requirements.delete".to_string(),
                            "project.remove".to_string(),
                            "git.*".to_string(),
                        ],
                    },
                    skills: vec![
                        "prioritization".to_string(),
                        "queue-management".to_string(),
                        "scheduling".to_string(),
                        "risk-management".to_string(),
                    ],
                    capabilities: BTreeMap::from([
                        ("planning".to_string(), true),
                        ("queue_management".to_string(), true),
                        ("scheduling".to_string(), true),
                        ("requirements_authoring".to_string(), false),
                        ("acceptance_validation".to_string(), true),
                        ("implementation".to_string(), false),
                        ("testing".to_string(), false),
                        ("code_review".to_string(), true),
                    ]),
                    tool: None,
                    tool_profile: None,
                    model: None,
                    fallback_models: vec![],
                    models: vec![],
                    fallback_tools: vec![],
                    reasoning_effort: None,
                    permission_mode: None,
                    web_search: None,
                    network_access: None,
                    timeout_secs: None,
                    max_attempts: None,
                    extra_args: vec![],
                    codex_config_overrides: vec![],
                    max_continuations: None,
                    approval_policy: None,
                    hooks: AgentHooksConfig::default(),
                    mcp_server_configs: None,
                    structured_capabilities: None,
                    project_overrides: None,
                },
            ),
            (
                "po".to_string(),
                AgentProfile {
                    name: None,
                    description: "Product Owner persona for requirements, vision, acceptance criteria, and deliverable validation.".to_string(),
                    system_prompt: "You are the Product Owner agent. Refine requirements into clear acceptance criteria, align work to product vision, and validate deliverables against user outcomes.".to_string(),
                    system_prompt_file: None,
                    role: Some("product_owner".to_string()),
                    persona: None,
                    memory: AgentMemoryConfig::default(),
                    communication: AgentCommunicationConfig::default(),
                    mcp_servers: vec!["animus".to_string()],
                    tool_policy: AgentToolPolicy {
                        allow: vec![
                            "vision.*".to_string(),
                            "requirements.*".to_string(),
                            "task.*".to_string(),
                            "review.*".to_string(),
                            "qa.*".to_string(),
                            "workflow.*".to_string(),
                        ],
                        deny: vec![
                            "task.delete".to_string(),
                            "project.remove".to_string(),
                            "git.*".to_string(),
                        ],
                    },
                    skills: vec![
                        "vision-alignment".to_string(),
                        "requirements-management".to_string(),
                        "acceptance-criteria".to_string(),
                        "deliverable-validation".to_string(),
                    ],
                    capabilities: BTreeMap::from([
                        ("planning".to_string(), true),
                        ("queue_management".to_string(), false),
                        ("scheduling".to_string(), false),
                        ("requirements_authoring".to_string(), true),
                        ("acceptance_validation".to_string(), true),
                        ("implementation".to_string(), false),
                        ("testing".to_string(), false),
                        ("code_review".to_string(), true),
                    ]),
                    tool: None,
                    tool_profile: None,
                    model: None,
                    fallback_models: vec![],
                    models: vec![],
                    fallback_tools: vec![],
                    reasoning_effort: None,
                    permission_mode: None,
                    web_search: None,
                    network_access: None,
                    timeout_secs: None,
                    max_attempts: None,
                    extra_args: vec![],
                    codex_config_overrides: vec![],
                    max_continuations: None,
                    approval_policy: None,
                    hooks: AgentHooksConfig::default(),
                    mcp_server_configs: None,
                    structured_capabilities: None,
                    project_overrides: None,
                },
            ),
            (
                "swe".to_string(),
                AgentProfile {
                    name: None,
                    description: "Software Engineer persona for implementation, testing, and code review.".to_string(),
                    system_prompt: "You are the software engineer execution agent. Implement production-ready code changes, add or update tests, and perform rigorous code review while keeping edits minimal and verifiable.".to_string(),
                    system_prompt_file: None,
                    role: Some("software_engineer".to_string()),
                    persona: None,
                    memory: AgentMemoryConfig::default(),
                    communication: AgentCommunicationConfig::default(),
                    mcp_servers: swe_mcp_servers,
                    tool_policy: swe_tool_policy,
                    skills: vec![
                        "implementation".to_string(),
                        "testing".to_string(),
                        "code-review".to_string(),
                        "debugging".to_string(),
                    ],
                    capabilities: swe_capabilities,
                    tool: None,
                    tool_profile: None,
                    model: None,
                    fallback_models: vec![],
                    models: vec![],
                    fallback_tools: vec![],
                    reasoning_effort: None,
                    permission_mode: None,
                    web_search: None,
                    network_access: None,
                    timeout_secs: None,
                    max_attempts: None,
                    extra_args: vec![],
                    codex_config_overrides: vec![],
                    max_continuations: None,
                    approval_policy: None,
                    hooks: AgentHooksConfig::default(),
                    mcp_server_configs: None,
                    structured_capabilities: None,
                    project_overrides: None,
                },
            ),
        ]),
        phases: BTreeMap::from([
            (
                "default".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some(
                        "Execute the current workflow phase with production-quality output."
                            .to_string(),
                    ),
                    system_prompt: None,
                    runtime: None,
                    capabilities: None,
                    output_contract: None,
                    output_json_schema: None,
                    decision_contract: None,
                    retry: None,
                    skills: Vec::new(),
                    command: None,
                    manual: None,
                    default_tool: None,
                    idempotency: Idempotency::Unknown,
                    worktree: None,
                evals: None,
                },
            ),
            (
                "requirements".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("po".to_string()),
                    directive: Some("Clarify implementation scope, constraints, and acceptance criteria. Update docs and implementation notes as needed.".to_string()),
                    system_prompt: None,
                    runtime: None,
                    capabilities: None,
                    output_contract: None,
                    output_json_schema: None,
                    decision_contract: Some(PhaseDecisionContract {
                        required_evidence: Vec::new(),
                        min_confidence: 0.6,
                        max_risk: crate::types::WorkflowDecisionRisk::Medium,
                        allow_missing_decision: true,
                        extra_json_schema: None,
                        fields: BTreeMap::new(),
                    }),
                    retry: None,
                    skills: Vec::new(),
                    command: None,
                    manual: None,
                    default_tool: None,
                    idempotency: Idempotency::Unknown,
                    worktree: None,
                evals: None,
                },
            ),
            (
                "research".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some(
                        "Gather external and codebase evidence needed to de-risk the next implementation step. Treat greenfield repositories as valid and provide assumptions/plan artifacts when source is sparse. Keep discovery targeted to first-party code and active requirement/task docs; avoid broad scans of dependency or workflow checkpoint directories."
                            .to_string(),
                    ),
                    system_prompt: None,
                    runtime: Some(AgentRuntimeOverrides {
                        web_search: Some(true),
                        timeout_secs: Some(900),
                        ..AgentRuntimeOverrides::default()
                    }),
                    capabilities: None,
                    output_contract: None,
                    output_json_schema: None,
                    decision_contract: None,
                    retry: None,
                    skills: Vec::new(),
                    command: None,
                    manual: None,
                    default_tool: None,
                    idempotency: Idempotency::Unknown,
                    worktree: None,
                evals: None,
                },
            ),
            (
                "ux-research".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some("Produce a UX brief from requirements and user flows. Identify key screens, interactions, and accessibility constraints.".to_string()),
                    system_prompt: None,
                    runtime: None,
                    capabilities: None,
                    output_contract: None,
                    output_json_schema: None,
                    decision_contract: None,
                    retry: None,
                    skills: Vec::new(),
                    command: None,
                    manual: None,
                    default_tool: None,
                    idempotency: Idempotency::Unknown,
                    worktree: None,
                evals: None,
                },
            ),
            (
                "wireframe".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some("Create concrete UI mockups/wireframes in the repository under mockups/. Prefer production-like React-oriented layouts and realistic states.".to_string()),
                    system_prompt: None,
                    runtime: None,
                    capabilities: None,
                    output_contract: None,
                    output_json_schema: None,
                    decision_contract: None,
                    retry: None,
                    skills: Vec::new(),
                    command: None,
                    manual: None,
                    default_tool: None,
                    idempotency: Idempotency::Unknown,
                    worktree: None,
                evals: None,
                },
            ),
            (
                "mockup-review".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some("Review mockups against linked requirements. Resolve mismatches, improve usability, and ensure acceptance criteria traceability.".to_string()),
                    system_prompt: None,
                    runtime: None,
                    capabilities: None,
                    output_contract: None,
                    output_json_schema: None,
                    decision_contract: None,
                    retry: None,
                    skills: Vec::new(),
                    command: None,
                    manual: None,
                    default_tool: None,
                    idempotency: Idempotency::Unknown,
                    worktree: None,
                evals: None,
                },
            ),
            (
                "implementation".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("swe".to_string()),
                    directive: Some(
                        "Implement production-quality code for this task. Keep changes focused and executable."
                            .to_string(),
                    ),
                    system_prompt: None,
                    runtime: None,
                    capabilities: None,
                    output_contract: Some(implementation_output_contract.clone()),
                    output_json_schema: Some(json!({
                        "type": "object",
                        "required": ["kind", "commit_message"],
                        "properties": {
                            "kind": {"const": "implementation_result"},
                            "commit_message": {"type": "string", "minLength": 1}
                        },
                        "additionalProperties": true
                    })),
                    decision_contract: Some(PhaseDecisionContract {
                        required_evidence: Vec::new(),
                        min_confidence: 0.7,
                        max_risk: crate::types::WorkflowDecisionRisk::Medium,
                        allow_missing_decision: true,
                        extra_json_schema: None,
                        fields: BTreeMap::new(),
                    }),
                    retry: None,
                    skills: Vec::new(),
                    command: None,
                    manual: None,
                    default_tool: None,
                    idempotency: Idempotency::Unknown,
                    worktree: None,
                evals: None,
                },
            ),
        ]),
        cli_tools: BTreeMap::new(),
    }
}

pub fn agent_runtime_config_path(project_root: &Path) -> PathBuf {
    let base = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".animus"));
    base.join("config").join(AGENT_RUNTIME_CONFIG_FILE_NAME)
}

pub fn ensure_agent_runtime_config_file(project_root: &Path) -> Result<()> {
    crate::workflow_config::ensure_workflow_yaml_scaffold(project_root).map(|_| ())
}

pub fn load_agent_runtime_config(project_root: &Path) -> Result<AgentRuntimeConfig> {
    Ok(load_agent_runtime_config_with_metadata(project_root)?.config)
}

pub fn load_agent_runtime_config_with_metadata(project_root: &Path) -> Result<LoadedAgentRuntimeConfig> {
    if let Ok(loaded_workflow) = crate::workflow_config::load_workflow_config_with_metadata(project_root) {
        let mut config = builtin_agent_runtime_config();
        let registry = crate::resolve_pack_registry(project_root)?;
        let mut path = loaded_workflow.path.clone();

        for entry in registry.entries_for_source(crate::PackRegistrySource::Installed) {
            let Some(pack) = entry.loaded_manifest() else {
                continue;
            };
            if let Some(overlay) = crate::load_pack_agent_runtime_overlay(pack)? {
                merge_agent_runtime_overlay(&mut config, &overlay);
                path = entry.pack_root.clone().unwrap_or_else(crate::machine_installed_packs_dir);
            }
        }

        merge_workflow_runtime_overlay(&mut config, &loaded_workflow.config);

        for entry in registry.entries_for_source(crate::PackRegistrySource::ProjectOverride) {
            let Some(pack) = entry.loaded_manifest() else {
                continue;
            };
            if let Some(overlay) = crate::load_pack_agent_runtime_overlay(pack)? {
                merge_agent_runtime_overlay(&mut config, &overlay);
                path = entry.pack_root.clone().unwrap_or_else(|| crate::project_pack_overrides_dir(project_root));
            }
        }

        backfill_agent_system_prompts(&mut config);
        validate_agent_runtime_config(&config)?;

        return Ok(LoadedAgentRuntimeConfig {
            metadata: AgentRuntimeMetadata {
                schema: config.schema.clone(),
                version: config.version,
                hash: agent_runtime_config_hash(&config),
                source: AgentRuntimeSource::WorkflowYaml,
            },
            config,
            path,
        });
    }

    Err(anyhow!(
        "agent runtime config is missing. Define runtime in .animus/workflows.yaml or .animus/workflows/*.yaml"
    ))
}

pub fn load_agent_runtime_config_or_default(project_root: &Path) -> AgentRuntimeConfig {
    match load_agent_runtime_config_with_metadata(project_root) {
        Ok(loaded) => loaded.config,
        Err(_) => builtin_agent_runtime_config(),
    }
}

fn merge_workflow_runtime_overlay(base: &mut AgentRuntimeConfig, workflow: &crate::workflow_config::WorkflowConfig) {
    for tool in &workflow.tools_allowlist {
        if !tool.trim().is_empty() && !base.tools_allowlist.iter().any(|candidate| candidate.eq_ignore_ascii_case(tool))
        {
            base.tools_allowlist.push(tool.clone());
        }
    }
    for (agent_id, profile) in &workflow.agent_profiles {
        match base.agents.get_mut(agent_id) {
            Some(existing) => merge_agent_profile(existing, profile),
            None => {
                base.agents.insert(agent_id.clone(), profile.to_profile());
            }
        }
    }
    for (phase_id, definition) in &workflow.phase_definitions {
        base.phases.insert(phase_id.clone(), definition.clone());
    }
    for (tool_id, definition) in &workflow.tools {
        let entry = base.cli_tools.entry(tool_id.clone()).or_insert_with(|| CliToolConfig {
            executable: None,
            supports_file_editing: None,
            supports_streaming: None,
            supports_tool_use: None,
            supports_vision: None,
            supports_long_context: None,
            max_context_tokens: None,
            supports_mcp: None,
            read_only_flag: None,
            response_schema_flag: None,
        });
        entry.executable = Some(definition.executable.clone());
        if definition.supports_mcp.is_some() {
            entry.supports_mcp = definition.supports_mcp;
        }
        if definition.supports_write.is_some() {
            entry.supports_file_editing = definition.supports_write;
        }
        if definition.context_window.is_some() {
            entry.max_context_tokens = definition.context_window;
        }
        if definition.supports_streaming.is_some() {
            entry.supports_streaming = definition.supports_streaming;
        }
        if definition.supports_tool_use.is_some() {
            entry.supports_tool_use = definition.supports_tool_use;
        }
        if definition.supports_vision.is_some() {
            entry.supports_vision = definition.supports_vision;
        }
        if definition.supports_long_context.is_some() {
            entry.supports_long_context = definition.supports_long_context;
        }
        if definition.read_only_flag.is_some() {
            entry.read_only_flag = definition.read_only_flag.clone();
        }
        if definition.response_schema_flag.is_some() {
            entry.response_schema_flag = definition.response_schema_flag.clone();
        }
    }
}

pub(crate) fn merge_agent_runtime_overlay(base: &mut AgentRuntimeConfig, overlay: &AgentRuntimeOverlay) {
    for tool in &overlay.tools_allowlist {
        if !tool.trim().is_empty() && !base.tools_allowlist.iter().any(|candidate| candidate.eq_ignore_ascii_case(tool))
        {
            base.tools_allowlist.push(tool.clone());
        }
    }
    for (agent_id, profile) in &overlay.agents {
        match base.agents.get_mut(agent_id) {
            Some(existing) => merge_agent_profile(existing, profile),
            None => {
                base.agents.insert(agent_id.clone(), profile.to_profile());
            }
        }
    }
    for (phase_id, definition) in &overlay.phases {
        base.phases.insert(phase_id.clone(), definition.clone());
    }
    for (tool_id, definition) in &overlay.cli_tools {
        match base.cli_tools.get_mut(tool_id) {
            Some(existing) => merge_cli_tool_config(existing, definition),
            None => {
                base.cli_tools.insert(tool_id.clone(), definition.clone());
            }
        }
    }
}

fn merge_agent_profile(base: &mut AgentProfile, overlay: &AgentProfileOverlay) {
    if overlay.name.is_some() {
        base.name = overlay.name.clone();
    }
    if let Some(description) = &overlay.description {
        base.description = description.clone();
    }
    if let Some(system_prompt) = &overlay.system_prompt {
        base.system_prompt = system_prompt.clone();
    }
    if overlay.system_prompt_file.is_some() {
        base.system_prompt_file = overlay.system_prompt_file.clone();
    }
    if overlay.role.is_some() {
        base.role = overlay.role.clone();
    }
    if overlay.persona.is_some() {
        base.persona = overlay.persona.clone();
    }
    if let Some(memory) = &overlay.memory {
        base.memory = memory.clone();
    }
    if let Some(communication) = &overlay.communication {
        base.communication = communication.clone();
    }
    if let Some(mcp_servers) = &overlay.mcp_servers {
        base.mcp_servers = mcp_servers.clone();
    }
    if let Some(tool_policy) = &overlay.tool_policy {
        base.tool_policy = tool_policy.clone();
    }
    if overlay.approval_policy.is_some() {
        base.approval_policy = overlay.approval_policy.clone();
    }
    if let Some(hooks) = &overlay.hooks {
        base.hooks = hooks.clone();
    }
    if let Some(skills) = &overlay.skills {
        base.skills = skills.clone();
    }
    if let Some(capabilities) = &overlay.capabilities {
        base.capabilities = capabilities.clone();
    }
    if overlay.mcp_server_configs.is_some() {
        base.mcp_server_configs = overlay.mcp_server_configs.clone();
    }
    if overlay.structured_capabilities.is_some() {
        base.structured_capabilities = overlay.structured_capabilities.clone();
    }
    if overlay.project_overrides.is_some() {
        base.project_overrides = overlay.project_overrides.clone();
    }
    if overlay.tool.is_some() {
        base.tool = overlay.tool.clone();
    }
    if overlay.tool_profile.is_some() {
        base.tool_profile = overlay.tool_profile.clone();
    }
    if overlay.model.is_some() {
        base.model = overlay.model.clone();
    }
    if let Some(fallback_models) = &overlay.fallback_models {
        base.fallback_models = fallback_models.clone();
    }
    if let Some(fallback_tools) = &overlay.fallback_tools {
        base.fallback_tools = fallback_tools.clone();
    }
    if let Some(models) = &overlay.models {
        base.models = models.clone();
    }
    if overlay.reasoning_effort.is_some() {
        base.reasoning_effort = overlay.reasoning_effort.clone();
    }
    if overlay.permission_mode.is_some() {
        base.permission_mode = overlay.permission_mode.clone();
    }
    if overlay.web_search.is_some() {
        base.web_search = overlay.web_search;
    }
    if overlay.network_access.is_some() {
        base.network_access = overlay.network_access;
    }
    if overlay.timeout_secs.is_some() {
        base.timeout_secs = overlay.timeout_secs;
    }
    if overlay.max_attempts.is_some() {
        base.max_attempts = overlay.max_attempts;
    }
    if let Some(extra_args) = &overlay.extra_args {
        base.extra_args = extra_args.clone();
    }
    if let Some(codex_config_overrides) = &overlay.codex_config_overrides {
        base.codex_config_overrides = codex_config_overrides.clone();
    }
    if overlay.max_continuations.is_some() {
        base.max_continuations = overlay.max_continuations;
    }
}

fn merge_cli_tool_config(base: &mut CliToolConfig, overlay: &CliToolConfig) {
    if overlay.executable.is_some() {
        base.executable = overlay.executable.clone();
    }
    if overlay.supports_file_editing.is_some() {
        base.supports_file_editing = overlay.supports_file_editing;
    }
    if overlay.supports_streaming.is_some() {
        base.supports_streaming = overlay.supports_streaming;
    }
    if overlay.supports_tool_use.is_some() {
        base.supports_tool_use = overlay.supports_tool_use;
    }
    if overlay.supports_vision.is_some() {
        base.supports_vision = overlay.supports_vision;
    }
    if overlay.supports_long_context.is_some() {
        base.supports_long_context = overlay.supports_long_context;
    }
    if overlay.max_context_tokens.is_some() {
        base.max_context_tokens = overlay.max_context_tokens;
    }
    if overlay.supports_mcp.is_some() {
        base.supports_mcp = overlay.supports_mcp;
    }
    if overlay.read_only_flag.is_some() {
        base.read_only_flag = overlay.read_only_flag.clone();
    }
    if overlay.response_schema_flag.is_some() {
        base.response_schema_flag = overlay.response_schema_flag.clone();
    }
}

pub fn write_agent_runtime_config(project_root: &Path, config: &AgentRuntimeConfig) -> Result<()> {
    validate_agent_runtime_config(config)?;
    let workflow_overlay = crate::workflow_config::WorkflowConfig {
        schema: crate::workflow_config::WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: crate::workflow_config::WORKFLOW_CONFIG_VERSION,
        default_workflow_ref: String::new(),
        phase_catalog: BTreeMap::new(),
        workflows: Vec::new(),
        checkpoint_retention: crate::workflow_config::WorkflowCheckpointRetentionConfig::default(),
        phase_definitions: config.phases.clone(),
        agent_profiles: config
            .agents
            .iter()
            .map(|(agent_id, profile)| (agent_id.clone(), AgentProfileOverlay::from(profile.clone())))
            .collect(),
        agent_channels: BTreeMap::new(),
        tools_allowlist: config.tools_allowlist.clone(),
        mcp_servers: BTreeMap::new(),
        phase_mcp_bindings: BTreeMap::new(),
        tools: config
            .cli_tools
            .iter()
            .filter_map(|(tool_id, cli_tool)| {
                cli_tool.executable.as_ref().map(|executable| {
                    (
                        tool_id.clone(),
                        crate::workflow_config::ToolDefinition {
                            executable: executable.clone(),
                            supports_mcp: cli_tool.supports_mcp,
                            supports_write: cli_tool.supports_file_editing,
                            context_window: cli_tool.max_context_tokens,
                            base_args: Vec::new(),
                            supports_streaming: cli_tool.supports_streaming,
                            supports_tool_use: cli_tool.supports_tool_use,
                            supports_vision: cli_tool.supports_vision,
                            supports_long_context: cli_tool.supports_long_context,
                            read_only_flag: cli_tool.read_only_flag.clone(),
                            response_schema_flag: cli_tool.response_schema_flag.clone(),
                        },
                    )
                })
            })
            .collect(),
        integrations: None,
        schedules: Vec::new(),
        triggers: Vec::new(),
        daemon: None,
        secrets: BTreeMap::new(),
    };
    crate::workflow_config::write_workflow_yaml_overlay(
        project_root,
        crate::workflow_config::GENERATED_RUNTIME_OVERLAY_FILE_NAME,
        &workflow_overlay,
    )
    .map(|_| ())
}

pub fn agent_runtime_config_hash(config: &AgentRuntimeConfig) -> String {
    let bytes = serde_json::to_vec(config).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn validate_phase_definition(
    phase_id: &str,
    definition: &PhaseExecutionDefinition,
    config: &AgentRuntimeConfig,
) -> Result<()> {
    fn is_valid_codex_config_override(value: &str) -> bool {
        let Some((key, expr)) = value.split_once('=') else {
            return false;
        };
        !key.trim().is_empty() && !expr.trim().is_empty()
    }

    if let Some(directive) = definition.directive.as_deref() {
        if directive.trim().is_empty() {
            return Err(anyhow!("phases['{}'].directive must not be empty when set", phase_id));
        }
    }

    if let Some(schema) = definition.output_json_schema.as_ref() {
        if !schema.is_object() {
            return Err(anyhow!("phases['{}'].output_json_schema must be a JSON object", phase_id));
        }
    }

    if let Some(contract) = definition.output_contract.as_ref() {
        if contract.kind.trim().is_empty() {
            return Err(anyhow!("phases['{}'].output_contract.kind must not be empty", phase_id));
        }
        if contract.required_fields.iter().any(|field| field.trim().is_empty()) {
            return Err(anyhow!(
                "phases['{}'].output_contract.required_fields must not contain empty values",
                phase_id
            ));
        }
        for (field_name, field) in &contract.fields {
            validate_phase_field_definition(
                format!("phases['{}'].output_contract.fields['{}']", phase_id, field_name),
                field,
            )?;
        }
    }

    if let Some(contract) = definition.decision_contract.as_ref() {
        if !(0.0..=1.0).contains(&contract.min_confidence) {
            return Err(anyhow!("phases['{}'].decision_contract.min_confidence must be between 0.0 and 1.0", phase_id));
        }
        if let Some(schema) = contract.extra_json_schema.as_ref() {
            if !schema.is_object() {
                return Err(anyhow!(
                    "phases['{}'].decision_contract.extra_json_schema must be a JSON object",
                    phase_id
                ));
            }
        }
        for (field_name, field) in &contract.fields {
            validate_phase_field_definition(
                format!("phases['{}'].decision_contract.fields['{}']", phase_id, field_name),
                field,
            )?;
        }
    }

    match definition.mode {
        PhaseExecutionMode::Agent => {
            let Some(agent_id) = trim_nonempty(definition.agent_id.as_deref()) else {
                return Err(anyhow!("phases['{}'] mode 'agent' requires non-empty agent_id", phase_id));
            };

            if lookup_case_insensitive(&config.agents, agent_id).is_none() {
                return Err(anyhow!("phases['{}'] references unknown agent '{}'", phase_id, agent_id));
            }

            if definition.command.is_some() {
                return Err(anyhow!("phases['{}'] mode 'agent' must not include command block", phase_id));
            }

            if definition.manual.is_some() {
                return Err(anyhow!("phases['{}'] mode 'agent' must not include manual block", phase_id));
            }
        }
        PhaseExecutionMode::Command => {
            let Some(command) = definition.command.as_ref() else {
                return Err(anyhow!("phases['{}'] mode 'command' requires command block", phase_id));
            };

            if command.program.trim().is_empty() {
                return Err(anyhow!("phases['{}'].command.program must not be empty", phase_id));
            }

            if command.args.iter().any(|value| value.trim().is_empty()) {
                return Err(anyhow!("phases['{}'].command.args must not contain empty values", phase_id));
            }

            if command.env.iter().any(|(key, _)| key.trim().is_empty()) {
                return Err(anyhow!("phases['{}'].command.env must not contain empty keys", phase_id));
            }

            if command.success_exit_codes.is_empty() {
                return Err(anyhow!(
                    "phases['{}'].command.success_exit_codes must include at least one code",
                    phase_id
                ));
            }

            if matches!(command.cwd_mode, CommandCwdMode::Path)
                && command.cwd_path.as_deref().is_none_or(|value| value.trim().is_empty())
            {
                return Err(anyhow!("phases['{}'].command.cwd_path must be set for cwd_mode='path'", phase_id));
            }

            if definition.agent_id.is_some() {
                return Err(anyhow!("phases['{}'] mode 'command' must not include agent_id", phase_id));
            }

            if definition.manual.is_some() {
                return Err(anyhow!("phases['{}'] mode 'command' must not include manual block", phase_id));
            }
        }
        PhaseExecutionMode::Manual => {
            let Some(manual) = definition.manual.as_ref() else {
                return Err(anyhow!("phases['{}'] mode 'manual' requires manual block", phase_id));
            };

            if manual.instructions.trim().is_empty() {
                return Err(anyhow!("phases['{}'].manual.instructions must not be empty", phase_id));
            }

            if let Some(timeout_secs) = manual.timeout_secs {
                if timeout_secs == 0 {
                    return Err(anyhow!("phases['{}'].manual.timeout_secs must be greater than 0", phase_id));
                }
            }

            if definition.agent_id.is_some() {
                return Err(anyhow!("phases['{}'] mode 'manual' must not include agent_id", phase_id));
            }

            if definition.command.is_some() {
                return Err(anyhow!("phases['{}'] mode 'manual' must not include command block", phase_id));
            }
        }
    }

    if let Some(runtime) = definition.runtime.as_ref() {
        if runtime.tool.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("phases['{}'].runtime.tool must not be empty", phase_id));
        }

        if runtime.tool_profile.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("phases['{}'].runtime.tool_profile must not be empty", phase_id));
        }

        if runtime.model.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("phases['{}'].runtime.model must not be empty", phase_id));
        }

        if runtime.fallback_models.iter().any(|value| value.trim().is_empty()) {
            return Err(anyhow!("phases['{}'].runtime.fallback_models must not contain empty values", phase_id));
        }

        if runtime.max_attempts == Some(0) {
            return Err(anyhow!("phases['{}'].runtime.max_attempts must be greater than 0", phase_id));
        }

        if runtime.timeout_secs == Some(0) {
            return Err(anyhow!("phases['{}'].runtime.timeout_secs must be greater than 0 when set", phase_id));
        }

        if runtime.extra_args.iter().any(|value| value.trim().is_empty()) {
            return Err(anyhow!("phases['{}'].runtime.extra_args must not contain empty values", phase_id));
        }

        if runtime.codex_config_overrides.iter().any(|value| !is_valid_codex_config_override(value.trim())) {
            return Err(anyhow!(
                "phases['{}'].runtime.codex_config_overrides values must use key=value syntax",
                phase_id
            ));
        }

        if runtime.reasoning_effort.as_deref().is_some_and(|value| !is_valid_reasoning_effort(value)) {
            return Err(anyhow!(
                "phases['{}'].runtime.reasoning_effort must be one of low, medium, high (got '{}')",
                phase_id,
                runtime.reasoning_effort.as_deref().unwrap_or_default()
            ));
        }

        if runtime.permission_mode.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("phases['{}'].runtime.permission_mode must not be empty when set", phase_id));
        }
    }

    if let Some(evals) = definition.evals.as_ref() {
        validate_evals_block_runtime(phase_id, evals, config)?;
    }

    Ok(())
}

// Codex round-7 P2: the same eval surface lives on `AgentRuntimeConfig`
// phases (overlay path), so the workflow-level validator is not enough.
// This runtime-side validator catches the structural issues (empty checks,
// threshold range, kind/field consistency, rework budget) without touching
// the workflow tools_allowlist.
fn validate_evals_block_runtime(phase_id: &str, evals: &EvalsConfig, config: &AgentRuntimeConfig) -> Result<()> {
    if !(0.0..=1.0).contains(&evals.pass_threshold) || !evals.pass_threshold.is_finite() {
        return Err(anyhow!("phases['{}'].evals.pass_threshold must be between 0.0 and 1.0", phase_id));
    }
    if evals.checks.is_empty() {
        return Err(anyhow!("phases['{}'].evals must declare at least one check", phase_id));
    }
    if evals.on_fail == EvalOnFail::Rework && evals.max_reworks == 0 {
        return Err(anyhow!("phases['{}'].evals.on_fail='rework' requires max_reworks > 0", phase_id));
    }
    let mut seen_ids = std::collections::BTreeSet::new();
    for check in &evals.checks {
        let trimmed = check.id.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("phases['{}'].evals.checks contains an empty check id", phase_id));
        }
        if !seen_ids.insert(trimmed.to_ascii_lowercase()) {
            return Err(anyhow!("phases['{}'].evals.checks contains duplicate id '{}'", phase_id, trimmed));
        }
        match check.kind {
            EvalKind::Command => {
                let program = check.command.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| {
                    anyhow!(
                        "phases['{}'].evals.checks['{}'] kind='command' requires a non-empty command field",
                        phase_id,
                        check.id
                    )
                })?;
                if !config.tools_allowlist.is_empty()
                    && !config.tools_allowlist.iter().any(|t| t.eq_ignore_ascii_case(program))
                {
                    return Err(anyhow!(
                        "phases['{}'].evals.checks['{}'].command '{}' is not in tools_allowlist",
                        phase_id,
                        check.id,
                        program
                    ));
                }
                if check.agent.is_some() || check.prompt.is_some() {
                    return Err(anyhow!(
                        "phases['{}'].evals.checks['{}'] kind='command' must not declare agent/prompt",
                        phase_id,
                        check.id
                    ));
                }
                if check.timeout_secs == Some(0) {
                    return Err(anyhow!(
                        "phases['{}'].evals.checks['{}'].timeout_secs must be greater than 0",
                        phase_id,
                        check.id
                    ));
                }
            }
            EvalKind::LlmJudge => {
                let agent_id = check.agent.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| {
                    anyhow!(
                        "phases['{}'].evals.checks['{}'] kind='llm_judge' requires a non-empty agent field",
                        phase_id,
                        check.id
                    )
                })?;
                if lookup_case_insensitive(&config.agents, agent_id).is_none() {
                    return Err(anyhow!(
                        "phases['{}'].evals.checks['{}'] references unknown agent '{}'",
                        phase_id,
                        check.id,
                        agent_id
                    ));
                }
                if check.prompt.as_deref().is_none_or(|s| s.trim().is_empty()) {
                    return Err(anyhow!(
                        "phases['{}'].evals.checks['{}'] kind='llm_judge' requires a non-empty prompt field",
                        phase_id,
                        check.id
                    ));
                }
                if check.command.is_some() || !check.args.is_empty() {
                    return Err(anyhow!(
                        "phases['{}'].evals.checks['{}'] kind='llm_judge' must not declare command/args",
                        phase_id,
                        check.id
                    ));
                }
                // Codex round-9 P3: working_dir + expected_exit are
                // command-only knobs the judge runner never consumes.
                if check.working_dir.is_some() {
                    return Err(anyhow!(
                        "phases['{}'].evals.checks['{}'] kind='llm_judge' must not declare working_dir",
                        phase_id,
                        check.id
                    ));
                }
                if check.expected_exit != default_eval_expected_exit() {
                    return Err(anyhow!(
                        "phases['{}'].evals.checks['{}'] kind='llm_judge' must not override expected_exit",
                        phase_id,
                        check.id
                    ));
                }
                // Codex round-7 P2: judge dispatch is one-shot through the
                // session backend; the runner has no per-call timeout
                // override surface today. Rejecting timeout_secs here keeps
                // the operator from being misled into thinking it will be
                // honoured.
                if check.timeout_secs.is_some() {
                    return Err(anyhow!(
                        "phases['{}'].evals.checks['{}'] kind='llm_judge' does not support timeout_secs (judge timeouts inherit from the agent profile)",
                        phase_id, check.id
                    ));
                }
            }
        }
    }
    Ok(())
}

fn backfill_agent_system_prompts(config: &mut AgentRuntimeConfig) {
    const DEFAULT_SYSTEM_PROMPT: &str =
        "You are the workflow phase execution agent. Produce deterministic, repository-safe outputs and keep changes scoped to the active phase.";
    for profile in config.agents.values_mut() {
        if profile.system_prompt.trim().is_empty() {
            profile.system_prompt = DEFAULT_SYSTEM_PROMPT.to_string();
        }
    }
}

/// Reasoning/thinking effort levels accepted by the runtime config and the
/// `--reasoning-effort` CLI flag. Provider transports map these to their own
/// flags (codex `model_reasoning_effort`, claude `--effort`).
pub const REASONING_EFFORT_LEVELS: &[&str] = &["low", "medium", "high"];

fn is_valid_reasoning_effort(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    REASONING_EFFORT_LEVELS.contains(&normalized.as_str())
}

/// Union of the permission/approval modes the known provider CLIs accept.
/// The value is forwarded to the provider VERBATIM — this list exists only
/// to warn on likely typos, never to block:
///
/// - claude `--permission-mode`: `default`, `acceptEdits`,
///   `bypassPermissions`, `plan`
/// - codex `-c approval_policy`: `untrusted`, `on-failure`, `on-request`,
///   `never`
/// - gemini `--approval-mode`: `default`, `auto_edit`, `yolo`
pub const KNOWN_PERMISSION_MODES: &[&str] = &[
    "default",
    "acceptEdits",
    "bypassPermissions",
    "plan",
    "untrusted",
    "on-failure",
    "on-request",
    "never",
    "auto_edit",
    "yolo",
];

/// Whether `value` is in the union of permission modes any known provider
/// accepts. Matching is case-insensitive so a casing slip does not trip the
/// typo warning; the original casing still rides the wire untouched.
pub fn is_known_permission_mode(value: &str) -> bool {
    let trimmed = value.trim();
    KNOWN_PERMISSION_MODES.iter().any(|known| known.eq_ignore_ascii_case(trimmed))
}

/// Validate an author-controlled `hooks` block: author policy rules must carry
/// valid matcher regexes (so a malformed guardrail is caught at config-load,
/// not silently fail-closed at session spawn), and every observer must name at
/// least one event. The author surface deliberately cannot express an
/// arbitrary command, so there is nothing shell-shaped to validate.
/// Public wrapper over [`validate_agent_hooks`] so workflow-YAML overlay
/// validation can check author `hooks` blocks with the same rules the
/// agent-runtime config uses (the overlay path does not go through
/// [`validate_agent_runtime_config`]).
pub fn validate_agent_hooks_block(agent_id: &str, hooks: &AgentHooksConfig) -> Result<()> {
    validate_agent_hooks(agent_id, hooks)
}

fn validate_agent_hooks(agent_id: &str, hooks: &AgentHooksConfig) -> Result<()> {
    // Reuse the kernel policy validator so author matcher regexes are checked
    // with the exact engine that evaluates them at session time. A malformed
    // guardrail is rejected at config-load instead of silently fail-closing
    // the whole session later.
    let probe = protocol::hook_policy::HookPolicy {
        version: protocol::hook_policy::HOOK_POLICY_VERSION,
        default_decision: protocol::hook_policy::PolicyDecision::Defer,
        rules: hooks.policy_rules.clone(),
    };
    probe
        .validate(&format!("agents['{agent_id}'].hooks.policy_rules"))
        .map_err(|err| anyhow!("agents['{}'].hooks.policy_rules are invalid: {}", agent_id, err))?;
    for (index, observer) in hooks.observers.iter().enumerate() {
        if observer.events.iter().all(|event| event.trim().is_empty()) {
            return Err(anyhow!(
                "agents['{}'].hooks.observers[{}] must name at least one harness event",
                agent_id,
                index
            ));
        }
    }
    Ok(())
}

fn validate_agent_runtime_config(config: &AgentRuntimeConfig) -> Result<()> {
    fn is_valid_codex_config_override(value: &str) -> bool {
        let Some((key, expr)) = value.split_once('=') else {
            return false;
        };
        !key.trim().is_empty() && !expr.trim().is_empty()
    }

    if config.schema.trim() != AGENT_RUNTIME_CONFIG_SCHEMA_ID {
        return Err(anyhow!("schema must be '{}' (got '{}')", AGENT_RUNTIME_CONFIG_SCHEMA_ID, config.schema));
    }

    if config.version != AGENT_RUNTIME_CONFIG_VERSION {
        return Err(anyhow!("version must be {} (got {})", AGENT_RUNTIME_CONFIG_VERSION, config.version));
    }

    if config.tools_allowlist.is_empty() || config.tools_allowlist.iter().all(|tool| tool.trim().is_empty()) {
        return Err(anyhow!("tools_allowlist must include at least one non-empty command"));
    }

    if config.agents.is_empty() {
        return Err(anyhow!("agents must include at least one profile"));
    }

    for (agent_id, profile) in &config.agents {
        if agent_id.trim().is_empty() {
            return Err(anyhow!("agents contains empty agent id"));
        }

        if profile.system_prompt.trim().is_empty() {
            return Err(anyhow!("agents['{}'].system_prompt must not be empty", agent_id));
        }

        if profile.system_prompt_file.is_some() {
            return Err(anyhow!(
                "agents['{}'].system_prompt_file is only valid in source YAML; the compiled runtime config must not contain it",
                agent_id
            ));
        }

        if profile.tool.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].tool must not be empty", agent_id));
        }

        if profile.tool_profile.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].tool_profile must not be empty", agent_id));
        }

        if profile.model.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].model must not be empty", agent_id));
        }

        if profile.fallback_models.iter().any(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].fallback_models must not contain empty values", agent_id));
        }

        if profile.max_attempts == Some(0) {
            return Err(anyhow!("agents['{}'].max_attempts must be greater than 0", agent_id));
        }

        if profile.timeout_secs == Some(0) {
            return Err(anyhow!("agents['{}'].timeout_secs must be greater than 0 when set", agent_id));
        }

        if profile.extra_args.iter().any(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].extra_args must not contain empty values", agent_id));
        }

        if profile.codex_config_overrides.iter().any(|value| !is_valid_codex_config_override(value.trim())) {
            return Err(anyhow!("agents['{}'].codex_config_overrides values must use key=value syntax", agent_id));
        }

        if profile.reasoning_effort.as_deref().is_some_and(|value| !is_valid_reasoning_effort(value)) {
            return Err(anyhow!(
                "agents['{}'].reasoning_effort must be one of low, medium, high (got '{}')",
                agent_id,
                profile.reasoning_effort.as_deref().unwrap_or_default()
            ));
        }

        if profile.permission_mode.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].permission_mode must not be empty when set", agent_id));
        }

        if profile.role.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].role must not be empty", agent_id));
        }

        if profile.name.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].name must not be empty", agent_id));
        }

        if let Some(persona) = profile.persona.as_ref() {
            if persona.style.as_deref().is_some_and(|value| value.trim().is_empty()) {
                return Err(anyhow!("agents['{}'].persona.style must not be empty", agent_id));
            }
            if persona.instructions.as_deref().is_some_and(|value| value.trim().is_empty()) {
                return Err(anyhow!("agents['{}'].persona.instructions must not be empty", agent_id));
            }
            if persona.traits.iter().any(|value| value.trim().is_empty()) {
                return Err(anyhow!("agents['{}'].persona.traits must not contain empty values", agent_id));
            }
            if persona.customizations.keys().any(|value| value.trim().is_empty()) {
                return Err(anyhow!("agents['{}'].persona.customizations must not contain empty keys", agent_id));
            }
        }

        if profile.memory.scope.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].memory.scope must not be empty", agent_id));
        }
        if profile.memory.max_context_chars == Some(0) {
            return Err(anyhow!("agents['{}'].memory.max_context_chars must be greater than 0", agent_id));
        }
        if profile.memory.max_entries == Some(0) {
            return Err(anyhow!("agents['{}'].memory.max_entries must be greater than 0", agent_id));
        }

        if profile.communication.max_context_chars == Some(0) {
            return Err(anyhow!("agents['{}'].communication.max_context_chars must be greater than 0", agent_id));
        }
        if profile.communication.channels.iter().any(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].communication.channels must not contain empty values", agent_id));
        }
        if profile.communication.can_message.iter().any(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].communication.can_message must not contain empty values", agent_id));
        }

        if profile.mcp_servers.iter().any(|server| server.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].mcp_servers must not contain empty values", agent_id));
        }

        if profile.tool_policy.allow.iter().chain(profile.tool_policy.deny.iter()).any(|value| value.trim().is_empty())
        {
            return Err(anyhow!("agents['{}'].tool_policy must not contain empty patterns", agent_id));
        }

        validate_agent_hooks(agent_id, &profile.hooks)?;

        if profile.skills.iter().any(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].skills must not contain empty values", agent_id));
        }

        if profile.capabilities.keys().any(|capability| capability.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].capabilities must not contain empty capability keys", agent_id));
        }
    }

    if config.phases.is_empty() {
        return Err(anyhow!("phases must include at least one phase definition"));
    }

    for (phase_id, definition) in &config.phases {
        if phase_id.trim().is_empty() {
            return Err(anyhow!("phases contains empty phase id"));
        }
        validate_phase_definition(phase_id, definition, config)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{env_lock, EnvVarGuard};
    use std::fs;

    #[test]
    fn approval_policy_deny_wins_over_allow_and_default_applies() {
        let policy = ApprovalPolicy {
            auto_allow: vec!["git.*".to_string(), "cargo test".to_string()],
            auto_deny: vec!["git.push*".to_string(), "*prod*".to_string()],
            default: ApprovalPolicyDefault::Ask,
        };
        assert_eq!(policy.evaluate("git.commit"), ApprovalPolicyDecision::Allow);
        assert_eq!(policy.evaluate("git.push --force"), ApprovalPolicyDecision::Deny);
        assert_eq!(policy.evaluate("cargo test"), ApprovalPolicyDecision::Allow);
        assert_eq!(policy.evaluate("deploy to prod cluster"), ApprovalPolicyDecision::Deny);
        assert_eq!(policy.evaluate("rm -rf node_modules"), ApprovalPolicyDecision::Ask);

        let allow_by_default = ApprovalPolicy { default: ApprovalPolicyDefault::Allow, ..Default::default() };
        assert_eq!(allow_by_default.evaluate("anything"), ApprovalPolicyDecision::Allow);
        let deny_by_default = ApprovalPolicy { default: ApprovalPolicyDefault::Deny, ..Default::default() };
        assert_eq!(deny_by_default.evaluate("anything"), ApprovalPolicyDecision::Deny);
    }

    #[test]
    fn approval_policy_round_trips_through_agent_overlay() {
        let overlay: AgentProfileOverlay = serde_json::from_value(json!({
            "approval_policy": {
                "auto_allow": ["task.*"],
                "auto_deny": ["daemon.stop"],
                "default": "deny"
            }
        }))
        .expect("overlay parses");
        let policy = overlay.approval_policy.clone().expect("approval policy present");
        assert_eq!(policy.auto_allow, vec!["task.*".to_string()]);
        assert_eq!(policy.auto_deny, vec!["daemon.stop".to_string()]);
        assert_eq!(policy.default, ApprovalPolicyDefault::Deny);

        let mut base = AgentProfile::default();
        assert!(base.approval_policy.is_none());
        merge_agent_profile(&mut base, &overlay);
        assert_eq!(base.approval_policy.as_ref(), Some(&policy));

        let round_tripped = AgentProfileOverlay::from(base.clone());
        assert_eq!(round_tripped.approval_policy.as_ref(), Some(&policy));

        let serialized = serde_json::to_value(&base).expect("profile serializes");
        assert_eq!(serialized.pointer("/approval_policy/default").and_then(Value::as_str), Some("deny"));
    }

    #[test]
    fn agent_profile_without_hooks_block_loads_unchanged() {
        // Back-compat: a profile with no `hooks` key deserializes to an empty
        // AgentHooksConfig and serializes without re-emitting the key.
        let profile: AgentProfile = serde_json::from_value(json!({
            "description": "legacy",
            "system_prompt": "p"
        }))
        .expect("legacy profile parses");
        assert!(profile.hooks.is_empty());
        let serialized = serde_json::to_value(&profile).expect("serializes");
        assert!(serialized.get("hooks").is_none(), "empty hooks block is skipped on serialize");
    }

    #[test]
    fn agent_hooks_round_trip_through_overlay_and_merge() {
        let overlay: AgentProfileOverlay = serde_json::from_value(json!({
            "hooks": {
                "policy_rules": [{
                    "id": "no-prod",
                    "tools": ["Bash"],
                    "input_matchers": [{"field": "command", "regex": "--env prod"}],
                    "decision": "deny",
                    "reason": "prod gated"
                }],
                "observers": [{ "events": ["PostToolUse"], "action": "record" }]
            }
        }))
        .expect("overlay parses");
        let hooks = overlay.hooks.clone().expect("hooks present");
        assert_eq!(hooks.policy_rules.len(), 1);
        assert_eq!(hooks.policy_rules[0].decision, protocol::hook_policy::PolicyDecision::Deny);
        assert_eq!(hooks.observers.len(), 1);
        assert_eq!(hooks.observers[0].action, AgentHookAction::Record);

        let mut base = AgentProfile::default();
        assert!(base.hooks.is_empty());
        merge_agent_profile(&mut base, &overlay);
        assert_eq!(base.hooks, hooks);

        let round_tripped = AgentProfileOverlay::from(base.clone());
        assert_eq!(round_tripped.hooks.as_ref(), Some(&hooks));
    }

    #[test]
    fn validate_agent_hooks_rejects_bad_regex_and_empty_observer_events() {
        let bad_regex = AgentHooksConfig {
            policy_rules: vec![protocol::hook_policy::HookPolicyRule {
                id: Some("bad".to_string()),
                events: vec![],
                tools: vec![],
                input_matchers: vec![protocol::hook_policy::InputMatcher {
                    field: "command".to_string(),
                    regex: "(".to_string(),
                }],
                decision: protocol::hook_policy::PolicyDecision::Deny,
                reason: None,
            }],
            observers: vec![],
        };
        assert!(validate_agent_hooks("a", &bad_regex).is_err(), "invalid regex rejected");

        let empty_observer = AgentHooksConfig {
            policy_rules: vec![],
            observers: vec![AgentHookObserver { events: vec![], action: AgentHookAction::Record }],
        };
        assert!(validate_agent_hooks("a", &empty_observer).is_err(), "observer with no events rejected");

        let ok = AgentHooksConfig {
            policy_rules: vec![],
            observers: vec![AgentHookObserver { events: vec!["Stop".to_string()], action: AgentHookAction::Record }],
        };
        assert!(validate_agent_hooks("a", &ok).is_ok());
    }

    #[test]
    fn workflow_tool_redeclare_with_executable_only_preserves_capabilities() {
        let mut base = builtin_agent_runtime_config();
        base.cli_tools.insert(
            "claude".to_string(),
            CliToolConfig {
                executable: Some("claude".to_string()),
                supports_file_editing: Some(true),
                supports_streaming: Some(true),
                supports_tool_use: Some(true),
                supports_vision: Some(true),
                supports_long_context: Some(true),
                max_context_tokens: Some(200_000),
                supports_mcp: Some(true),
                read_only_flag: Some("--read-only".to_string()),
                response_schema_flag: None,
            },
        );
        let mut workflow = crate::workflow_config::builtin_workflow_config();
        workflow.tools.insert(
            "claude".to_string(),
            crate::workflow_config::ToolDefinition {
                executable: "claude-custom".to_string(),
                supports_mcp: None,
                supports_write: None,
                context_window: None,
                base_args: vec![],
                supports_streaming: None,
                supports_tool_use: None,
                supports_vision: None,
                supports_long_context: None,
                read_only_flag: None,
                response_schema_flag: None,
            },
        );

        merge_workflow_runtime_overlay(&mut base, &workflow);

        let tool = base.cli_tools.get("claude").expect("tool present");
        assert_eq!(tool.executable.as_deref(), Some("claude-custom"));
        assert_eq!(tool.max_context_tokens, Some(200_000), "omitted context_window must not wipe the prior value");
        assert_eq!(tool.supports_mcp, Some(true), "omitted supports_mcp must not flip to false");
        assert_eq!(tool.supports_file_editing, Some(true), "omitted supports_write must not flip to false");
    }

    #[test]
    fn phase_execution_prefers_exact_key_and_falls_back_to_default_case_insensitively() {
        let mut config = builtin_agent_runtime_config();
        let template = config.phases.values().next().expect("builtin has phases").clone();
        config.phases.clear();
        let mut exact = template.clone();
        exact.agent_id = Some("exact-agent".to_string());
        let mut cased = template.clone();
        cased.agent_id = Some("cased-agent".to_string());
        let mut fallback = template;
        fallback.agent_id = Some("default-agent".to_string());
        config.phases.insert("Build".to_string(), cased);
        config.phases.insert("build".to_string(), exact);
        config.phases.insert("Default".to_string(), fallback);

        let resolved = config.phase_execution("build").expect("phase resolves");
        assert_eq!(resolved.agent_id.as_deref(), Some("exact-agent"), "exact key must win over case-insensitive");
        let fallback = config.phase_execution("nonexistent").expect("default fallback resolves");
        assert_eq!(fallback.agent_id.as_deref(), Some("default-agent"), "default fallback must be case-insensitive");
    }

    fn write_pack_agent_overlay_fixture(root: &std::path::Path, pack_id: &str, version: &str) {
        fs::create_dir_all(root.join("workflows")).expect("create workflows");
        fs::create_dir_all(root.join("runtime")).expect("create runtime");
        fs::create_dir_all(root.join("assets")).expect("create assets");
        fs::write(root.join("assets/review-helper.sh"), "#!/bin/sh\nexit 0\n").expect("write helper");
        fs::write(
            root.join(crate::PACK_MANIFEST_FILE_NAME),
            format!(
                r#"
schema = "animus.pack.v1"
id = "{pack_id}"
version = "{version}"
kind = "domain-pack"
title = "{pack_id}"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
animus_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["animus.task"]
default_kind = "animus.task"

[workflows]
root = "workflows"
exports = ["{pack_id}/cycle"]

[runtime]
agent_overlay = "runtime/agent-runtime.overlay.yaml"
workflow_overlay = "runtime/workflow-runtime.overlay.yaml"

[permissions]
tools = ["review_helper"]
"#
            ),
        )
        .expect("write manifest");
        fs::write(
            root.join("runtime/workflow-runtime.overlay.yaml"),
            format!(
                r#"
phase_catalog:
  code-review:
    label: Code Review
    description: Review implementation quality, correctness, and maintainability.
    category: review
    tags: ["review", "code", "fixture"]
  testing:
    label: Testing
    description: Validate the implementation by running or inspecting the relevant test suite.
    category: verification
    tags: ["testing", "verification", "fixture"]
  po-review:
    label: PO Review
    description: Validate delivered work against product intent and acceptance criteria.
    category: review
    tags: ["review", "acceptance", "fixture"]
  unit-test:
    label: Unit Test
    description: Run the workspace test suite as a deterministic gate.
    category: verification
    tags: ["testing", "gate", "fixture"]
  lint:
    label: Lint
    description: Run the linter as a deterministic gate.
    category: verification
    tags: ["lint", "gate", "fixture"]

workflows:
  - id: {pack_id}/cycle
    name: "{pack_id}/cycle"
    phases:
      - code-review:
          on_verdict:
            rework:
              target: code-review
      - testing
  - id: builtin/review-cycle
    name: "builtin/review-cycle"
    phases:
      - workflow_ref: {pack_id}/cycle
"#,
            ),
        )
        .expect("write workflow overlay");
        fs::write(
            root.join("runtime/agent-runtime.overlay.yaml"),
            r#"
tools_allowlist:
  - review_helper
phases:
  pack-review:
    mode: command
    command:
      program: ./assets/review-helper.sh
      cwd_mode: path
      cwd_path: workspace/scripts
cli_tools:
  pack-tool:
    executable: ./assets/review-helper.sh
    supports_mcp: false
    supports_file_editing: false
"#,
        )
        .expect("write agent runtime overlay");
    }

    #[test]
    fn empty_project_loads_bundled_pack_runtime_defaults() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let temp = tempfile::tempdir().expect("tempdir");
        let config = load_agent_runtime_config_or_default(temp.path());

        // Verify builtin phases are present with expected agent IDs
        assert_eq!(config.phase_agent_id("requirements"), Some("po"));
        assert_eq!(config.phase_agent_id("implementation"), Some("swe"));
    }

    #[test]
    fn ensure_creates_config_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        ensure_agent_runtime_config_file(temp.path()).expect("ensure file");

        let workflows_dir = crate::workflow_config::yaml_workflows_dir(temp.path());
        assert!(workflows_dir.join("custom.yaml").exists());
        assert!(workflows_dir.join("standard-workflow.yaml").exists());
        assert!(workflows_dir.join("hotfix-workflow.yaml").exists());
        assert!(workflows_dir.join("research-workflow.yaml").exists());
    }

    #[test]
    fn runtime_resolution_merges_workflow_config_overlays() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow = crate::workflow_config::builtin_workflow_config();
        let builtin = builtin_agent_runtime_config();
        let mut overlay_agent = builtin.agent_profile("po").expect("builtin po profile should exist").clone();
        overlay_agent.mcp_servers.clear();
        overlay_agent.skills.clear();
        workflow.agent_profiles.insert("workflow-test-agent".to_string(), overlay_agent.into());
        workflow.phase_definitions.insert(
            "workflow-test-phase".to_string(),
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Agent,
                agent_id: Some("workflow-test-agent".to_string()),
                directive: Some("workflow test".to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: Some(PhaseDecisionContract {
                    required_evidence: Vec::new(),
                    min_confidence: 0.7,
                    max_risk: crate::types::WorkflowDecisionRisk::Medium,
                    allow_missing_decision: false,
                    extra_json_schema: None,
                    fields: BTreeMap::new(),
                }),
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        workflow.tools.insert(
            "custom-runner".to_string(),
            crate::workflow_config::ToolDefinition {
                executable: "custom-runner-bin".to_string(),
                supports_mcp: Some(true),
                supports_write: Some(true),
                context_window: Some(42_000),
                base_args: vec![],
                supports_streaming: None,
                supports_tool_use: None,
                supports_vision: None,
                supports_long_context: None,
                read_only_flag: None,
                response_schema_flag: None,
            },
        );
        crate::workflow_config::write_workflow_config(temp.path(), &workflow).expect("write workflow config");

        let resolved = load_agent_runtime_config_or_default(temp.path());
        let phase = resolved.phase_decision_contract("workflow-test-phase").expect("workflow phase contract");
        assert!(!phase.allow_missing_decision);
    }

    #[test]
    fn runtime_resolution_merges_pack_agent_overlays_and_rebases_assets() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let temp = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        write_pack_agent_overlay_fixture(
            &crate::machine_installed_packs_dir().join("animus.review").join("0.2.0"),
            "animus.review",
            "0.2.0",
        );

        crate::workflow_config::load_workflow_config_with_metadata(temp.path()).expect("workflow config should load");
        let resolved = load_agent_runtime_config_with_metadata(temp.path()).expect("load runtime config");
        let command = resolved.config.phase_command("pack-review").expect("pack review command");
        let tool = resolved.config.cli_tools.get("pack-tool").expect("pack tool");

        assert!(command.program.ends_with("assets/review-helper.sh"));
        assert_eq!(command.cwd_mode, CommandCwdMode::Path);
        assert_eq!(command.cwd_path.as_deref(), Some("workspace/scripts"));
        assert_eq!(tool.executable.as_deref().is_some_and(|value| value.ends_with("assets/review-helper.sh")), true);
    }

    #[test]
    fn runtime_resolution_prefers_project_workflow_over_installed_pack_runtime_overlay() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let temp = tempfile::tempdir().expect("project tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        write_pack_agent_overlay_fixture(
            &crate::machine_installed_packs_dir().join("animus.review").join("0.2.0"),
            "animus.review",
            "0.2.0",
        );

        let mut workflow = crate::workflow_config::builtin_workflow_config();
        workflow.phase_catalog.insert(
            "pack-review".to_string(),
            crate::workflow_config::PhaseUiDefinition {
                label: "Pack Review".to_string(),
                description: "Project override".to_string(),
                category: "verification".to_string(),
                icon: None,
                docs_url: None,
                tags: vec!["override".to_string()],
                visible: true,
            },
        );
        let mut project_agent = builtin_agent_runtime_config().agent_profile("po").expect("builtin po profile").clone();
        project_agent.description = "Project agent".to_string();
        project_agent.system_prompt = "Project prompt".to_string();
        project_agent.mcp_servers.clear();
        project_agent.skills.clear();
        project_agent.capabilities.clear();
        workflow.agent_profiles.insert("project-agent".to_string(), project_agent.into());
        workflow.phase_definitions.insert(
            "pack-review".to_string(),
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Agent,
                agent_id: Some("project-agent".to_string()),
                directive: Some("project override".to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        workflow.workflows.push(crate::workflow_config::WorkflowDefinition {
            id: "project-override-check".to_string(),
            name: "Project Override Check".to_string(),
            description: String::new(),
            phases: vec!["pack-review".to_string().into()],
            variables: Vec::new(),
            worktree: None,
            budget: None,
        });
        crate::workflow_config::write_workflow_config(temp.path(), &workflow).expect("write workflow config");

        let resolved = load_agent_runtime_config_with_metadata(temp.path()).expect("load runtime config");
        let phase = resolved.config.phase_execution("pack-review").expect("pack-review phase should exist");
        assert_eq!(phase.mode, PhaseExecutionMode::Agent);
        assert_eq!(phase.agent_id.as_deref(), Some("project-agent"));
        assert!(phase.command.is_none(), "project workflow phase should override installed pack command phase");
    }

    #[test]
    fn builtin_defaults_expose_phase_definitions() {
        let config = builtin_agent_runtime_config();
        assert_eq!(config.phase_agent_id("requirements"), Some("po"));
        assert_eq!(config.phase_agent_id("implementation"), Some("swe"));
        assert!(!config.phases.contains_key("code-review"));
        assert!(!config.phases.contains_key("testing"));
        assert!(config.phase_output_json_schema("implementation").is_some());
    }

    #[test]
    fn builtin_phase_prompts_resolve_to_expected_personas() {
        let config = builtin_agent_runtime_config();
        for (phase_id, agent_id) in [("requirements", "po"), ("implementation", "swe")] {
            let expected_prompt = config
                .agent_profile(agent_id)
                .expect("phase agent profile should exist")
                .system_prompt
                .trim()
                .to_string();
            assert_eq!(config.phase_agent_id(phase_id), Some(agent_id));
            assert_eq!(config.phase_system_prompt(phase_id), Some(expected_prompt.as_str()));
        }
    }

    #[test]
    fn builtin_phase_decision_contracts_match_expected_evidence_requirements() {
        let config = builtin_agent_runtime_config();

        assert_eq!(
            config.phase_decision_contract("requirements").map(|contract| contract.required_evidence.clone()),
            Some(Vec::new())
        );
        assert_eq!(
            config.phase_decision_contract("implementation").map(|contract| contract.required_evidence.clone()),
            Some(Vec::new())
        );
    }

    #[test]
    fn builtin_defaults_include_em_po_and_swe_profiles() {
        let config = builtin_agent_runtime_config();
        for agent_id in ["em", "po", "swe"] {
            let profile = config.agent_profile(agent_id).expect("builtin profile should exist");
            assert!(!profile.description.trim().is_empty());
            assert!(!profile.system_prompt.trim().is_empty());
            assert!(profile.role.as_deref().is_some_and(|role| !role.is_empty()));
            assert!(!profile.capabilities.is_empty());
            assert!(!profile.mcp_servers.is_empty());
        }
    }

    #[test]
    fn builtin_persona_capabilities_and_tool_patterns_are_role_specific() {
        let config = builtin_agent_runtime_config();
        let em = config.agent_profile("em").expect("em profile should exist");
        let po = config.agent_profile("po").expect("po profile should exist");
        let swe = config.agent_profile("swe").expect("swe profile should exist");

        assert_eq!(em.capabilities.get("queue_management"), Some(&true));
        assert_eq!(em.capabilities.get("scheduling"), Some(&true));
        assert_eq!(em.capabilities.get("implementation"), Some(&false));

        assert_eq!(po.capabilities.get("requirements_authoring"), Some(&true));
        assert_eq!(po.capabilities.get("acceptance_validation"), Some(&true));
        assert_eq!(po.capabilities.get("implementation"), Some(&false));

        assert_eq!(swe.capabilities.get("implementation"), Some(&true));
        assert_eq!(swe.capabilities.get("testing"), Some(&true));
        assert_eq!(swe.capabilities.get("code_review"), Some(&true));
        assert_eq!(swe.capabilities.get("planning"), Some(&false));

        assert!(em.mcp_servers.iter().any(|server| server == "animus"));
        assert!(po.mcp_servers.iter().any(|server| server == "animus"));
        assert!(swe.mcp_servers.iter().any(|server| server == "animus"));
    }

    #[test]
    fn builtin_json_and_fallback_match_persona_phase_defaults() {
        let from_json = serde_json::from_str::<AgentRuntimeConfig>(BUILTIN_AGENT_RUNTIME_CONFIG_JSON)
            .expect("builtin json should deserialize");
        validate_agent_runtime_config(&from_json).expect("builtin json should validate");
        let fallback = hardcoded_builtin_agent_runtime_config();

        for phase_id in ["requirements", "implementation"] {
            assert_eq!(from_json.phase_agent_id(phase_id), fallback.phase_agent_id(phase_id));
            assert_eq!(
                from_json.phase_decision_contract(phase_id).map(|contract| (
                    contract.required_evidence.clone(),
                    contract.min_confidence,
                    contract.max_risk.clone(),
                    contract.allow_missing_decision,
                    contract.extra_json_schema.clone()
                )),
                fallback.phase_decision_contract(phase_id).map(|contract| (
                    contract.required_evidence.clone(),
                    contract.min_confidence,
                    contract.max_risk.clone(),
                    contract.allow_missing_decision,
                    contract.extra_json_schema.clone()
                ))
            );
        }

        for agent_id in ["em", "po", "swe"] {
            let json_profile = from_json.agent_profile(agent_id).expect("json profile should exist");
            let fallback_profile = fallback.agent_profile(agent_id).expect("fallback profile should exist");
            assert_eq!(json_profile.role, fallback_profile.role);
            assert_eq!(json_profile.mcp_servers, fallback_profile.mcp_servers);
            assert_eq!(json_profile.tool_policy, fallback_profile.tool_policy);
            assert_eq!(json_profile.skills, fallback_profile.skills);
            assert_eq!(json_profile.capabilities, fallback_profile.capabilities);
        }
    }

    #[test]
    fn phase_decision_contract_lookup_is_case_insensitive() {
        let config = builtin_agent_runtime_config();
        assert!(config.phase_decision_contract("implementation").is_some());
        assert!(config.phase_decision_contract("IMPLEMENTATION").is_some());
    }

    #[test]
    fn builtin_defaults_mark_review_as_structured_output() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let temp = tempfile::tempdir().expect("tempdir");
        let config = load_agent_runtime_config_or_default(temp.path());
        // Verify that structured output phases are marked correctly
        assert!(config.is_structured_output_phase("implementation"));
    }

    #[test]
    fn structured_output_phase_accepts_trimmed_phase_ids() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let temp = tempfile::tempdir().expect("tempdir");
        let config = load_agent_runtime_config_or_default(temp.path());
        // Verify that phase IDs are trimmed and case-insensitive
        assert!(config.is_structured_output_phase(" implementation "));
        assert!(config.is_structured_output_phase(" IMPLEMENTATION "));
    }

    #[test]
    fn bundled_pack_runtime_supports_extended_task_requirement_and_review_phases() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let temp = tempfile::tempdir().expect("tempdir");
        let config = load_agent_runtime_config_or_default(temp.path());

        // Verify builtin phases are available
        assert_eq!(config.phase_agent_id("requirements"), Some("po"));
        assert_eq!(config.phase_agent_id("implementation"), Some("swe"));
    }

    #[test]
    fn structured_output_phase_rejects_empty_phase_even_with_structured_default() {
        let mut config = builtin_agent_runtime_config();
        let default_phase = config.phases.get_mut("default").expect("builtin config includes default phase");
        default_phase.output_contract = Some(PhaseOutputContract {
            kind: "phase_result".to_string(),
            required_fields: Vec::new(),
            fields: BTreeMap::new(),
        });

        assert!(config.is_structured_output_phase("custom-phase"));
        assert!(!config.is_structured_output_phase("   "));
    }

    fn make_minimal_config_with_phase(phase_id: &str, definition: PhaseExecutionDefinition) -> AgentRuntimeConfig {
        let mut config = builtin_agent_runtime_config();
        config.phases.insert(phase_id.to_string(), definition);
        config
    }

    #[test]
    fn command_mode_phase_roundtrips_through_json() {
        let definition = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
            agent_id: None,
            directive: Some("Run cargo test".to_string()),
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: Some(PhaseCommandDefinition {
                program: "cargo".to_string(),
                args: vec!["test".to_string(), "--workspace".to_string()],
                env: BTreeMap::from([("RUST_LOG".to_string(), "info".to_string())]),
                cwd_mode: CommandCwdMode::ProjectRoot,
                cwd_path: None,
                timeout_secs: Some(300),
                success_exit_codes: vec![0],
                parse_json_output: false,
                expected_result_kind: None,
                expected_schema: None,
                category: None,
                failure_pattern: None,
                excerpt_max_chars: None,
                on_success_verdict: None,
                on_failure_verdict: None,
                confidence: None,
                failure_risk: None,
            }),
            manual: None,
            default_tool: None,
            idempotency: Idempotency::Unknown,
            worktree: None,
            evals: None,
        };

        let json = serde_json::to_string(&definition).expect("serialize");
        let restored: PhaseExecutionDefinition = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.mode, PhaseExecutionMode::Command);
        assert!(restored.agent_id.is_none());
        let cmd = restored.command.expect("command block present");
        assert_eq!(cmd.program, "cargo");
        assert_eq!(cmd.args, vec!["test", "--workspace"]);
        assert_eq!(cmd.timeout_secs, Some(300));
        assert_eq!(cmd.success_exit_codes, vec![0]);
        assert!(!cmd.parse_json_output);
    }

    #[test]
    fn command_mode_phase_validates_successfully() {
        let config = make_minimal_config_with_phase(
            "lint",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: None,
                directive: Some("Run linter".to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: Some(PhaseCommandDefinition {
                    program: "cargo".to_string(),
                    args: vec!["clippy".to_string()],
                    env: BTreeMap::new(),
                    cwd_mode: CommandCwdMode::ProjectRoot,
                    cwd_path: None,
                    timeout_secs: None,
                    success_exit_codes: vec![0],
                    parse_json_output: false,
                    expected_result_kind: None,
                    expected_schema: None,
                    category: None,
                    failure_pattern: None,
                    excerpt_max_chars: None,
                    on_success_verdict: None,
                    on_failure_verdict: None,
                    confidence: None,
                    failure_risk: None,
                }),
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        validate_agent_runtime_config(&config).expect("valid command-mode config");
    }

    #[test]
    fn command_mode_rejects_missing_command_block() {
        let config = make_minimal_config_with_phase(
            "lint",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: None,
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("requires command block"));
    }

    #[test]
    fn command_mode_rejects_empty_program() {
        let config = make_minimal_config_with_phase(
            "lint",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: None,
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: Some(PhaseCommandDefinition {
                    program: "  ".to_string(),
                    args: vec![],
                    env: BTreeMap::new(),
                    cwd_mode: CommandCwdMode::ProjectRoot,
                    cwd_path: None,
                    timeout_secs: None,
                    success_exit_codes: vec![0],
                    parse_json_output: false,
                    expected_result_kind: None,
                    expected_schema: None,
                    category: None,
                    failure_pattern: None,
                    excerpt_max_chars: None,
                    on_success_verdict: None,
                    on_failure_verdict: None,
                    confidence: None,
                    failure_risk: None,
                }),
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("program must not be empty"));
    }

    #[test]
    fn command_mode_rejects_agent_id() {
        let config = make_minimal_config_with_phase(
            "lint",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: Some("swe".to_string()),
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: Some(PhaseCommandDefinition {
                    program: "cargo".to_string(),
                    args: vec![],
                    env: BTreeMap::new(),
                    cwd_mode: CommandCwdMode::ProjectRoot,
                    cwd_path: None,
                    timeout_secs: None,
                    success_exit_codes: vec![0],
                    parse_json_output: false,
                    expected_result_kind: None,
                    expected_schema: None,
                    category: None,
                    failure_pattern: None,
                    excerpt_max_chars: None,
                    on_success_verdict: None,
                    on_failure_verdict: None,
                    confidence: None,
                    failure_risk: None,
                }),
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("must not include agent_id"));
    }

    #[test]
    fn command_mode_rejects_empty_success_exit_codes() {
        let config = make_minimal_config_with_phase(
            "lint",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: None,
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: Some(PhaseCommandDefinition {
                    program: "cargo".to_string(),
                    args: vec![],
                    env: BTreeMap::new(),
                    cwd_mode: CommandCwdMode::ProjectRoot,
                    cwd_path: None,
                    timeout_secs: None,
                    success_exit_codes: vec![],
                    parse_json_output: false,
                    expected_result_kind: None,
                    expected_schema: None,
                    category: None,
                    failure_pattern: None,
                    excerpt_max_chars: None,
                    on_success_verdict: None,
                    on_failure_verdict: None,
                    confidence: None,
                    failure_risk: None,
                }),
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("success_exit_codes must include at least one code"));
    }

    #[test]
    fn command_mode_cwd_path_required_for_path_mode() {
        let config = make_minimal_config_with_phase(
            "lint",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: None,
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: Some(PhaseCommandDefinition {
                    program: "cargo".to_string(),
                    args: vec![],
                    env: BTreeMap::new(),
                    cwd_mode: CommandCwdMode::Path,
                    cwd_path: None,
                    timeout_secs: None,
                    success_exit_codes: vec![0],
                    parse_json_output: false,
                    expected_result_kind: None,
                    expected_schema: None,
                    category: None,
                    failure_pattern: None,
                    excerpt_max_chars: None,
                    on_success_verdict: None,
                    on_failure_verdict: None,
                    confidence: None,
                    failure_risk: None,
                }),
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("cwd_path must be set"));
    }

    #[test]
    fn command_mode_rejects_manual_block() {
        let config = make_minimal_config_with_phase(
            "lint",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: None,
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: Some(PhaseCommandDefinition {
                    program: "cargo".to_string(),
                    args: vec![],
                    env: BTreeMap::new(),
                    cwd_mode: CommandCwdMode::ProjectRoot,
                    cwd_path: None,
                    timeout_secs: None,
                    success_exit_codes: vec![0],
                    parse_json_output: false,
                    expected_result_kind: None,
                    expected_schema: None,
                    category: None,
                    failure_pattern: None,
                    excerpt_max_chars: None,
                    on_success_verdict: None,
                    on_failure_verdict: None,
                    confidence: None,
                    failure_risk: None,
                }),
                manual: Some(PhaseManualDefinition {
                    instructions: "Wait for approval".to_string(),
                    approval_note_required: false,
                    timeout_secs: None,
                }),
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("must not include manual block"));
    }

    #[test]
    fn phase_mode_returns_command_for_command_phase() {
        let config = make_minimal_config_with_phase(
            "lint",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: None,
                directive: Some("Run linter".to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: Some(PhaseCommandDefinition {
                    program: "cargo".to_string(),
                    args: vec!["clippy".to_string()],
                    env: BTreeMap::new(),
                    cwd_mode: CommandCwdMode::ProjectRoot,
                    cwd_path: None,
                    timeout_secs: None,
                    success_exit_codes: vec![0],
                    parse_json_output: false,
                    expected_result_kind: None,
                    expected_schema: None,
                    category: None,
                    failure_pattern: None,
                    excerpt_max_chars: None,
                    on_success_verdict: None,
                    on_failure_verdict: None,
                    confidence: None,
                    failure_risk: None,
                }),
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        assert_eq!(config.phase_mode("lint"), Some(PhaseExecutionMode::Command));
        let cmd = config.phase_command("lint").expect("command block present");
        assert_eq!(cmd.program, "cargo");
        assert_eq!(cmd.args, vec!["clippy"]);
    }

    #[test]
    fn command_mode_with_json_output_parsing_roundtrips() {
        let definition = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
            agent_id: None,
            directive: None,
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: Some(PhaseCommandDefinition {
                program: "bash".to_string(),
                args: vec!["-c".to_string(), "echo '{\"kind\":\"test_result\",\"passed\":true}'".to_string()],
                env: BTreeMap::new(),
                cwd_mode: CommandCwdMode::TaskRoot,
                cwd_path: None,
                timeout_secs: Some(60),
                success_exit_codes: vec![0, 1],
                parse_json_output: true,
                expected_result_kind: Some("test_result".to_string()),
                expected_schema: Some(serde_json::json!({
                    "type": "object",
                    "required": ["kind", "passed"],
                    "properties": {
                        "kind": {"const": "test_result"},
                        "passed": {"type": "boolean"}
                    }
                })),
                category: None,
                failure_pattern: None,
                excerpt_max_chars: None,
                on_success_verdict: None,
                on_failure_verdict: None,
                confidence: None,
                failure_risk: None,
            }),
            manual: None,
            default_tool: None,
            idempotency: Idempotency::Unknown,
            worktree: None,
            evals: None,
        };

        let json = serde_json::to_string_pretty(&definition).expect("serialize");
        let restored: PhaseExecutionDefinition = serde_json::from_str(&json).expect("deserialize");

        let cmd = restored.command.expect("command present");
        assert!(cmd.parse_json_output);
        assert_eq!(cmd.expected_result_kind.as_deref(), Some("test_result"));
        assert!(cmd.expected_schema.is_some());
        assert_eq!(cmd.success_exit_codes, vec![0, 1]);
        assert_eq!(cmd.cwd_mode, CommandCwdMode::TaskRoot);
    }

    #[test]
    fn command_mode_defaults_cwd_to_task_root_and_exit_code_zero() {
        let json = r#"{
            "mode": "command",
            "command": {
                "program": "make"
            }
        }"#;
        let definition: PhaseExecutionDefinition =
            serde_json::from_str(json).expect("deserialize minimal command phase");
        assert_eq!(definition.mode, PhaseExecutionMode::Command);
        let cmd = definition.command.expect("command present");
        assert_eq!(cmd.program, "make");
        assert_eq!(cmd.cwd_mode, CommandCwdMode::TaskRoot);
        assert_eq!(cmd.success_exit_codes, vec![0]);
        assert!(cmd.args.is_empty());
        assert!(cmd.env.is_empty());
        assert!(cmd.timeout_secs.is_none());
        assert!(!cmd.parse_json_output);
    }

    #[test]
    fn builtin_kernel_config_all_phases_are_agent_mode() {
        let config = builtin_agent_runtime_config();
        for (phase_id, definition) in &config.phases {
            assert_eq!(definition.mode, PhaseExecutionMode::Agent, "builtin phase '{}' should be agent mode", phase_id);
            assert!(definition.command.is_none(), "builtin phase '{}' should have no command block", phase_id);
        }
    }

    #[test]
    fn command_mode_rejects_empty_args() {
        let config = make_minimal_config_with_phase(
            "lint",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: None,
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: Some(PhaseCommandDefinition {
                    program: "cargo".to_string(),
                    args: vec!["test".to_string(), "  ".to_string()],
                    env: BTreeMap::new(),
                    cwd_mode: CommandCwdMode::ProjectRoot,
                    cwd_path: None,
                    timeout_secs: None,
                    success_exit_codes: vec![0],
                    parse_json_output: false,
                    expected_result_kind: None,
                    expected_schema: None,
                    category: None,
                    failure_pattern: None,
                    excerpt_max_chars: None,
                    on_success_verdict: None,
                    on_failure_verdict: None,
                    confidence: None,
                    failure_risk: None,
                }),
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("args must not contain empty values"));
    }

    #[test]
    fn command_mode_rejects_empty_env_keys() {
        let config = make_minimal_config_with_phase(
            "lint",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: None,
                directive: None,
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: Some(PhaseCommandDefinition {
                    program: "cargo".to_string(),
                    args: vec![],
                    env: BTreeMap::from([("  ".to_string(), "value".to_string())]),
                    cwd_mode: CommandCwdMode::ProjectRoot,
                    cwd_path: None,
                    timeout_secs: None,
                    success_exit_codes: vec![0],
                    parse_json_output: false,
                    expected_result_kind: None,
                    expected_schema: None,
                    category: None,
                    failure_pattern: None,
                    excerpt_max_chars: None,
                    on_success_verdict: None,
                    on_failure_verdict: None,
                    confidence: None,
                    failure_risk: None,
                }),
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("env must not contain empty keys"));
    }

    #[test]
    fn legacy_config_without_new_fields_deserializes_with_none_defaults() {
        let json = r#"{
            "schema": "animus.agent-runtime-config.v2",
            "version": 2,
            "tools_allowlist": ["cargo"],
            "agents": {
                "default": {
                    "description": "Test agent",
                    "system_prompt": "You are a test agent.",
                    "tool": null,
                    "model": null,
                    "fallback_models": [],
                    "reasoning_effort": null,
                    "web_search": null,
                    "network_access": null,
                    "timeout_secs": null,
                    "max_attempts": null,
                    "extra_args": [],
                    "codex_config_overrides": []
                }
            },
            "phases": {
                "default": {
                    "mode": "agent",
                    "agent_id": "default",
                    "directive": "Do work."
                }
            }
        }"#;

        let config: AgentRuntimeConfig = serde_json::from_str(json).expect("deserialize");
        validate_agent_runtime_config(&config).expect("validate");
        let profile = config.agent_profile("default").expect("default profile");
        assert!(profile.role.is_none());
        assert!(profile.mcp_servers.is_empty());
        assert!(profile.skills.is_empty());
        assert!(profile.capabilities.is_empty());
        assert_eq!(profile.tool_policy, AgentToolPolicy::default());
        assert!(profile.mcp_server_configs.is_none());
        assert!(profile.structured_capabilities.is_none());
        assert!(profile.project_overrides.is_none());
    }

    #[test]
    fn agent_tool_policy_roundtrips() {
        let policy = AgentToolPolicy {
            allow: vec!["task.*".to_string(), "workflow.*".to_string()],
            deny: vec!["project.remove".to_string()],
        };
        let json = serde_json::to_string(&policy).expect("serialize");
        let restored: AgentToolPolicy = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, policy);
    }

    #[test]
    fn agent_mcp_server_config_roundtrips() {
        let config = AgentMcpServerConfig {
            source: AgentMcpServerSource::Custom,
            tool_policy: AgentToolPolicy { allow: vec!["read.*".to_string()], deny: vec!["write.*".to_string()] },
            env: BTreeMap::from([("API_KEY".to_string(), "secret".to_string())]),
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: AgentMcpServerConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, config);
    }

    #[test]
    fn agent_mcp_server_source_defaults_to_builtin() {
        let config: AgentMcpServerConfig = serde_json::from_str("{}").expect("deserialize empty");
        assert_eq!(config.source, AgentMcpServerSource::Builtin);
        assert!(config.tool_policy.allow.is_empty());
        assert!(config.tool_policy.deny.is_empty());
        assert!(config.env.is_empty());
    }

    #[test]
    fn agent_memory_capability_helper_matches_explicit_true_flag() {
        let mut profile = AgentProfile::default();
        assert!(!agent_memory_capability_enabled(&profile), "absent capability should be false");
        profile.capabilities.insert("memory".to_string(), false);
        assert!(!agent_memory_capability_enabled(&profile), "explicit false should be false");
        profile.capabilities.insert("memory".to_string(), true);
        assert!(agent_memory_capability_enabled(&profile), "explicit true should be true");
    }

    #[test]
    fn agent_capabilities_flattens_bool_map() {
        let caps = AgentCapabilities {
            flags: BTreeMap::from([("planning".to_string(), true), ("implementation".to_string(), false)]),
        };
        let json = serde_json::to_string(&caps).expect("serialize");
        let value: Value = serde_json::from_str(&json).expect("parse value");
        assert_eq!(value["planning"], json!(true));
        assert_eq!(value["implementation"], json!(false));

        let restored: AgentCapabilities = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, caps);
    }

    #[test]
    fn agent_project_overrides_roundtrips() {
        let overrides = AgentProjectOverrides {
            tool: Some("codex".to_string()),
            model: Some("gpt-4".to_string()),
            extra_args: vec!["--verbose".to_string()],
            env: BTreeMap::from([("DEBUG".to_string(), "1".to_string())]),
        };
        let json = serde_json::to_string(&overrides).expect("serialize");
        let restored: AgentProjectOverrides = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.tool, overrides.tool);
        assert_eq!(restored.model, overrides.model);
        assert_eq!(restored.extra_args, overrides.extra_args);
        assert_eq!(restored.env, overrides.env);
    }

    #[test]
    fn profile_with_new_fields_roundtrips_through_json() {
        let mut config = builtin_agent_runtime_config();
        let profile = config.agents.get_mut("default").expect("default profile");
        profile.mcp_server_configs = Some(BTreeMap::from([(
            "animus".to_string(),
            AgentMcpServerConfig {
                source: AgentMcpServerSource::Builtin,
                tool_policy: AgentToolPolicy { allow: vec!["task.*".to_string()], deny: vec![] },
                env: BTreeMap::new(),
            },
        )]));
        profile.structured_capabilities =
            Some(AgentCapabilities { flags: BTreeMap::from([("planning".to_string(), true)]) });
        profile.project_overrides = Some(BTreeMap::from([(
            "my-project".to_string(),
            AgentProjectOverrides {
                tool: Some("codex".to_string()),
                model: None,
                extra_args: vec![],
                env: BTreeMap::new(),
            },
        )]));

        let json = serde_json::to_string_pretty(&config).expect("serialize");
        let restored: AgentRuntimeConfig = serde_json::from_str(&json).expect("deserialize");
        validate_agent_runtime_config(&restored).expect("validate");

        let restored_profile = restored.agent_profile("default").expect("default profile");
        assert!(restored_profile.mcp_server_configs.is_some());
        let mcp_configs = restored_profile.mcp_server_configs.as_ref().unwrap();
        assert_eq!(mcp_configs.len(), 1);
        assert_eq!(mcp_configs["animus"].source, AgentMcpServerSource::Builtin);

        assert!(restored_profile.structured_capabilities.is_some());
        let caps = restored_profile.structured_capabilities.as_ref().unwrap();
        assert_eq!(caps.flags.get("planning"), Some(&true));

        assert!(restored_profile.project_overrides.is_some());
        let overrides = restored_profile.project_overrides.as_ref().unwrap();
        assert_eq!(overrides["my-project"].tool.as_deref(), Some("codex"));
    }

    #[test]
    fn new_fields_skipped_in_serialization_when_none() {
        let config = builtin_agent_runtime_config();
        let json = serde_json::to_string_pretty(&config).expect("serialize");
        assert!(!json.contains("mcp_server_configs"));
        assert!(!json.contains("structured_capabilities"));
        assert!(!json.contains("project_overrides"));
    }

    #[test]
    fn tool_policy_empty_permits_all() {
        let policy = AgentToolPolicy::default();
        assert!(policy.is_tool_permitted("task.list"));
        assert!(policy.is_tool_permitted("anything"));
        assert!(policy.is_tool_permitted(""));
    }

    #[test]
    fn tool_policy_allowlist_only() {
        let policy = AgentToolPolicy { allow: vec!["task.*".to_string(), "workflow.run".to_string()], deny: vec![] };
        assert!(policy.is_tool_permitted("task.list"));
        assert!(policy.is_tool_permitted("task.create"));
        assert!(policy.is_tool_permitted("task.get"));
        assert!(policy.is_tool_permitted("workflow.run"));
        assert!(!policy.is_tool_permitted("workflow.cancel"));
        assert!(!policy.is_tool_permitted("daemon.stop"));
        assert!(!policy.is_tool_permitted(""));
    }

    #[test]
    fn tool_policy_denylist_only() {
        let policy =
            AgentToolPolicy { allow: vec![], deny: vec!["daemon.*".to_string(), "project.remove".to_string()] };
        assert!(policy.is_tool_permitted("task.list"));
        assert!(policy.is_tool_permitted("workflow.run"));
        assert!(!policy.is_tool_permitted("daemon.stop"));
        assert!(!policy.is_tool_permitted("daemon.start"));
        assert!(!policy.is_tool_permitted("project.remove"));
        assert!(policy.is_tool_permitted("project.list"));
    }

    #[test]
    fn tool_policy_combined_allow_and_deny() {
        let policy = AgentToolPolicy { allow: vec!["task.*".to_string()], deny: vec!["task.delete".to_string()] };
        assert!(policy.is_tool_permitted("task.list"));
        assert!(policy.is_tool_permitted("task.create"));
        assert!(!policy.is_tool_permitted("task.delete"));
        assert!(!policy.is_tool_permitted("workflow.run"));
    }

    #[test]
    fn tool_policy_glob_wildcard_matches_across_dots() {
        let policy = AgentToolPolicy { allow: vec!["animus.*".to_string()], deny: vec![] };
        assert!(policy.is_tool_permitted("animus.task.list"));
        assert!(policy.is_tool_permitted("animus.workflow.run"));
        assert!(policy.is_tool_permitted("animus.x"));
        assert!(!policy.is_tool_permitted("other.thing"));
    }

    #[test]
    fn tool_policy_exact_match() {
        let policy = AgentToolPolicy { allow: vec!["task.list".to_string()], deny: vec![] };
        assert!(policy.is_tool_permitted("task.list"));
        assert!(!policy.is_tool_permitted("task.create"));
        assert!(!policy.is_tool_permitted("task.list.extra"));
    }

    #[test]
    fn tool_policy_wildcard_only_pattern() {
        let policy = AgentToolPolicy { allow: vec!["*".to_string()], deny: vec![] };
        assert!(policy.is_tool_permitted("anything"));
        assert!(policy.is_tool_permitted("a.b.c"));
        assert!(policy.is_tool_permitted(""));
    }

    #[test]
    fn tool_policy_empty_tool_name() {
        let policy = AgentToolPolicy { allow: vec!["task.*".to_string()], deny: vec![] };
        assert!(!policy.is_tool_permitted(""));

        let deny_policy = AgentToolPolicy { allow: vec![], deny: vec!["*".to_string()] };
        assert!(!deny_policy.is_tool_permitted(""));
    }

    #[test]
    fn tool_policy_multiple_wildcards() {
        let policy = AgentToolPolicy { allow: vec!["a.*.c".to_string()], deny: vec![] };
        assert!(policy.is_tool_permitted("a.b.c"));
        assert!(policy.is_tool_permitted("a.x.y.c"));
        assert!(!policy.is_tool_permitted("a.b.d"));
    }

    #[test]
    fn tool_policy_prefix_wildcard() {
        let policy = AgentToolPolicy { allow: vec!["task.get*".to_string()], deny: vec![] };
        assert!(policy.is_tool_permitted("task.get"));
        assert!(policy.is_tool_permitted("task.get_by_id"));
        assert!(!policy.is_tool_permitted("task.list"));
    }

    #[test]
    fn glob_match_basic() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("abc", "abc"));
        assert!(!glob_match("abc", "abcd"));
        assert!(!glob_match("abcd", "abc"));
        assert!(glob_match("a*c", "abc"));
        assert!(glob_match("a*c", "aXYZc"));
        assert!(!glob_match("a*c", "aXYZd"));
        assert!(glob_match("*.*", "a.b"));
        assert!(glob_match("task.*", "task.list"));
        assert!(glob_match("task.*", "task.list.nested"));
    }

    fn make_agent_profile_with_system_prompt(prompt: &str) -> AgentProfile {
        serde_json::from_value(serde_json::json!({
            "system_prompt": prompt
        }))
        .expect("deserialize agent profile")
    }

    #[test]
    fn phase_system_prompt_override_takes_precedence_over_agent_profile() {
        let mut config = builtin_agent_runtime_config();
        config.agents.insert("test-agent".to_string(), make_agent_profile_with_system_prompt("Agent profile prompt"));
        config.phases.insert(
            "custom-phase".to_string(),
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Agent,
                agent_id: Some("test-agent".to_string()),
                directive: Some("Do the thing".to_string()),
                system_prompt: Some("Phase-level prompt override".to_string()),
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        assert_eq!(config.phase_system_prompt("custom-phase"), Some("Phase-level prompt override"));
    }

    #[test]
    fn phase_system_prompt_falls_back_to_agent_profile() {
        let mut config = builtin_agent_runtime_config();
        config.agents.insert("test-agent".to_string(), make_agent_profile_with_system_prompt("Agent profile prompt"));
        config.phases.insert(
            "custom-phase".to_string(),
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Agent,
                agent_id: Some("test-agent".to_string()),
                directive: Some("Do the thing".to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        assert_eq!(config.phase_system_prompt("custom-phase"), Some("Agent profile prompt"));
    }

    #[test]
    fn phase_system_prompt_ignores_empty_override() {
        let mut config = builtin_agent_runtime_config();
        config.agents.insert("test-agent".to_string(), make_agent_profile_with_system_prompt("Agent profile prompt"));
        config.phases.insert(
            "custom-phase".to_string(),
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Agent,
                agent_id: Some("test-agent".to_string()),
                directive: None,
                system_prompt: Some("   ".to_string()),
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        assert_eq!(config.phase_system_prompt("custom-phase"), Some("Agent profile prompt"));
    }

    #[test]
    fn phase_system_prompt_deserializes_with_and_without_field() {
        let with_prompt: PhaseExecutionDefinition = serde_json::from_str(
            r#"{
            "mode": "agent",
            "agent_id": "default",
            "system_prompt": "Custom prompt from JSON"
        }"#,
        )
        .expect("deserialize with system_prompt");
        assert_eq!(with_prompt.system_prompt.as_deref(), Some("Custom prompt from JSON"));

        let without_prompt: PhaseExecutionDefinition = serde_json::from_str(
            r#"{
            "mode": "agent",
            "agent_id": "default"
        }"#,
        )
        .expect("deserialize without system_prompt");
        assert!(without_prompt.system_prompt.is_none());
    }

    #[test]
    fn phase_system_prompt_skips_serialization_when_none() {
        let definition = PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("default".to_string()),
            directive: None,
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: None,
            manual: None,
            default_tool: None,
            idempotency: Idempotency::Unknown,
            worktree: None,
            evals: None,
        };
        let json = serde_json::to_string(&definition).expect("serialize");
        assert!(!json.contains("system_prompt"));

        let with_prompt =
            PhaseExecutionDefinition { system_prompt: Some("My custom prompt".to_string()), ..definition };
        let json = serde_json::to_string(&with_prompt).expect("serialize");
        assert!(json.contains("system_prompt"));
        assert!(json.contains("My custom prompt"));
    }

    #[test]
    fn decision_contract_rejects_min_confidence_above_one() {
        let config = make_minimal_config_with_phase(
            "review",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Agent,
                agent_id: Some("default".to_string()),
                directive: Some("Review the code.".to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: Some(PhaseDecisionContract {
                    required_evidence: Vec::new(),
                    min_confidence: 1.5,
                    max_risk: crate::types::WorkflowDecisionRisk::Medium,
                    allow_missing_decision: true,
                    extra_json_schema: None,
                    fields: BTreeMap::new(),
                }),
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("min_confidence must be between 0.0 and 1.0"));
    }

    #[test]
    fn decision_contract_rejects_min_confidence_below_zero() {
        let config = make_minimal_config_with_phase(
            "review",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Agent,
                agent_id: Some("default".to_string()),
                directive: Some("Review the code.".to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: Some(PhaseDecisionContract {
                    required_evidence: Vec::new(),
                    min_confidence: -0.1,
                    max_risk: crate::types::WorkflowDecisionRisk::Low,
                    allow_missing_decision: true,
                    extra_json_schema: None,
                    fields: BTreeMap::new(),
                }),
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("min_confidence must be between 0.0 and 1.0"));
    }

    #[test]
    fn decision_contract_rejects_extra_json_schema_non_object() {
        let config = make_minimal_config_with_phase(
            "review",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Agent,
                agent_id: Some("default".to_string()),
                directive: Some("Review the code.".to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: Some(PhaseDecisionContract {
                    required_evidence: Vec::new(),
                    min_confidence: 0.7,
                    max_risk: crate::types::WorkflowDecisionRisk::Medium,
                    allow_missing_decision: true,
                    extra_json_schema: Some(serde_json::json!(["not", "an", "object"])),
                    fields: BTreeMap::new(),
                }),
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("extra_json_schema must be a JSON object"));
    }

    #[test]
    fn decision_contract_rejects_invalid_field_type() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "my_field".to_string(),
            PhaseFieldDefinition {
                field_type: "uuid".to_string(),
                required: false,
                description: None,
                enum_values: Vec::new(),
                items: None,
                fields: BTreeMap::new(),
            },
        );
        let config = make_minimal_config_with_phase(
            "review",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Agent,
                agent_id: Some("default".to_string()),
                directive: Some("Review the code.".to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: Some(PhaseDecisionContract {
                    required_evidence: Vec::new(),
                    min_confidence: 0.7,
                    max_risk: crate::types::WorkflowDecisionRisk::Medium,
                    allow_missing_decision: true,
                    extra_json_schema: None,
                    fields,
                }),
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("must be one of string, number, integer, boolean, array, object, null"));
    }

    #[test]
    fn decision_contract_accepts_valid_contract_with_fields() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "verdict".to_string(),
            PhaseFieldDefinition {
                field_type: "string".to_string(),
                required: true,
                description: Some("The review verdict".to_string()),
                enum_values: vec!["approve".to_string(), "reject".to_string()],
                items: None,
                fields: BTreeMap::new(),
            },
        );
        let config = make_minimal_config_with_phase(
            "review",
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Agent,
                agent_id: Some("default".to_string()),
                directive: Some("Review the code.".to_string()),
                system_prompt: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: Some(PhaseDecisionContract {
                    required_evidence: Vec::new(),
                    min_confidence: 0.8,
                    max_risk: crate::types::WorkflowDecisionRisk::Low,
                    allow_missing_decision: false,
                    extra_json_schema: Some(serde_json::json!({"type": "object"})),
                    fields,
                }),
                retry: None,
                skills: Vec::new(),
                command: None,
                manual: None,
                default_tool: None,
                idempotency: Idempotency::Unknown,
                worktree: None,
                evals: None,
            },
        );
        validate_agent_runtime_config(&config).expect("valid decision contract should pass validation");
    }

    #[test]
    fn decision_contract_yaml_roundtrip() {
        let yaml = r#"
phases:
  my-phase:
    mode: agent
    agent: default
    directive: Do work.
    decision_contract:
      min_confidence: 0.85
      max_risk: low
      allow_missing_decision: false
      fields:
        status:
          type: string
          required: true
          enum: [advance, rework, fail]
        reason:
          type: string
          required: true
"#;
        let config = crate::workflow_config::parse_yaml_workflow_config(yaml).expect("parse YAML");
        let phase_def = config.phase_definitions.get("my-phase").expect("phase should exist");
        let contract = phase_def.decision_contract.as_ref().expect("decision_contract should exist");
        assert!((contract.min_confidence - 0.85).abs() < 1e-6);
        assert_eq!(contract.max_risk, crate::types::WorkflowDecisionRisk::Low);
        assert!(!contract.allow_missing_decision);
        let status_field = contract.fields.get("status").expect("status field should exist");
        assert_eq!(status_field.field_type, "string");
        assert!(status_field.required);
        assert_eq!(status_field.enum_values, vec!["advance", "rework", "fail"]);
        let reason_field = contract.fields.get("reason").expect("reason field should exist");
        assert_eq!(reason_field.field_type, "string");
        assert!(reason_field.required);
    }

    #[test]
    fn cli_tool_metadata_all_fields_propagate_via_merge() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow = crate::workflow_config::builtin_workflow_config();
        workflow.tools.insert(
            "full-tool".to_string(),
            crate::workflow_config::ToolDefinition {
                executable: "full-tool-bin".to_string(),
                supports_mcp: Some(true),
                supports_write: Some(true),
                context_window: Some(128_000),
                base_args: vec![],
                supports_streaming: Some(true),
                supports_tool_use: Some(false),
                supports_vision: Some(true),
                supports_long_context: Some(true),
                read_only_flag: Some("--read-only".to_string()),
                response_schema_flag: Some("--schema".to_string()),
            },
        );
        crate::workflow_config::write_workflow_config(temp.path(), &workflow).expect("write workflow config");

        let resolved = load_agent_runtime_config_or_default(temp.path());
        let tool = resolved.cli_tools.get("full-tool").expect("full-tool should exist in cli_tools");
        assert_eq!(tool.executable.as_deref(), Some("full-tool-bin"));
        assert_eq!(tool.supports_streaming, Some(true));
        assert_eq!(tool.supports_tool_use, Some(false));
        assert_eq!(tool.supports_vision, Some(true));
        assert_eq!(tool.supports_long_context, Some(true));
        assert_eq!(tool.read_only_flag.as_deref(), Some("--read-only"));
        assert_eq!(tool.response_schema_flag.as_deref(), Some("--schema"));
    }

    #[test]
    fn phase_fallback_tools_resolves_from_agent_profile() {
        let mut config = builtin_agent_runtime_config();
        let profile = config.agents.get_mut("swe").expect("swe profile");
        profile.fallback_models = vec!["gpt-4o".to_string(), "o4-mini".to_string()];
        profile.fallback_tools = vec!["oai-runner".to_string()];

        let tools = config.phase_fallback_tools("implementation");
        assert_eq!(tools, vec!["oai-runner"]);
    }

    #[test]
    fn phase_fallback_tools_resolves_from_phase_runtime() {
        let mut config = builtin_agent_runtime_config();
        let phase = config.phases.get_mut("implementation").expect("implementation phase");
        phase.runtime = Some(AgentRuntimeOverrides {
            fallback_models: vec!["gpt-4o".to_string(), "o4-mini".to_string()],
            fallback_tools: vec!["oai-runner".to_string(), "codex".to_string()],
            ..AgentRuntimeOverrides::default()
        });

        let tools = config.phase_fallback_tools("implementation");
        assert_eq!(tools, vec!["oai-runner", "codex"]);
    }

    #[test]
    fn phase_fallback_tools_phase_runtime_takes_precedence_over_agent_profile() {
        let mut config = builtin_agent_runtime_config();
        let profile = config.agents.get_mut("swe").expect("swe profile");
        profile.fallback_models = vec!["gpt-4o".to_string()];
        profile.fallback_tools = vec!["claude".to_string()];
        let phase = config.phases.get_mut("implementation").expect("implementation phase");
        phase.runtime = Some(AgentRuntimeOverrides {
            fallback_models: vec!["o4-mini".to_string()],
            fallback_tools: vec!["codex".to_string()],
            ..AgentRuntimeOverrides::default()
        });

        let tools = config.phase_fallback_tools("implementation");
        assert_eq!(tools, vec!["codex"]);
    }

    #[test]
    fn phase_fallback_tools_defaults_to_empty() {
        let config = builtin_agent_runtime_config();
        let tools = config.phase_fallback_tools("implementation");
        assert!(tools.is_empty());
    }

    #[test]
    fn fallback_models_and_tools_roundtrip_through_json() {
        let overrides = AgentRuntimeOverrides {
            model: Some("claude-sonnet-4-20250514".to_string()),
            fallback_models: vec!["gpt-4o".to_string(), "o4-mini".to_string()],
            fallback_tools: vec!["oai-runner".to_string()],
            ..AgentRuntimeOverrides::default()
        };
        let json = serde_json::to_string(&overrides).expect("serialize");
        let restored: AgentRuntimeOverrides = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.fallback_models, vec!["gpt-4o", "o4-mini"]);
        assert_eq!(restored.fallback_tools, vec!["oai-runner"]);
    }

    fn make_check_command(id: &str, program: &str) -> EvalCheck {
        EvalCheck {
            id: id.into(),
            kind: EvalKind::Command,
            command: Some(program.into()),
            args: Vec::new(),
            working_dir: None,
            timeout_secs: None,
            expected_exit: 0,
            agent: None,
            prompt: None,
        }
    }

    #[test]
    fn validate_agent_runtime_config_rejects_evals_with_empty_check_list() {
        let mut config = builtin_agent_runtime_config();
        let phase = config.phases.get_mut("implementation").expect("phase exists");
        phase.evals =
            Some(EvalsConfig { pass_threshold: 1.0, on_fail: EvalOnFail::Block, max_reworks: 0, checks: Vec::new() });
        let err = validate_agent_runtime_config(&config).expect_err("empty checks must fail");
        let msg = format!("{:#}", err);
        assert!(msg.contains("must declare at least one check"), "got: {msg}");
    }

    #[test]
    fn validate_agent_runtime_config_rejects_invalid_agent_reasoning_effort() {
        let mut config = builtin_agent_runtime_config();
        let profile = config.agents.get_mut("default").expect("profile exists");
        profile.reasoning_effort = Some("turbo".to_string());
        let err = validate_agent_runtime_config(&config).expect_err("invalid effort must fail");
        let msg = format!("{:#}", err);
        assert!(msg.contains("reasoning_effort must be one of"), "got: {msg}");
    }

    #[test]
    fn validate_agent_runtime_config_accepts_valid_agent_reasoning_effort() {
        let mut config = builtin_agent_runtime_config();
        let profile = config.agents.get_mut("default").expect("profile exists");
        profile.reasoning_effort = Some("High".to_string());
        validate_agent_runtime_config(&config).expect("case-insensitive valid effort must pass");
    }

    #[test]
    fn validate_agent_runtime_config_rejects_invalid_phase_reasoning_effort() {
        let mut config = builtin_agent_runtime_config();
        let phase = config.phases.get_mut("implementation").expect("phase exists");
        phase.runtime = Some(AgentRuntimeOverrides { reasoning_effort: Some("max".to_string()), ..Default::default() });
        let err = validate_agent_runtime_config(&config).expect_err("invalid phase effort must fail");
        let msg = format!("{:#}", err);
        assert!(msg.contains("runtime.reasoning_effort must be one of"), "got: {msg}");
    }

    #[test]
    fn permission_mode_roundtrips_from_workflow_yaml_into_compiled_config() {
        let yaml = r#"
agents:
  cautious:
    description: "Cautious agent"
    permission_mode: plan
"#;
        let config = crate::workflow_config::parse_yaml_workflow_config(yaml).expect("parse YAML");
        let overlay = config.agent_profiles.get("cautious").expect("cautious profile parsed");
        assert_eq!(overlay.permission_mode.as_deref(), Some("plan"));

        let mut runtime = builtin_agent_runtime_config();
        merge_workflow_runtime_overlay(&mut runtime, &config);
        let profile = runtime.agent_profile("cautious").expect("cautious profile compiled");
        assert_eq!(profile.permission_mode.as_deref(), Some("plan"));

        // Serializing back through the overlay shape preserves the field.
        let overlay = AgentProfileOverlay::from(profile.clone());
        let json = serde_json::to_string(&overlay).expect("serialize overlay");
        let restored: AgentProfileOverlay = serde_json::from_str(&json).expect("deserialize overlay");
        assert_eq!(restored.permission_mode.as_deref(), Some("plan"));
    }

    #[test]
    fn merge_agent_profile_overlay_permission_mode_wins_and_absent_inherits() {
        let mut base: AgentProfile = serde_json::from_value(serde_json::json!({
            "system_prompt": "base prompt",
            "permission_mode": "plan"
        }))
        .expect("base profile");

        let absent: AgentProfileOverlay = serde_json::from_value(serde_json::json!({})).expect("empty overlay");
        merge_agent_profile(&mut base, &absent);
        assert_eq!(base.permission_mode.as_deref(), Some("plan"), "absent overlay field must inherit the base");

        let declared: AgentProfileOverlay = serde_json::from_value(serde_json::json!({
            "permission_mode": "acceptEdits"
        }))
        .expect("overlay profile");
        merge_agent_profile(&mut base, &declared);
        assert_eq!(base.permission_mode.as_deref(), Some("acceptEdits"), "declared overlay field must win");
    }

    #[test]
    fn phase_permission_mode_phase_runtime_takes_precedence_over_agent_profile() {
        let mut config = builtin_agent_runtime_config();
        let profile = config.agents.get_mut("swe").expect("swe profile");
        profile.permission_mode = Some("plan".to_string());

        assert_eq!(config.phase_permission_mode("implementation"), Some("plan"));

        let phase = config.phases.get_mut("implementation").expect("implementation phase");
        phase.runtime =
            Some(AgentRuntimeOverrides { permission_mode: Some("acceptEdits".to_string()), ..Default::default() });
        assert_eq!(config.phase_permission_mode("implementation"), Some("acceptEdits"));
    }

    #[test]
    fn validate_agent_runtime_config_rejects_empty_permission_mode() {
        let mut config = builtin_agent_runtime_config();
        let profile = config.agents.get_mut("default").expect("profile exists");
        profile.permission_mode = Some("   ".to_string());
        let err = validate_agent_runtime_config(&config).expect_err("empty permission_mode must fail");
        let msg = format!("{:#}", err);
        assert!(msg.contains("permission_mode must not be empty"), "got: {msg}");
    }

    #[test]
    fn validate_agent_runtime_config_accepts_unknown_permission_mode() {
        let mut config = builtin_agent_runtime_config();
        let profile = config.agents.get_mut("default").expect("profile exists");
        profile.permission_mode = Some("totally-custom".to_string());
        validate_agent_runtime_config(&config).expect("unknown permission_mode must pass through, not block");
    }

    #[test]
    fn known_permission_modes_match_case_insensitively() {
        assert!(is_known_permission_mode("acceptEdits"));
        assert!(is_known_permission_mode("acceptedits"));
        assert!(is_known_permission_mode("on-failure"));
        assert!(is_known_permission_mode(" yolo "));
        assert!(!is_known_permission_mode("totally-custom"));
    }

    #[test]
    fn validate_agent_runtime_config_rejects_rework_with_zero_budget() {
        let mut config = builtin_agent_runtime_config();
        let phase = config.phases.get_mut("implementation").expect("phase exists");
        phase.evals = Some(EvalsConfig {
            pass_threshold: 1.0,
            on_fail: EvalOnFail::Rework,
            max_reworks: 0,
            checks: vec![make_check_command("x", "cargo")],
        });
        let err = validate_agent_runtime_config(&config).expect_err("rework w/ zero budget must fail");
        let msg = format!("{:#}", err);
        assert!(msg.contains("max_reworks > 0"), "got: {msg}");
    }

    #[test]
    fn validate_agent_runtime_config_rejects_llm_judge_timeout_secs() {
        let mut config = builtin_agent_runtime_config();
        let phase = config.phases.get_mut("implementation").expect("phase exists");
        phase.evals = Some(EvalsConfig {
            pass_threshold: 1.0,
            on_fail: EvalOnFail::Block,
            max_reworks: 0,
            checks: vec![EvalCheck {
                id: "q".into(),
                kind: EvalKind::LlmJudge,
                command: None,
                args: Vec::new(),
                working_dir: None,
                timeout_secs: Some(15),
                expected_exit: 0,
                agent: Some("default".into()),
                prompt: Some("Verdict?".into()),
            }],
        });
        let err = validate_agent_runtime_config(&config).expect_err("judge timeout_secs must fail");
        let msg = format!("{:#}", err);
        assert!(msg.contains("does not support timeout_secs"), "got: {msg}");
    }

    #[test]
    fn validate_agent_runtime_config_rejects_system_prompt_file_in_compiled_config() {
        let mut config = builtin_agent_runtime_config();
        let default_profile = config.agents.get_mut("default").expect("default agent");
        default_profile.system_prompt_file = Some("prompts/whatever.md".to_string());

        let err = validate_agent_runtime_config(&config).expect_err("compiled config must reject the field");
        let msg = format!("{:#}", err);
        assert!(msg.contains("system_prompt_file"), "missing field name: {msg}");
        assert!(msg.contains("source YAML"), "missing guidance: {msg}");
    }

    #[test]
    fn pack_agent_runtime_overlay_rejects_absolute_system_prompt_file() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        let pack_root = crate::machine_installed_packs_dir().join("animus.fixture-abs").join("0.1.0");
        fs::create_dir_all(pack_root.join("workflows")).expect("create workflows");
        fs::create_dir_all(pack_root.join("runtime")).expect("create runtime");

        fs::write(
            pack_root.join(crate::PACK_MANIFEST_FILE_NAME),
            r#"
schema = "animus.pack.v1"
id = "animus.fixture-abs"
version = "0.1.0"
kind = "domain-pack"
title = "animus.fixture-abs"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
animus_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["animus.task"]
default_kind = "animus.task"

[workflows]
root = "workflows"
exports = ["animus.fixture-abs/noop"]

[runtime]
agent_overlay = "runtime/agent-runtime.overlay.yaml"
"#,
        )
        .expect("write manifest");

        fs::write(
            pack_root.join("workflows/noop.yaml"),
            r#"
workflows:
  - id: animus.fixture-abs/noop
    name: noop
    phases: []
"#,
        )
        .expect("write workflow");

        fs::write(
            pack_root.join("runtime/agent-runtime.overlay.yaml"),
            r#"
agents:
  pack-agent:
    description: "Escapes"
    system_prompt_file: /etc/passwd
"#,
        )
        .expect("write agent overlay");

        let manifest = crate::load_pack_manifest(&pack_root).expect("load manifest");
        let err = crate::load_pack_agent_runtime_overlay(&manifest).expect_err("absolute path should be rejected");
        let msg = format!("{:#}", err);
        assert!(msg.contains("must be a relative path"), "missing guidance: {msg}");
        assert!(msg.contains("animus.fixture-abs"), "missing pack id: {msg}");
    }

    #[test]
    #[cfg(unix)]
    fn pack_agent_runtime_overlay_rejects_symlink_escape() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        let target = outside.path().join("secret.md");
        fs::write(&target, "secret body").expect("write secret");

        let pack_root = crate::machine_installed_packs_dir().join("animus.fixture-symlink").join("0.1.0");
        fs::create_dir_all(pack_root.join("workflows")).expect("create workflows");
        fs::create_dir_all(pack_root.join("runtime/prompts")).expect("create runtime/prompts");

        let symlink_path = pack_root.join("runtime/prompts/agent.md");
        std::os::unix::fs::symlink(&target, &symlink_path).expect("symlink");

        fs::write(
            pack_root.join(crate::PACK_MANIFEST_FILE_NAME),
            r#"
schema = "animus.pack.v1"
id = "animus.fixture-symlink"
version = "0.1.0"
kind = "domain-pack"
title = "animus.fixture-symlink"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
animus_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["animus.task"]
default_kind = "animus.task"

[workflows]
root = "workflows"
exports = ["animus.fixture-symlink/noop"]

[runtime]
agent_overlay = "runtime/agent-runtime.overlay.yaml"
"#,
        )
        .expect("write manifest");

        fs::write(
            pack_root.join("workflows/noop.yaml"),
            r#"
workflows:
  - id: animus.fixture-symlink/noop
    name: noop
    phases: []
"#,
        )
        .expect("write workflow");

        fs::write(
            pack_root.join("runtime/agent-runtime.overlay.yaml"),
            r#"
agents:
  pack-agent:
    description: "Escapes via symlink"
    system_prompt_file: prompts/agent.md
"#,
        )
        .expect("write overlay");

        let manifest = crate::load_pack_manifest(&pack_root).expect("load manifest");
        let err = crate::load_pack_agent_runtime_overlay(&manifest).expect_err("symlink escape should be rejected");
        let msg = format!("{:#}", err);
        assert!(msg.contains("outside the pack root"), "missing containment error: {msg}");
    }

    #[test]
    fn pack_agent_runtime_overlay_rejects_parent_dir_segments() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        let pack_root = crate::machine_installed_packs_dir().join("animus.fixture-parent").join("0.1.0");
        fs::create_dir_all(pack_root.join("workflows")).expect("create workflows");
        fs::create_dir_all(pack_root.join("runtime")).expect("create runtime");

        fs::write(
            pack_root.join(crate::PACK_MANIFEST_FILE_NAME),
            r#"
schema = "animus.pack.v1"
id = "animus.fixture-parent"
version = "0.1.0"
kind = "domain-pack"
title = "animus.fixture-parent"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
animus_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["animus.task"]
default_kind = "animus.task"

[workflows]
root = "workflows"
exports = ["animus.fixture-parent/noop"]

[runtime]
agent_overlay = "runtime/agent-runtime.overlay.yaml"
"#,
        )
        .expect("write manifest");

        fs::write(
            pack_root.join("workflows/noop.yaml"),
            r#"
workflows:
  - id: animus.fixture-parent/noop
    name: noop
    phases: []
"#,
        )
        .expect("write workflow");

        fs::write(
            pack_root.join("runtime/agent-runtime.overlay.yaml"),
            r#"
agents:
  pack-agent:
    description: "Escapes"
    system_prompt_file: ../../escape.md
"#,
        )
        .expect("write agent overlay");

        let manifest = crate::load_pack_manifest(&pack_root).expect("load manifest");
        let err = crate::load_pack_agent_runtime_overlay(&manifest).expect_err("parent dir should be rejected");
        let msg = format!("{:#}", err);
        assert!(msg.contains("must not contain '..'"), "missing parent dir error: {msg}");
    }

    #[test]
    fn merge_agent_profile_overlay_takes_system_prompt_file() {
        let mut base: AgentProfile = serde_json::from_value(serde_json::json!({
            "system_prompt": "base prompt"
        }))
        .expect("base profile");
        let overlay: AgentProfileOverlay = serde_json::from_value(serde_json::json!({
            "system_prompt_file": "prompts/overlay.md"
        }))
        .expect("overlay profile");

        merge_agent_profile(&mut base, &overlay);
        assert_eq!(base.system_prompt_file.as_deref(), Some("prompts/overlay.md"));
        assert_eq!(base.system_prompt, "base prompt");
    }

    #[test]
    fn merge_agent_profile_explicit_default_values_override_base() {
        let mut base: AgentProfile = serde_json::from_value(serde_json::json!({
            "description": "base description",
            "memory": { "enabled": true, "scope": "project" },
            "mcp_servers": ["animus", "github"],
            "skills": ["base-skill"],
            "capabilities": { "memory": true },
            "fallback_models": ["base-fallback"]
        }))
        .expect("base profile");
        let overlay: AgentProfileOverlay = serde_json::from_value(serde_json::json!({
            "memory": { "enabled": false },
            "mcp_servers": [],
            "skills": [],
            "capabilities": {},
            "fallback_models": []
        }))
        .expect("overlay profile");

        merge_agent_profile(&mut base, &overlay);
        assert!(!base.memory.enabled, "explicit memory.enabled=false must win over the base");
        assert!(base.mcp_servers.is_empty(), "explicit empty mcp_servers must clear the base list");
        assert!(base.skills.is_empty(), "explicit empty skills must clear the base list");
        assert!(base.capabilities.is_empty(), "explicit empty capabilities must clear the base map");
        assert!(base.fallback_models.is_empty(), "explicit empty fallback_models must clear the base list");
        assert_eq!(base.description, "base description", "absent overlay fields must inherit the base value");
    }

    #[test]
    fn memory_max_entries_round_trips_through_profile() {
        let profile: AgentProfile = serde_json::from_value(serde_json::json!({
            "memory": { "enabled": true, "max_entries": 50 }
        }))
        .expect("profile with memory.max_entries");
        assert_eq!(profile.memory.max_entries, Some(50));

        // Re-serialize and confirm the field survives the round trip.
        let reserialized = serde_json::to_value(&profile).expect("serialize profile");
        assert_eq!(reserialized.pointer("/memory/max_entries").and_then(Value::as_u64), Some(50));

        // Absent → None, so the store applies its default cap.
        let bare: AgentProfile = serde_json::from_value(serde_json::json!({ "memory": { "enabled": true } }))
            .expect("profile without max_entries");
        assert_eq!(bare.memory.max_entries, None);
    }

    #[test]
    fn memory_max_entries_zero_is_rejected_by_validation() {
        let mut config = builtin_agent_runtime_config();
        let profile = config.agents.get_mut("default").expect("profile exists");
        profile.memory.max_entries = Some(0);
        let err = validate_agent_runtime_config(&config).expect_err("max_entries: 0 must fail validation");
        let msg = format!("{:#}", err);
        assert!(msg.contains("max_entries"), "error names the offending field: {msg}");
    }

    #[test]
    fn merge_agent_profile_absent_fields_inherit_base() {
        let mut base: AgentProfile = serde_json::from_value(serde_json::json!({
            "description": "base description",
            "memory": { "enabled": true },
            "mcp_servers": ["animus"],
            "skills": ["base-skill"],
            "model": "base-model"
        }))
        .expect("base profile");
        let overlay: AgentProfileOverlay = serde_json::from_value(serde_json::json!({
            "model": "overlay-model"
        }))
        .expect("overlay profile");

        merge_agent_profile(&mut base, &overlay);
        assert_eq!(base.model.as_deref(), Some("overlay-model"));
        assert!(base.memory.enabled);
        assert_eq!(base.mcp_servers, vec!["animus".to_string()]);
        assert_eq!(base.skills, vec!["base-skill".to_string()]);
        assert_eq!(base.description, "base description");
    }

    #[test]
    fn pack_agent_runtime_overlay_resolves_system_prompt_file() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());

        let pack_root = crate::machine_installed_packs_dir().join("animus.fixture-prompt").join("0.1.0");
        fs::create_dir_all(pack_root.join("workflows")).expect("create workflows");
        fs::create_dir_all(pack_root.join("runtime/prompts")).expect("create runtime/prompts");
        let prompt_body = "Pack-supplied prompt body.\n";
        fs::write(pack_root.join("runtime/prompts/agent.md"), prompt_body).expect("write prompt");

        fs::write(
            pack_root.join(crate::PACK_MANIFEST_FILE_NAME),
            r#"
schema = "animus.pack.v1"
id = "animus.fixture-prompt"
version = "0.1.0"
kind = "domain-pack"
title = "animus.fixture-prompt"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
animus_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["animus.task"]
default_kind = "animus.task"

[workflows]
root = "workflows"
exports = ["animus.fixture-prompt/noop"]

[runtime]
agent_overlay = "runtime/agent-runtime.overlay.yaml"
"#,
        )
        .expect("write manifest");

        fs::write(
            pack_root.join("workflows/noop.yaml"),
            r#"
workflows:
  - id: animus.fixture-prompt/noop
    name: noop
    phases: []
"#,
        )
        .expect("write workflow");

        fs::write(
            pack_root.join("runtime/agent-runtime.overlay.yaml"),
            r#"
agents:
  pack-agent:
    description: "Pack-shipped agent"
    system_prompt_file: prompts/agent.md
"#,
        )
        .expect("write agent overlay");

        let manifest = crate::load_pack_manifest(&pack_root).expect("load manifest");
        let overlay = crate::load_pack_agent_runtime_overlay(&manifest).expect("load overlay").expect("overlay");
        let agent = overlay.agents.get("pack-agent").expect("pack-agent");
        assert_eq!(agent.system_prompt.as_deref(), Some(prompt_body));
        assert!(agent.system_prompt_file.is_none());
    }

    #[test]
    fn merge_agent_profile_keeps_base_system_prompt_file_when_overlay_none() {
        let mut base: AgentProfile = serde_json::from_value(serde_json::json!({
            "system_prompt": "base prompt",
            "system_prompt_file": "prompts/base.md"
        }))
        .expect("base profile");
        let overlay: AgentProfileOverlay = serde_json::from_value(serde_json::json!({
            "system_prompt": "overlay prompt"
        }))
        .expect("overlay profile");

        merge_agent_profile(&mut base, &overlay);
        assert_eq!(base.system_prompt_file.as_deref(), Some("prompts/base.md"));
        assert_eq!(base.system_prompt, "overlay prompt");
    }
}
