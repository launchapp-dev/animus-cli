use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const AGENT_RUNTIME_CONFIG_SCHEMA_ID: &str = "ao.agent-runtime-config.v2";
pub const AGENT_RUNTIME_CONFIG_VERSION: u32 = 2;
pub const AGENT_RUNTIME_CONFIG_FILE_NAME: &str = "agent-runtime-config.v2.json";
const BUILTIN_AGENT_RUNTIME_CONFIG_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/config/agent-runtime-config.v2.json"
));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseOutputContract {
    pub kind: String,
    #[serde(default)]
    pub required_fields: Vec<String>,
}

impl PhaseOutputContract {
    pub fn requires_field(&self, field: &str) -> bool {
        self.required_fields
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(field))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseExecutionMode {
    Agent,
    Command,
    Manual,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseManualDefinition {
    pub instructions: String,
    #[serde(default)]
    pub approval_note_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseExecutionDefinition {
    pub mode: PhaseExecutionMode,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub directive: Option<String>,
    #[serde(default)]
    pub runtime: Option<AgentRuntimeOverrides>,
    #[serde(default)]
    pub output_contract: Option<PhaseOutputContract>,
    #[serde(default)]
    pub output_json_schema: Option<Value>,
    #[serde(default)]
    pub command: Option<PhaseCommandDefinition>,
    #[serde(default)]
    pub manual: Option<PhaseManualDefinition>,
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
}

impl Default for AgentRuntimeConfig {
    fn default() -> Self {
        builtin_agent_runtime_config()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeSource {
    Json,
    Builtin,
    BuiltinFallback,
}

impl AgentRuntimeSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Json => "json",
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
    map.iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
}

fn trim_nonempty(value: Option<&str>) -> Option<&str> {
    value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
}

fn normalized_nonempty_values(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

impl AgentRuntimeConfig {
    pub fn has_phase_definition(&self, phase_id: &str) -> bool {
        self.phase_execution(phase_id).is_some()
    }

    pub fn phase_execution(&self, phase_id: &str) -> Option<&PhaseExecutionDefinition> {
        lookup_case_insensitive(&self.phases, phase_id).or_else(|| self.phases.get("default"))
    }

    pub fn phase_mode(&self, phase_id: &str) -> Option<PhaseExecutionMode> {
        self.phase_execution(phase_id)
            .map(|definition| definition.mode.clone())
    }

    pub fn phase_agent_id(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(
            self.phase_execution(phase_id)
                .and_then(|definition| definition.agent_id.as_deref()),
        )
    }

    pub fn agent_profile(&self, agent_id: &str) -> Option<&AgentProfile> {
        lookup_case_insensitive(&self.agents, agent_id)
    }

    pub fn phase_agent_profile(&self, phase_id: &str) -> Option<&AgentProfile> {
        self.phase_agent_id(phase_id)
            .and_then(|agent_id| self.agent_profile(agent_id))
    }

    pub fn phase_system_prompt(&self, phase_id: &str) -> Option<&str> {
        self.phase_agent_profile(phase_id)
            .map(|profile| profile.system_prompt.trim())
            .filter(|value| !value.is_empty())
    }

    pub fn phase_tool_override(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(
            self.phase_execution(phase_id)
                .and_then(|definition| definition.runtime.as_ref())
                .and_then(|runtime| runtime.tool.as_deref()),
        )
        .or_else(|| {
            trim_nonempty(
                self.phase_agent_profile(phase_id)
                    .and_then(|profile| profile.tool.as_deref()),
            )
        })
    }

    pub fn phase_model_override(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(
            self.phase_execution(phase_id)
                .and_then(|definition| definition.runtime.as_ref())
                .and_then(|runtime| runtime.model.as_deref()),
        )
        .or_else(|| {
            trim_nonempty(
                self.phase_agent_profile(phase_id)
                    .and_then(|profile| profile.model.as_deref()),
            )
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

    pub fn phase_reasoning_effort(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(
            self.phase_execution(phase_id)
                .and_then(|definition| definition.runtime.as_ref())
                .and_then(|runtime| runtime.reasoning_effort.as_deref()),
        )
        .or_else(|| {
            trim_nonempty(
                self.phase_agent_profile(phase_id)
                    .and_then(|profile| profile.reasoning_effort.as_deref()),
            )
        })
    }

    pub fn phase_web_search(&self, phase_id: &str) -> Option<bool> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .and_then(|runtime| runtime.web_search)
            .or_else(|| {
                self.phase_agent_profile(phase_id)
                    .and_then(|profile| profile.web_search)
            })
    }

    pub fn phase_network_access(&self, phase_id: &str) -> Option<bool> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .and_then(|runtime| runtime.network_access)
            .or_else(|| {
                self.phase_agent_profile(phase_id)
                    .and_then(|profile| profile.network_access)
            })
    }

    pub fn phase_timeout_secs(&self, phase_id: &str) -> Option<u64> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .and_then(|runtime| runtime.timeout_secs)
            .or_else(|| {
                self.phase_agent_profile(phase_id)
                    .and_then(|profile| profile.timeout_secs)
            })
    }

    pub fn phase_max_attempts(&self, phase_id: &str) -> Option<usize> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.runtime.as_ref())
            .and_then(|runtime| runtime.max_attempts)
            .or_else(|| {
                self.phase_agent_profile(phase_id)
                    .and_then(|profile| profile.max_attempts)
            })
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
        self.phase_execution(phase_id)
            .and_then(|definition| definition.output_json_schema.as_ref())
    }

    pub fn phase_directive(&self, phase_id: &str) -> Option<&str> {
        trim_nonempty(
            self.phase_execution(phase_id)
                .and_then(|definition| definition.directive.as_deref()),
        )
    }

    pub fn phase_output_contract(&self, phase_id: &str) -> Option<&PhaseOutputContract> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.output_contract.as_ref())
    }

    pub fn phase_command(&self, phase_id: &str) -> Option<&PhaseCommandDefinition> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.command.as_ref())
    }

    pub fn phase_manual(&self, phase_id: &str) -> Option<&PhaseManualDefinition> {
        self.phase_execution(phase_id)
            .and_then(|definition| definition.manual.as_ref())
    }

    pub fn is_structured_output_phase(&self, phase_id: &str) -> bool {
        if self.phase_output_contract(phase_id).is_some()
            || self.phase_output_json_schema(phase_id).is_some()
        {
            return true;
        }

        let normalized = phase_id.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return false;
        }

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

    pub fn structured_output_allowed_tools(&self) -> Vec<String> {
        if self.tools_allowlist.is_empty() {
            return vec!["claude".to_string(), "codex".to_string()];
        }

        self.tools_allowlist
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .collect()
    }

    pub fn structured_output_default_tool(&self) -> String {
        let allowlist = self.structured_output_allowed_tools();
        if allowlist
            .iter()
            .any(|tool| tool.eq_ignore_ascii_case("claude"))
        {
            return "claude".to_string();
        }
        if allowlist
            .iter()
            .any(|tool| tool.eq_ignore_ascii_case("codex"))
        {
            return "codex".to_string();
        }
        allowlist
            .first()
            .cloned()
            .unwrap_or_else(|| "claude".to_string())
    }
}

pub fn builtin_agent_runtime_config() -> AgentRuntimeConfig {
    static BUILTIN_CONFIG: OnceLock<AgentRuntimeConfig> = OnceLock::new();
    BUILTIN_CONFIG
        .get_or_init(|| {
            match serde_json::from_str::<AgentRuntimeConfig>(BUILTIN_AGENT_RUNTIME_CONFIG_JSON) {
                Ok(config) if validate_agent_runtime_config(&config).is_ok() => config,
                _ => hardcoded_builtin_agent_runtime_config(),
            }
        })
        .clone()
}

fn hardcoded_builtin_agent_runtime_config() -> AgentRuntimeConfig {
    let implementation_output_contract = PhaseOutputContract {
        kind: "implementation_result".to_string(),
        required_fields: vec!["commit_message".to_string()],
    };

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
                },
            ),
            (
                "implementation".to_string(),
                AgentProfile {
                    description: "Implementation-focused coding agent profile".to_string(),
                    system_prompt: "You are the implementation agent. Make minimal production-ready code changes and emit machine-readable completion payloads required by policy.".to_string(),
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
                    runtime: None,
                    output_contract: None,
                    output_json_schema: None,
                    command: None,
                    manual: None,
                },
            ),
            (
                "requirements".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some("Clarify implementation scope, constraints, and acceptance criteria. Update docs and implementation notes as needed.".to_string()),
                    runtime: None,
                    output_contract: None,
                    output_json_schema: None,
                    command: None,
                    manual: None,
                },
            ),
            (
                "research".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some(
                        "Gather external and codebase evidence needed to de-risk the next implementation step. Treat greenfield repositories as valid and provide assumptions/plan artifacts when source is sparse. Keep discovery targeted to first-party code and active requirement/task docs; avoid broad scans of dependency or workflow checkpoint directories. Only emit research_required for true external blockers."
                            .to_string(),
                    ),
                    runtime: Some(AgentRuntimeOverrides {
                        web_search: Some(true),
                        timeout_secs: Some(900),
                        ..AgentRuntimeOverrides::default()
                    }),
                    output_contract: None,
                    output_json_schema: None,
                    command: None,
                    manual: None,
                },
            ),
            (
                "ux-research".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some("Produce a UX brief from requirements and user flows. Identify key screens, interactions, and accessibility constraints.".to_string()),
                    runtime: None,
                    output_contract: None,
                    output_json_schema: None,
                    command: None,
                    manual: None,
                },
            ),
            (
                "wireframe".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some("Create concrete UI mockups/wireframes in the repository under mockups/. Prefer production-like React-oriented layouts and realistic states.".to_string()),
                    runtime: None,
                    output_contract: None,
                    output_json_schema: None,
                    command: None,
                    manual: None,
                },
            ),
            (
                "mockup-review".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some("Review mockups against linked requirements. Resolve mismatches, improve usability, and ensure acceptance criteria traceability.".to_string()),
                    runtime: None,
                    output_contract: None,
                    output_json_schema: None,
                    command: None,
                    manual: None,
                },
            ),
            (
                "implementation".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("implementation".to_string()),
                    directive: Some(
                        "Implement production-quality code for this task. Keep changes focused and executable."
                            .to_string(),
                    ),
                    runtime: None,
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
                    command: None,
                    manual: None,
                },
            ),
            (
                "code-review".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some("Perform a rigorous code review pass. Fix defects, tighten edge cases, and improve maintainability.".to_string()),
                    runtime: None,
                    output_contract: None,
                    output_json_schema: None,
                    command: None,
                    manual: None,
                },
            ),
            (
                "testing".to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: Some("Add or update tests and validate behavior. Ensure failures are addressed before finishing.".to_string()),
                    runtime: None,
                    output_contract: None,
                    output_json_schema: None,
                    command: None,
                    manual: None,
                },
            ),
        ]),
    }
}

pub fn agent_runtime_config_path(project_root: &Path) -> PathBuf {
    project_root
        .join(".ao")
        .join("state")
        .join(AGENT_RUNTIME_CONFIG_FILE_NAME)
}

pub fn legacy_agent_runtime_config_path(project_root: &Path) -> PathBuf {
    project_root
        .join(".ao")
        .join("state")
        .join("agent-runtime-config.v1.json")
}

pub fn ensure_agent_runtime_config_file(project_root: &Path) -> Result<()> {
    let path = agent_runtime_config_path(project_root);
    if path.exists() {
        return Ok(());
    }

    write_agent_runtime_config(project_root, &builtin_agent_runtime_config())
}

pub fn load_agent_runtime_config(project_root: &Path) -> Result<AgentRuntimeConfig> {
    Ok(load_agent_runtime_config_with_metadata(project_root)?.config)
}

pub fn load_agent_runtime_config_with_metadata(
    project_root: &Path,
) -> Result<LoadedAgentRuntimeConfig> {
    let path = agent_runtime_config_path(project_root);
    if !path.exists() {
        let legacy = legacy_agent_runtime_config_path(project_root);
        if legacy.exists() {
            return Err(anyhow!(
                "agent runtime config v2 is required at {} (found legacy file at {}). Run `ao workflow config migrate-v2 --json`",
                path.display(),
                legacy.display()
            ));
        }

        return Err(anyhow!(
            "agent runtime config v2 file is missing at {}. Run `ao workflow config migrate-v2 --json` or initialize a new project",
            path.display()
        ));
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read agent runtime config at {}", path.display()))?;

    let config = serde_json::from_str::<AgentRuntimeConfig>(&content)
        .with_context(|| format!("invalid agent runtime config JSON at {}", path.display()))?;

    validate_agent_runtime_config(&config)?;

    Ok(LoadedAgentRuntimeConfig {
        metadata: AgentRuntimeMetadata {
            schema: config.schema.clone(),
            version: config.version,
            hash: agent_runtime_config_hash(&config),
            source: AgentRuntimeSource::Json,
        },
        config,
        path,
    })
}

pub fn load_agent_runtime_config_or_default(project_root: &Path) -> AgentRuntimeConfig {
    match load_agent_runtime_config(project_root) {
        Ok(config) => config,
        Err(_) => builtin_agent_runtime_config(),
    }
}

pub fn write_agent_runtime_config(project_root: &Path, config: &AgentRuntimeConfig) -> Result<()> {
    validate_agent_runtime_config(config)?;

    let path = agent_runtime_config_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let payload = serde_json::to_string_pretty(config)?;
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("agent-runtime-config"),
        Uuid::new_v4()
    ));

    {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(payload.as_bytes())?;
        file.sync_all()?;
    }

    fs::rename(&tmp_path, &path).with_context(|| {
        format!(
            "failed to atomically move {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;

    Ok(())
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
            return Err(anyhow!(
                "phases['{}'].directive must not be empty when set",
                phase_id
            ));
        }
    }

    if let Some(schema) = definition.output_json_schema.as_ref() {
        if !schema.is_object() {
            return Err(anyhow!(
                "phases['{}'].output_json_schema must be a JSON object",
                phase_id
            ));
        }
    }

    if let Some(contract) = definition.output_contract.as_ref() {
        if contract.kind.trim().is_empty() {
            return Err(anyhow!(
                "phases['{}'].output_contract.kind must not be empty",
                phase_id
            ));
        }
        if contract
            .required_fields
            .iter()
            .any(|field| field.trim().is_empty())
        {
            return Err(anyhow!(
                "phases['{}'].output_contract.required_fields must not contain empty values",
                phase_id
            ));
        }
    }

    match definition.mode {
        PhaseExecutionMode::Agent => {
            let Some(agent_id) = trim_nonempty(definition.agent_id.as_deref()) else {
                return Err(anyhow!(
                    "phases['{}'] mode 'agent' requires non-empty agent_id",
                    phase_id
                ));
            };

            if lookup_case_insensitive(&config.agents, agent_id).is_none() {
                return Err(anyhow!(
                    "phases['{}'] references unknown agent '{}'",
                    phase_id,
                    agent_id
                ));
            }

            if definition.command.is_some() {
                return Err(anyhow!(
                    "phases['{}'] mode 'agent' must not include command block",
                    phase_id
                ));
            }

            if definition.manual.is_some() {
                return Err(anyhow!(
                    "phases['{}'] mode 'agent' must not include manual block",
                    phase_id
                ));
            }
        }
        PhaseExecutionMode::Command => {
            let Some(command) = definition.command.as_ref() else {
                return Err(anyhow!(
                    "phases['{}'] mode 'command' requires command block",
                    phase_id
                ));
            };

            if command.program.trim().is_empty() {
                return Err(anyhow!(
                    "phases['{}'].command.program must not be empty",
                    phase_id
                ));
            }

            if command.args.iter().any(|value| value.trim().is_empty()) {
                return Err(anyhow!(
                    "phases['{}'].command.args must not contain empty values",
                    phase_id
                ));
            }

            if command.env.iter().any(|(key, _)| key.trim().is_empty()) {
                return Err(anyhow!(
                    "phases['{}'].command.env must not contain empty keys",
                    phase_id
                ));
            }

            if command.success_exit_codes.is_empty() {
                return Err(anyhow!(
                    "phases['{}'].command.success_exit_codes must include at least one code",
                    phase_id
                ));
            }

            if matches!(command.cwd_mode, CommandCwdMode::Path)
                && command
                    .cwd_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
            {
                return Err(anyhow!(
                    "phases['{}'].command.cwd_path must be set for cwd_mode='path'",
                    phase_id
                ));
            }

            if definition.agent_id.is_some() {
                return Err(anyhow!(
                    "phases['{}'] mode 'command' must not include agent_id",
                    phase_id
                ));
            }

            if definition.manual.is_some() {
                return Err(anyhow!(
                    "phases['{}'] mode 'command' must not include manual block",
                    phase_id
                ));
            }
        }
        PhaseExecutionMode::Manual => {
            let Some(manual) = definition.manual.as_ref() else {
                return Err(anyhow!(
                    "phases['{}'] mode 'manual' requires manual block",
                    phase_id
                ));
            };

            if manual.instructions.trim().is_empty() {
                return Err(anyhow!(
                    "phases['{}'].manual.instructions must not be empty",
                    phase_id
                ));
            }

            if definition.agent_id.is_some() {
                return Err(anyhow!(
                    "phases['{}'] mode 'manual' must not include agent_id",
                    phase_id
                ));
            }

            if definition.command.is_some() {
                return Err(anyhow!(
                    "phases['{}'] mode 'manual' must not include command block",
                    phase_id
                ));
            }
        }
    }

    if let Some(runtime) = definition.runtime.as_ref() {
        if runtime
            .tool
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(anyhow!(
                "phases['{}'].runtime.tool must not be empty",
                phase_id
            ));
        }

        if runtime
            .model
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(anyhow!(
                "phases['{}'].runtime.model must not be empty",
                phase_id
            ));
        }

        if runtime
            .fallback_models
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err(anyhow!(
                "phases['{}'].runtime.fallback_models must not contain empty values",
                phase_id
            ));
        }

        if runtime.max_attempts == Some(0) {
            return Err(anyhow!(
                "phases['{}'].runtime.max_attempts must be greater than 0",
                phase_id
            ));
        }

        if runtime.timeout_secs == Some(0) {
            return Err(anyhow!(
                "phases['{}'].runtime.timeout_secs must be greater than 0 when set",
                phase_id
            ));
        }

        if runtime.extra_args.iter().any(|value| value.trim().is_empty()) {
            return Err(anyhow!(
                "phases['{}'].runtime.extra_args must not contain empty values",
                phase_id
            ));
        }

        if runtime
            .codex_config_overrides
            .iter()
            .any(|value| !is_valid_codex_config_override(value.trim()))
        {
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
        return Err(anyhow!(
            "schema must be '{}' (got '{}')",
            AGENT_RUNTIME_CONFIG_SCHEMA_ID,
            config.schema
        ));
    }

    if config.version != AGENT_RUNTIME_CONFIG_VERSION {
        return Err(anyhow!(
            "version must be {} (got {})",
            AGENT_RUNTIME_CONFIG_VERSION,
            config.version
        ));
    }

    if config.tools_allowlist.is_empty()
        || config
            .tools_allowlist
            .iter()
            .all(|tool| tool.trim().is_empty())
    {
        return Err(anyhow!(
            "tools_allowlist must include at least one non-empty command"
        ));
    }

    if config.agents.is_empty() {
        return Err(anyhow!("agents must include at least one profile"));
    }

    for (agent_id, profile) in &config.agents {
        if agent_id.trim().is_empty() {
            return Err(anyhow!("agents contains empty agent id"));
        }

        if profile.system_prompt.trim().is_empty() {
            return Err(anyhow!(
                "agents['{}'].system_prompt must not be empty",
                agent_id
            ));
        }

        if profile
            .tool
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(anyhow!("agents['{}'].tool must not be empty", agent_id));
        }

        if profile
            .model
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(anyhow!("agents['{}'].model must not be empty", agent_id));
        }

        if profile
            .fallback_models
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err(anyhow!(
                "agents['{}'].fallback_models must not contain empty values",
                agent_id
            ));
        }

        if profile.max_attempts == Some(0) {
            return Err(anyhow!(
                "agents['{}'].max_attempts must be greater than 0",
                agent_id
            ));
        }

        if profile.timeout_secs == Some(0) {
            return Err(anyhow!(
                "agents['{}'].timeout_secs must be greater than 0 when set",
                agent_id
            ));
        }

        if profile.extra_args.iter().any(|value| value.trim().is_empty()) {
            return Err(anyhow!(
                "agents['{}'].extra_args must not contain empty values",
                agent_id
            ));
        }

        if profile
            .codex_config_overrides
            .iter()
            .any(|value| !is_valid_codex_config_override(value.trim()))
        {
            return Err(anyhow!(
                "agents['{}'].codex_config_overrides values must use key=value syntax",
                agent_id
            ));
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

    #[test]
    fn missing_config_reports_actionable_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let err = load_agent_runtime_config(temp.path()).expect_err("missing config should fail");
        let message = err.to_string();
        assert!(message.contains("agent runtime config v2 file is missing"));
        assert!(message.contains("migrate-v2"));
    }

    #[test]
    fn ensure_creates_config_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        ensure_agent_runtime_config_file(temp.path()).expect("ensure file");

        let path = agent_runtime_config_path(temp.path());
        assert!(path.exists());
    }

    #[test]
    fn builtin_defaults_expose_phase_definitions() {
        let config = builtin_agent_runtime_config();
        assert_eq!(
            config.phase_agent_id("implementation"),
            Some("implementation")
        );
        assert!(config.phase_output_json_schema("implementation").is_some());
    }

    #[test]
    fn builtin_defaults_mark_review_as_structured_output() {
        let config = builtin_agent_runtime_config();
        assert!(config.is_structured_output_phase("code-review"));
        assert!(!config.is_structured_output_phase("implementation"));
    }
}
