//! Agent-runtime *type definitions* shared by the canonical `WorkflowConfig`
//! model and the standardized YAML parser.
//!
//! These are the parse-input / config-model types (phase execution, eval,
//! agent profile + overlay, approval policy, ...). They moved here from the
//! kernel's `orchestrator-config::agent_runtime_config` so the protocol crate
//! is the canonical home for the config contract. The kernel's
//! `agent_runtime_config` module re-exports them and keeps the COMPILER
//! (derivation, validation, pack-overlay merge, file IO, the builtin runtime
//! config) that consumes them.
//!
//! `crate::workflow_types::WorktreeConfig` is referenced by
//! [`PhaseExecutionDefinition`]; both live in this crate now.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow_types::WorktreeConfig;

/// `crate::types::*` in the original kernel module resolved to
/// `protocol::orchestrator::{PhaseEvidenceKind, WorkflowDecisionRisk}`.
use protocol::orchestrator::{PhaseEvidenceKind, WorkflowDecisionRisk};

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
    pub required_evidence: Vec<PhaseEvidenceKind>,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f32,
    #[serde(default = "default_max_risk")]
    pub max_risk: WorkflowDecisionRisk,
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
fn default_max_risk() -> WorkflowDecisionRisk {
    WorkflowDecisionRisk::Medium
}
fn default_true() -> bool {
    true
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

pub fn glob_match(pattern: &str, value: &str) -> bool {
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

pub fn default_eval_pass_threshold() -> f32 {
    1.0
}

pub fn default_eval_expected_exit() -> i32 {
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
    pub worktree: Option<WorktreeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evals: Option<EvalsConfig>,
}

pub fn merge_agent_profile(base: &mut AgentProfile, overlay: &AgentProfileOverlay) {
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

pub(crate) fn default_command_cwd_mode() -> CommandCwdMode {
    CommandCwdMode::TaskRoot
}

pub(crate) fn default_success_exit_codes() -> Vec<i32> {
    vec![0]
}
