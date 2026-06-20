use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_types::AgentProfileOverlay;
use crate::agent_types::PhaseExecutionDefinition;

pub const WORKFLOW_CONFIG_SCHEMA_ID: &str = "animus.workflow-config.v2";
pub const WORKFLOW_CONFIG_VERSION: u32 = 2;
pub const WORKFLOW_CONFIG_FILE_NAME: &str = "workflow-config.v2.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseUiDefinition {
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub docs_url: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_visible")]
    pub visible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PhaseTransitionConfig {
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
    #[serde(default)]
    pub allow_agent_target: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_targets: Vec<String>,
}

pub fn default_max_rework_attempts() -> u32 {
    3
}

/// What the workflow runner should do when a [`BudgetConfig`] cap is hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BudgetOnExceed {
    /// Move the workflow into a manual-approval state with reason `budget_exceeded`.
    #[default]
    Pause,
    /// Terminate the workflow as failed.
    Fail,
    /// Emit a warning to logs + events and continue running.
    Warn,
}

impl BudgetOnExceed {
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetOnExceed::Pause => "pause",
            BudgetOnExceed::Fail => "fail",
            BudgetOnExceed::Warn => "warn",
        }
    }
}

/// Cost ceiling declared on a [`WorkflowDefinition`] or a phase entry.
///
/// Workflow caps subsume phase caps: a workflow cap that is lower than
/// any individual phase cap is still authoritative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BudgetConfig {
    /// Combined input + output + reasoning token ceiling. `None` means
    /// no token ceiling for this scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// USD ceiling with cents precision. `None` means no cost ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
    /// Action to take when either ceiling is crossed.
    #[serde(default)]
    pub on_exceed: BudgetOnExceed,
}

impl BudgetConfig {
    /// True when no cap is set — the config is effectively a no-op.
    pub fn is_empty(&self) -> bool {
        self.max_tokens.is_none() && self.max_cost_usd.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowPhaseConfig {
    pub id: String,
    #[serde(default = "default_max_rework_attempts")]
    pub max_rework_attempts: u32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub on_verdict: HashMap<String, PhaseTransitionConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_if: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubWorkflowRef {
    pub workflow_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkflowPhaseEntry {
    SubWorkflow(SubWorkflowRef),
    Simple(String),
    Rich(WorkflowPhaseConfig),
}

impl WorkflowPhaseEntry {
    pub fn phase_id(&self) -> &str {
        match self {
            WorkflowPhaseEntry::Simple(id) => id.as_str(),
            WorkflowPhaseEntry::Rich(config) => config.id.as_str(),
            WorkflowPhaseEntry::SubWorkflow(sub) => sub.workflow_ref.as_str(),
        }
    }

    pub fn on_verdict(&self) -> Option<&HashMap<String, PhaseTransitionConfig>> {
        match self {
            WorkflowPhaseEntry::Simple(_) | WorkflowPhaseEntry::SubWorkflow(_) => None,
            WorkflowPhaseEntry::Rich(config) => {
                if config.on_verdict.is_empty() {
                    None
                } else {
                    Some(&config.on_verdict)
                }
            }
        }
    }

    pub fn max_rework_attempts(&self) -> Option<u32> {
        match self {
            WorkflowPhaseEntry::Simple(_) | WorkflowPhaseEntry::SubWorkflow(_) => None,
            WorkflowPhaseEntry::Rich(config) => Some(config.max_rework_attempts),
        }
    }

    pub fn skip_if(&self) -> &[String] {
        match self {
            WorkflowPhaseEntry::Simple(_) | WorkflowPhaseEntry::SubWorkflow(_) => &[],
            WorkflowPhaseEntry::Rich(config) => &config.skip_if,
        }
    }

    pub fn is_sub_workflow(&self) -> bool {
        matches!(self, WorkflowPhaseEntry::SubWorkflow(_))
    }

    /// Phase-level budget cap declared inline on a rich phase entry.
    pub fn budget(&self) -> Option<&BudgetConfig> {
        match self {
            WorkflowPhaseEntry::Simple(_) | WorkflowPhaseEntry::SubWorkflow(_) => None,
            WorkflowPhaseEntry::Rich(config) => config.budget.as_ref(),
        }
    }
}

impl From<String> for WorkflowPhaseEntry {
    fn from(id: String) -> Self {
        WorkflowPhaseEntry::Simple(id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowVariable {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeMode {
    /// Create a worktree when the subject implies write work (legacy behavior).
    #[default]
    Auto,
    /// Always require a fresh worktree; fail-fast if it can't be created.
    Required,
    /// Never create a worktree; the phase runs in the project root.
    Skip,
}

/// Workflow- or phase-level worktree control. Authors may shorten to a single
/// `worktree: skip` scalar in YAML; the parser expands that into
/// `{ mode: Skip, .. }` with defaults.
///
/// The kernel surfaces this on the compiled `WorkflowConfig` but does NOT
/// itself create or remove worktrees — that lives in the out-of-tree
/// `launchapp-dev/animus-workflow-runner-default` plugin (v0.4.0+). Older
/// runner plugins ignore the field and behave as `Auto`.
// TODO(codex-p2): wire mode + base_ref + cleanup into
// animus-workflow-runner-default's `WorkflowSession::ensure_cwd()` so
// `required` and `skip` enforce, not just document, runtime behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeConfig {
    #[serde(default)]
    pub mode: WorktreeMode,
    #[serde(default = "default_worktree_cleanup")]
    pub cleanup: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self { mode: WorktreeMode::Auto, cleanup: default_worktree_cleanup(), base_ref: None }
    }
}

pub(crate) fn default_worktree_cleanup() -> bool {
    true
}

impl WorktreeConfig {
    pub fn skip() -> Self {
        Self { mode: WorktreeMode::Skip, cleanup: default_worktree_cleanup(), base_ref: None }
    }

    pub fn required() -> Self {
        Self { mode: WorktreeMode::Required, cleanup: default_worktree_cleanup(), base_ref: None }
    }

    pub(crate) fn parse_mode(value: &str) -> Result<WorktreeMode> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(WorktreeMode::Auto),
            "required" => Ok(WorktreeMode::Required),
            "skip" => Ok(WorktreeMode::Skip),
            other => Err(anyhow!("invalid worktree mode '{}' (expected auto, required, or skip)", other)),
        }
    }

    /// Convert the permissive YAML representation (`YamlPhaseWorktree`) into
    /// the canonical `WorktreeConfig`. Accepts a short-form scalar
    /// (`worktree: skip`) or a long-form map.
    pub(crate) fn from_yaml(yaml: crate::yaml_types::YamlPhaseWorktree) -> Result<Self> {
        match yaml {
            crate::yaml_types::YamlPhaseWorktree::Bool(flag) => {
                let mode = if flag { WorktreeMode::Auto } else { WorktreeMode::Skip };
                Ok(Self { mode, cleanup: default_worktree_cleanup(), base_ref: None })
            }
            crate::yaml_types::YamlPhaseWorktree::Mode(scalar) => {
                let mode = Self::parse_mode(&scalar)?;
                Ok(Self { mode, cleanup: default_worktree_cleanup(), base_ref: None })
            }
            crate::yaml_types::YamlPhaseWorktree::Full(config) => Ok(config),
        }
    }
}

/// Top-level declarative secret reference. `${secret.<key>}` interpolation
/// resolves the named env var at compile time; required-but-unset fails the
/// compile with a file path + line number diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretRef {
    pub env: String,
    #[serde(default = "default_secret_required")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

pub(crate) fn default_secret_required() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub phases: Vec<WorkflowPhaseEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<WorkflowVariable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeConfig>,
    pub budget: Option<BudgetConfig>,
}

impl WorkflowDefinition {
    pub fn phase_ids(&self) -> Vec<String> {
        self.phases.iter().map(|entry| entry.phase_id().trim().to_owned()).filter(|id| !id.is_empty()).collect()
    }
}

pub fn expand_workflow_phases(workflows: &[WorkflowDefinition], workflow_ref: &str) -> Result<Vec<WorkflowPhaseEntry>> {
    let mut visited = HashSet::new();
    expand_workflow_phases_inner(workflows, workflow_ref, &mut visited)
}

pub fn collect_workflow_refs(workflows: &[WorkflowDefinition], workflow_ref: &str) -> Result<Vec<String>> {
    let mut active = HashSet::new();
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    collect_workflow_refs_inner(workflows, workflow_ref, &mut active, &mut seen, &mut refs)?;
    Ok(refs)
}

fn collect_workflow_refs_inner(
    workflows: &[WorkflowDefinition],
    workflow_ref: &str,
    active: &mut HashSet<String>,
    seen: &mut HashSet<String>,
    refs: &mut Vec<String>,
) -> Result<()> {
    let normalized = workflow_ref.to_ascii_lowercase();
    if !active.insert(normalized.clone()) {
        let chain: Vec<&str> = active.iter().map(String::as_str).collect();
        return Err(anyhow!(
            "circular sub-workflow reference detected: '{}' (visited: {})",
            workflow_ref,
            chain.join(" -> ")
        ));
    }

    let workflow = workflows
        .iter()
        .find(|candidate| candidate.id.eq_ignore_ascii_case(workflow_ref))
        .ok_or_else(|| anyhow!("sub-workflow '{}' not found", workflow_ref))?;

    if seen.insert(normalized.clone()) {
        refs.push(workflow.id.clone());
        for entry in &workflow.phases {
            if let WorkflowPhaseEntry::SubWorkflow(sub) = entry {
                collect_workflow_refs_inner(workflows, &sub.workflow_ref, active, seen, refs)?;
            }
        }
    }

    active.remove(&normalized);
    Ok(())
}

fn expand_workflow_phases_inner(
    workflows: &[WorkflowDefinition],
    workflow_ref: &str,
    visited: &mut HashSet<String>,
) -> Result<Vec<WorkflowPhaseEntry>> {
    let normalized = workflow_ref.to_ascii_lowercase();
    if !visited.insert(normalized.clone()) {
        let chain: Vec<&str> = visited.iter().map(String::as_str).collect();
        return Err(anyhow!(
            "circular sub-workflow reference detected: '{}' (visited: {})",
            workflow_ref,
            chain.join(" -> ")
        ));
    }

    let workflow = workflows
        .iter()
        .find(|p| p.id.eq_ignore_ascii_case(workflow_ref))
        .ok_or_else(|| anyhow!("sub-workflow '{}' not found", workflow_ref))?;

    let mut expanded = Vec::new();
    for entry in &workflow.phases {
        match entry {
            WorkflowPhaseEntry::SubWorkflow(sub) => {
                let sub_phases = expand_workflow_phases_inner(workflows, &sub.workflow_ref, visited)?;
                expanded.extend(sub_phases);
            }
            other => {
                expanded.push(other.clone());
            }
        }
    }

    visited.remove(&normalized);
    Ok(expanded)
}

pub fn resolve_workflow_variables(
    definitions: &[WorkflowVariable],
    cli_vars: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut resolved = HashMap::new();
    let mut missing: Vec<String> = Vec::new();

    for var in definitions {
        if let Some(value) = cli_vars.get(&var.name) {
            resolved.insert(var.name.clone(), value.clone());
        } else if let Some(ref default) = var.default {
            resolved.insert(var.name.clone(), default.clone());
        } else if var.required {
            missing.push(var.name.clone());
        }
    }

    if !missing.is_empty() {
        missing.sort();
        return Err(anyhow!("missing required workflow variable(s): {}", missing.join(", ")));
    }

    Ok(resolved)
}

pub fn expand_variables(text: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start + 2..].find("}}") else {
            break;
        };
        let name = &rest[start + 2..start + 2 + end];
        match vars.get(name) {
            Some(value) => {
                result.push_str(&rest[..start]);
                result.push_str(value);
                rest = &rest[start + 2 + end + 2..];
            }
            None => {
                result.push_str(&rest[..=start]);
                rest = &rest[start + 1..];
            }
        }
    }
    result.push_str(rest);
    result
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowCheckpointRetentionConfig {
    #[serde(default = "default_keep_last_per_phase")]
    pub keep_last_per_phase: usize,
    #[serde(default)]
    pub max_age_hours: Option<u64>,
    #[serde(default)]
    pub auto_prune_on_completion: bool,
}

impl Default for WorkflowCheckpointRetentionConfig {
    fn default() -> Self {
        Self {
            keep_last_per_phase: default_keep_last_per_phase(),
            max_age_hours: None,
            auto_prune_on_completion: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerDefinition {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Transport type: "stdio" (default) or "http".
    #[serde(default)]
    pub transport: Option<String>,
    /// HTTP endpoint URL. Required when transport is "http".
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// OAuth broker configuration for HTTP-transport MCP servers. When set,
    /// the daemon resolves a bearer token (caching under
    /// `~/.animus/<scope>/mcp-oauth-cache/`) and injects it as an
    /// `Authorization: Bearer <token>` header into the additional MCP
    /// server entry passed to the agent. Only valid with `transport: http`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OauthConfig>,
}

/// OAuth flow shape attached to an HTTP-transport MCP server.
///
/// All credential material is read from env vars (`*_env` fields) rather
/// than baked into the YAML, so workflow files stay safe to commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OauthConfig {
    pub flow: OauthFlow,
    /// Token endpoint for `client_credentials` and `refresh_token` flows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    /// Env var name that holds the OAuth client id (required for
    /// `client_credentials` and `refresh_token`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id_env: Option<String>,
    /// Env var name that holds the OAuth client secret (required for
    /// `client_credentials`; optional for `refresh_token`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_env: Option<String>,
    /// Env var name that holds a long-lived refresh token (required for
    /// `refresh_token`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token_env: Option<String>,
    /// Env var name that holds a pre-baked bearer token (required for
    /// `manual_bearer`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_env: Option<String>,
    /// OAuth scopes requested at token exchange.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Optional `audience` parameter (e.g. for Auth0-style flows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    /// When false, disable the on-disk token cache and re-fetch on every
    /// contract assembly. Defaults to true.
    #[serde(default = "default_oauth_cache")]
    pub cache: bool,
    /// Pre-registered OAuth client id for the interactive
    /// `authorization_code` flow. When unset, the daemon performs Dynamic
    /// Client Registration (RFC 7591) against the discovered registration
    /// endpoint. Ignored by the machine-to-machine flows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

fn default_oauth_cache() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OauthFlow {
    ClientCredentials,
    RefreshToken,
    ManualBearer,
    /// Interactive OAuth 2.1 authorization-code + PKCE flow. The daemon
    /// drives discovery, Dynamic Client Registration, browser login, and
    /// token exchange via `animus mcp auth <server>`, then repoints the
    /// agent at a local auth-free stdio proxy (`animus-mcp-proxy`) instead
    /// of injecting a bearer header. Tokens persist in the OS keychain.
    AuthorizationCode,
}

impl OauthFlow {
    pub fn as_str(self) -> &'static str {
        match self {
            OauthFlow::ClientCredentials => "client_credentials",
            OauthFlow::RefreshToken => "refresh_token",
            OauthFlow::ManualBearer => "manual_bearer",
            OauthFlow::AuthorizationCode => "authorization_code",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PhaseMcpBinding {
    #[serde(default)]
    pub servers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub executable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mcp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_write: Option<bool>,
    #[serde(default)]
    pub context_window: Option<usize>,
    #[serde(default)]
    pub base_args: Vec<String>,
    #[serde(default)]
    pub supports_streaming: Option<bool>,
    #[serde(default)]
    pub supports_tool_use: Option<bool>,
    #[serde(default)]
    pub supports_vision: Option<bool>,
    #[serde(default)]
    pub supports_long_context: Option<bool>,
    #[serde(default)]
    pub read_only_flag: Option<String>,
    #[serde(default)]
    pub response_schema_flag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskIntegrationConfig {
    pub provider: String,
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitIntegrationConfig {
    pub provider: String,
    #[serde(default)]
    pub base_branch: Option<String>,
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntegrationsConfig {
    #[serde(default)]
    pub tasks: Option<TaskIntegrationConfig>,
    #[serde(default)]
    pub git: Option<GitIntegrationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSchedule {
    pub id: String,
    #[serde(default)]
    pub cron: String,
    #[serde(default)]
    pub workflow_ref: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default = "default_schedule_enabled")]
    pub enabled: bool,
}

pub(crate) fn default_schedule_enabled() -> bool {
    true
}

/// Type of event trigger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    /// Watch local filesystem paths for changes and fire on modification.
    FileWatcher,
    /// Inbound HTTP webhook (generic).
    Webhook,
    /// GitHub webhook with event filtering.
    GithubWebhook,
    /// External trigger backend plugin (Slack, file watchers, custom adapters).
    ///
    /// The daemon spawns the plugin via the stdio plugin host and forwards
    /// `trigger/event` notifications into the same `pending_events` queue
    /// used by webhook triggers. Plugin-routed events are drained by
    /// `TriggerDispatch::process_due_triggers` each tick.
    Plugin,
}

fn default_trigger_enabled() -> bool {
    true
}

/// An event-driven trigger that enqueues a workflow when an external event fires.
///
/// Triggers live in workflow YAML alongside `schedules:` and are processed each
/// daemon tick after the cron schedule block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTrigger {
    /// Unique identifier for this trigger within the project.
    pub id: String,
    /// The kind of event source.
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
    /// Workflow to enqueue when the trigger fires.
    #[serde(default)]
    pub workflow_ref: Option<String>,
    /// Whether this trigger is active.
    #[serde(default = "default_trigger_enabled")]
    pub enabled: bool,
    /// Type-specific configuration (paths, debounce, etc.).
    #[serde(default)]
    pub config: Value,
    /// Optional static input forwarded to the spawned workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
}

/// Parsed configuration for a `file_watcher` trigger.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileWatcherTriggerConfig {
    /// Glob patterns (relative to project root) to watch.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Debounce window in seconds before re-dispatching. Defaults to 5.
    #[serde(default = "default_debounce_secs")]
    pub debounce_secs: u64,
    /// Glob patterns to ignore (relative to project root).
    #[serde(default)]
    pub ignore: Vec<String>,
}

pub(crate) fn default_debounce_secs() -> u64 {
    5
}

impl FileWatcherTriggerConfig {
    pub fn from_value(value: &Value) -> Self {
        Self::try_from_value(value).unwrap_or_default()
    }

    pub fn try_from_value(value: &Value) -> Result<Self, serde_json::Error> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone())
    }
}

fn default_max_triggers_per_minute() -> u32 {
    10
}

/// Parsed configuration for a `webhook` (or `github_webhook`) trigger.
///
/// The daemon HTTP server registers a `POST /triggers/{id}` route for each
/// enabled webhook trigger.  Requests are validated against an optional
/// HMAC-SHA256 signature and rate-limited to `max_triggers_per_minute`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebhookTriggerConfig {
    /// Environment variable whose value is used as the HMAC-SHA256 signing
    /// secret.  When set, the handler validates the request signature header
    /// (`sha256=<hex>`) as defined by the installed webhook trigger plugin.
    /// When absent, signature verification is skipped.
    #[serde(default)]
    pub secret_env: Option<String>,
    /// Maximum dispatches allowed in any rolling 60-second window.
    /// Requests exceeding this limit receive HTTP 429.  Default: 10.
    #[serde(default = "default_max_triggers_per_minute")]
    pub max_triggers_per_minute: u32,
}

impl WebhookTriggerConfig {
    pub fn from_value(value: &Value) -> Self {
        Self::try_from_value(value).unwrap_or_default()
    }

    pub fn try_from_value(value: &Value) -> Result<Self, serde_json::Error> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone())
    }
}

pub(crate) fn default_visible() -> bool {
    true
}

pub(crate) fn default_keep_last_per_phase() -> usize {
    crate::workflow::DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default)]
    pub active_hours: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_routing: Option<protocol::PhaseRoutingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<protocol::McpRuntimeConfig>,
    /// Fleet-level daily spend cap. Distinct from per-workflow / per-phase
    /// [`BudgetConfig`]: this bounds the daemon's TOTAL rolling-24h spend
    /// and pauses new dispatch when crossed. The scoped daemon runtime
    /// config (`max_daily_usd`, set by `animus daemon config
    /// --max-daily-usd`) takes precedence over this YAML block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<DaemonBudgetConfig>,
}

/// Fleet/daemon-level daily spend cap declared under the workflow YAML
/// `daemon.budget` block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DaemonBudgetConfig {
    /// USD ceiling for the daemon's total rolling-24h spend. `None` (or a
    /// non-positive value) means no fleet cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd_per_day: Option<f64>,
    /// Action when the daily cap is crossed. Only `pause` is honored today
    /// (new dispatch stops until spend ages out of the window or the cap is
    /// raised); the field is carried for forward compatibility.
    #[serde(default)]
    pub on_exceed: BudgetOnExceed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentChannelConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_chars: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfig {
    pub schema: String,
    pub version: u32,
    pub default_workflow_ref: String,
    #[serde(default)]
    pub phase_catalog: BTreeMap<String, PhaseUiDefinition>,
    #[serde(default)]
    pub workflows: Vec<WorkflowDefinition>,
    #[serde(default)]
    pub checkpoint_retention: WorkflowCheckpointRetentionConfig,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub phase_definitions: BTreeMap<String, PhaseExecutionDefinition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_profiles: BTreeMap<String, AgentProfileOverlay>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_channels: BTreeMap<String, AgentChannelConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_servers: BTreeMap<String, McpServerDefinition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub phase_mcp_bindings: BTreeMap<String, PhaseMcpBinding>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrations: Option<IntegrationsConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<WorkflowSchedule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<WorkflowTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<DaemonConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, SecretRef>,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        crate::builtins::builtin_workflow_config()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowConfigSource {
    Json,
    Yaml,
    Builtin,
    BuiltinFallback,
}

impl WorkflowConfigSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Builtin => "builtin",
            Self::BuiltinFallback => "builtin_fallback",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfigMetadata {
    pub schema: String,
    pub version: u32,
    pub hash: String,
    pub source: WorkflowConfigSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedWorkflowConfig {
    pub config: WorkflowConfig,
    pub metadata: WorkflowConfigMetadata,
    pub path: std::path::PathBuf,
}
