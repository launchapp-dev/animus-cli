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
            (
                "code-review".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("swe".to_string()),
                    directive: Some("Perform a rigorous code review pass. Fix defects, tighten edge cases, and improve maintainability.".to_string()),
                    system_prompt: None,
                    runtime: None,
                    capabilities: None,
                    output_contract: None,
                    output_json_schema: None,
                    decision_contract: Some(PhaseDecisionContract {
                        required_evidence: vec![crate::types::PhaseEvidenceKind::CodeReviewClean],
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
            (
                "testing".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("swe".to_string()),
                    directive: Some("Add or update tests and validate behavior. Ensure failures are addressed before finishing.".to_string()),
                    system_prompt: None,
                    runtime: None,
                    capabilities: None,
                    output_contract: None,
                    output_json_schema: None,
                    decision_contract: Some(PhaseDecisionContract {
                        required_evidence: vec![crate::types::PhaseEvidenceKind::TestsPassed],
                        min_confidence: 0.8,
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
        merge_workflow_runtime_overlay(&mut config, &loaded_workflow.config);
        validate_agent_runtime_config(&config)?;

        return Ok(LoadedAgentRuntimeConfig {
            metadata: AgentRuntimeMetadata {
                schema: config.schema.clone(),
                version: config.version,
                hash: agent_runtime_config_hash(&config),
                source: AgentRuntimeSource::WorkflowYaml,
            },
            config,
            path: loaded_workflow.path,
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
mod tests;
