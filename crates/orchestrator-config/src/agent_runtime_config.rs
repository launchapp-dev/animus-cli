use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const AGENT_RUNTIME_CONFIG_SCHEMA_ID: &str = "ao.agent-runtime-config.v2";
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
    pub model: Option<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentCapabilities {
    #[serde(flatten)]
    pub flags: BTreeMap<String, bool>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub tool_policy: AgentToolPolicy,
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
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
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
    pub agents: BTreeMap<String, AgentProfile>,
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
    CommandCwdMode::ProjectRoot
}

fn default_success_exit_codes() -> Vec<i32> {
    vec![0]
}

fn lookup_case_insensitive<'a, T>(map: &'a BTreeMap<String, T>, key: &str) -> Option<&'a T> {
    map.iter().find(|(candidate, _)| candidate.eq_ignore_ascii_case(key)).map(|(_, value)| value)
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
        lookup_case_insensitive(&self.phases, phase_id).or_else(|| self.phases.get("default"))
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
    let swe_mcp_servers = vec!["ao".to_string()];
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
                    description: "Default workflow phase agent profile".to_string(),
                    system_prompt: "You are the workflow phase execution agent. Produce deterministic, repository-safe outputs and keep changes scoped to the active phase.".to_string(),
                    role: None,
                    mcp_servers: Vec::new(),
                    tool_policy: AgentToolPolicy::default(),
                    skills: vec![],
                    capabilities: BTreeMap::new(),
                    tool: None,
                    model: None,
                    fallback_models: vec![],
                    reasoning_effort: None,
                    web_search: None,
                    network_access: None,
                    timeout_secs: None,
                    max_attempts: None,
                    extra_args: vec![],
                    codex_config_overrides: vec![],
                    max_continuations: None,
                    mcp_server_configs: None,
                    structured_capabilities: None,
                    project_overrides: None,
                },
            ),
            (
                "implementation".to_string(),
                AgentProfile {
                    description: "Compatibility alias for the software engineer persona.".to_string(),
                    system_prompt: "You are the software engineer execution agent. Implement production-ready code changes, add or update tests, and perform rigorous code review while keeping edits minimal and verifiable.".to_string(),
                    role: Some("software_engineer".to_string()),
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
                    model: None,
                    fallback_models: vec![],
                    reasoning_effort: None,
                    web_search: None,
                    network_access: None,
                    timeout_secs: None,
                    max_attempts: None,
                    extra_args: vec![],
                    codex_config_overrides: vec![],
                    max_continuations: None,
                    mcp_server_configs: None,
                    structured_capabilities: None,
                    project_overrides: None,
                },
            ),
            (
                "em".to_string(),
                AgentProfile {
                    description: "Engineering Manager persona for prioritization, queue management, and scheduling.".to_string(),
                    system_prompt: "You are the Engineering Manager agent. Prioritize work, manage queue health, sequence delivery safely, and keep execution plans realistic and dependency-aware.".to_string(),
                    role: Some("engineering_manager".to_string()),
                    mcp_servers: vec!["ao".to_string()],
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
                    model: None,
                    fallback_models: vec![],
                    reasoning_effort: None,
                    web_search: None,
                    network_access: None,
                    timeout_secs: None,
                    max_attempts: None,
                    extra_args: vec![],
                    codex_config_overrides: vec![],
                    max_continuations: None,
                    mcp_server_configs: None,
                    structured_capabilities: None,
                    project_overrides: None,
                },
            ),
            (
                "po".to_string(),
                AgentProfile {
                    description: "Product Owner persona for requirements, vision, acceptance criteria, and deliverable validation.".to_string(),
                    system_prompt: "You are the Product Owner agent. Refine requirements into clear acceptance criteria, align work to product vision, and validate deliverables against user outcomes.".to_string(),
                    role: Some("product_owner".to_string()),
                    mcp_servers: vec!["ao".to_string()],
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
                    model: None,
                    fallback_models: vec![],
                    reasoning_effort: None,
                    web_search: None,
                    network_access: None,
                    timeout_secs: None,
                    max_attempts: None,
                    extra_args: vec![],
                    codex_config_overrides: vec![],
                    max_continuations: None,
                    mcp_server_configs: None,
                    structured_capabilities: None,
                    project_overrides: None,
                },
            ),
            (
                "swe".to_string(),
                AgentProfile {
                    description: "Software Engineer persona for implementation, testing, and code review.".to_string(),
                    system_prompt: "You are the software engineer execution agent. Implement production-ready code changes, add or update tests, and perform rigorous code review while keeping edits minimal and verifiable.".to_string(),
                    role: Some("software_engineer".to_string()),
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
                    model: None,
                    fallback_models: vec![],
                    reasoning_effort: None,
                    web_search: None,
                    network_access: None,
                    timeout_secs: None,
                    max_attempts: None,
                    extra_args: vec![],
                    codex_config_overrides: vec![],
                    max_continuations: None,
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
                        required_evidence: vec![crate::types::PhaseEvidenceKind::FilesModified],
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
                },
            ),
        ]),
        cli_tools: BTreeMap::new(),
    }
}

pub fn agent_runtime_config_path(project_root: &Path) -> PathBuf {
    let base = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".ao"));
    base.join("state").join(AGENT_RUNTIME_CONFIG_FILE_NAME)
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

        for entry in registry.entries_for_source(crate::PackRegistrySource::Bundled) {
            let Some(pack) = entry.loaded_manifest() else {
                continue;
            };
            if let Some(overlay) = crate::load_pack_agent_runtime_overlay(pack)? {
                merge_agent_runtime_overlay(&mut config, &overlay);
                if let Some(pack_root) = entry.pack_root.as_ref() {
                    path = pack_root.clone();
                }
            }
        }

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

    Err(anyhow!("agent runtime config is missing. Define runtime in .ao/workflows.yaml or .ao/workflows/*.yaml"))
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
                base.agents.insert(agent_id.clone(), profile.clone());
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
        entry.supports_mcp = Some(definition.supports_mcp);
        entry.supports_file_editing = Some(definition.supports_write);
        entry.max_context_tokens = definition.context_window;
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
                base.agents.insert(agent_id.clone(), profile.clone());
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

fn merge_agent_profile(base: &mut AgentProfile, overlay: &AgentProfile) {
    if !overlay.description.trim().is_empty() {
        base.description = overlay.description.clone();
    }
    if !overlay.system_prompt.trim().is_empty() {
        base.system_prompt = overlay.system_prompt.clone();
    }
    if overlay.role.is_some() {
        base.role = overlay.role.clone();
    }
    if !overlay.mcp_servers.is_empty() {
        base.mcp_servers = overlay.mcp_servers.clone();
    }
    if !overlay.tool_policy.allow.is_empty() || !overlay.tool_policy.deny.is_empty() {
        base.tool_policy = overlay.tool_policy.clone();
    }
    if !overlay.skills.is_empty() {
        base.skills = overlay.skills.clone();
    }
    if !overlay.capabilities.is_empty() {
        base.capabilities = overlay.capabilities.clone();
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
    if overlay.model.is_some() {
        base.model = overlay.model.clone();
    }
    if !overlay.fallback_models.is_empty() {
        base.fallback_models = overlay.fallback_models.clone();
    }
    if overlay.reasoning_effort.is_some() {
        base.reasoning_effort = overlay.reasoning_effort.clone();
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
    if !overlay.extra_args.is_empty() {
        base.extra_args = overlay.extra_args.clone();
    }
    if !overlay.codex_config_overrides.is_empty() {
        base.codex_config_overrides = overlay.codex_config_overrides.clone();
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
        agent_profiles: config.agents.clone(),
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
                            supports_mcp: cli_tool.supports_mcp.unwrap_or(false),
                            supports_write: cli_tool.supports_file_editing.unwrap_or(false),
                            context_window: cli_tool.max_context_tokens,
                            base_args: Vec::new(),
                        },
                    )
                })
            })
            .collect(),
        integrations: None,
        schedules: Vec::new(),
        daemon: None,
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
                && command.cwd_path.as_deref().map(str::trim).filter(|value| !value.is_empty()).is_none()
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

        if profile.tool.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].tool must not be empty", agent_id));
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

        if profile.role.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].role must not be empty", agent_id));
        }

        if profile.mcp_servers.iter().any(|server| server.trim().is_empty()) {
            return Err(anyhow!("agents['{}'].mcp_servers must not contain empty values", agent_id));
        }

        if profile.tool_policy.allow.iter().chain(profile.tool_policy.deny.iter()).any(|value| value.trim().is_empty())
        {
            return Err(anyhow!("agents['{}'].tool_policy must not contain empty patterns", agent_id));
        }

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
    use std::env;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let original = env::var(key).ok();
            env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.as_deref() {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
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
schema = "ao.pack.v1"
id = "{pack_id}"
version = "{version}"
kind = "domain-pack"
title = "{pack_id}"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["ao.task"]
default_kind = "ao.task"

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
        let config = load_agent_runtime_config(temp.path()).expect("bundled runtime defaults should load");

        assert_eq!(config.phase_agent_id("requirements"), Some("po"));
        assert_eq!(config.phase_agent_id("implementation"), Some("swe"));
        assert_eq!(config.phase_agent_id("triage"), Some("triager"));
        assert_eq!(config.phase_agent_id("refine-requirements"), Some("requirements-refiner"));
        assert_eq!(config.phase_agent_id("requirement-task-generation"), Some("requirements-planner"));
        assert_eq!(config.phase_agent_id("requirement-workflow-bootstrap"), Some("requirements-planner"));
        assert_eq!(config.phase_agent_id("po-review"), Some("po-reviewer"));
        assert_eq!(config.phase_agent_id("code-review"), Some("swe"));
        assert_eq!(config.phase_agent_id("testing"), Some("swe"));
        assert_eq!(config.phase_mode("unit-test"), Some(PhaseExecutionMode::Command));
        assert_eq!(config.phase_mode("lint"), Some(PhaseExecutionMode::Command));
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
        workflow.agent_profiles.insert("workflow-test-agent".to_string(), overlay_agent);
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
            },
        );
        workflow.tools.insert(
            "custom-runner".to_string(),
            crate::workflow_config::ToolDefinition {
                executable: "custom-runner-bin".to_string(),
                supports_mcp: true,
                supports_write: true,
                context_window: Some(42_000),
                base_args: vec![],
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
            &crate::machine_installed_packs_dir().join("ao.review").join("0.2.0"),
            "ao.review",
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
            Some(vec![crate::types::PhaseEvidenceKind::FilesModified])
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

        assert!(em.mcp_servers.iter().any(|server| server == "ao"));
        assert!(po.mcp_servers.iter().any(|server| server == "ao"));
        assert!(swe.mcp_servers.iter().any(|server| server == "ao"));
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
        let config = load_agent_runtime_config(temp.path()).expect("bundled runtime defaults should load");
        assert!(config.is_structured_output_phase("code-review"));
        assert!(config.is_structured_output_phase("implementation"));
        assert!(config.is_structured_output_phase("testing"));
    }

    #[test]
    fn structured_output_phase_accepts_trimmed_phase_ids() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let temp = tempfile::tempdir().expect("tempdir");
        let config = load_agent_runtime_config(temp.path()).expect("bundled runtime defaults should load");
        assert!(config.is_structured_output_phase(" implementation "));
        assert!(config.is_structured_output_phase(" CODE-REVIEW "));
        assert!(config.is_structured_output_phase(" testing "));
    }

    #[test]
    fn bundled_pack_runtime_supports_extended_task_requirement_and_review_phases() {
        let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let temp = tempfile::tempdir().expect("tempdir");
        let config = load_agent_runtime_config(temp.path()).expect("bundled runtime defaults should load");

        assert_eq!(config.phase_agent_id("triage"), Some("triager"));
        assert_eq!(config.phase_agent_id("refine-requirements"), Some("requirements-refiner"));
        assert_eq!(config.phase_agent_id("requirement-task-generation"), Some("requirements-planner"));
        assert_eq!(config.phase_agent_id("requirement-workflow-bootstrap"), Some("requirements-planner"));
        assert_eq!(config.phase_agent_id("po-review"), Some("po-reviewer"));
        assert_eq!(config.phase_mode("unit-test"), Some(PhaseExecutionMode::Command));
        assert_eq!(config.phase_mode("lint"), Some(PhaseExecutionMode::Command));
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
    fn command_mode_defaults_cwd_to_project_root_and_exit_code_zero() {
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
        assert_eq!(cmd.cwd_mode, CommandCwdMode::ProjectRoot);
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
            },
        );
        let err = validate_agent_runtime_config(&config).unwrap_err();
        assert!(err.to_string().contains("env must not contain empty keys"));
    }

    #[test]
    fn legacy_config_without_new_fields_deserializes_with_none_defaults() {
        let json = r#"{
            "schema": "ao.agent-runtime-config.v2",
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
            "ao".to_string(),
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
        assert_eq!(mcp_configs["ao"].source, AgentMcpServerSource::Builtin);

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
        let policy = AgentToolPolicy { allow: vec!["ao.*".to_string()], deny: vec![] };
        assert!(policy.is_tool_permitted("ao.task.list"));
        assert!(policy.is_tool_permitted("ao.workflow.run"));
        assert!(policy.is_tool_permitted("ao.x"));
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
        };
        let json = serde_json::to_string(&definition).expect("serialize");
        assert!(!json.contains("system_prompt"));

        let with_prompt =
            PhaseExecutionDefinition { system_prompt: Some("My custom prompt".to_string()), ..definition };
        let json = serde_json::to_string(&with_prompt).expect("serialize");
        assert!(json.contains("system_prompt"));
        assert!(json.contains("My custom prompt"));
    }
}
