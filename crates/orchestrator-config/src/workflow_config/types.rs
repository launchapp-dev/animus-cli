use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_runtime_config::AgentProfile;
use crate::PhaseExecutionDefinition;

pub const WORKFLOW_CONFIG_SCHEMA_ID: &str = "ao.workflow-config.v2";
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

pub(crate) fn default_max_rework_attempts() -> u32 {
    3
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub phases: Vec<WorkflowPhaseEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_success: Option<PostSuccessConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<WorkflowVariable>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum MergeStrategy {
    Squash,
    #[default]
    Merge,
    Rebase,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeConfig {
    #[serde(default)]
    pub strategy: MergeStrategy,
    #[serde(default = "default_target_branch")]
    pub target_branch: String,
    #[serde(default)]
    pub create_pr: bool,
    #[serde(default)]
    pub auto_merge: bool,
    #[serde(default)]
    pub cleanup_worktree: bool,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            strategy: MergeStrategy::default(),
            target_branch: default_target_branch(),
            create_pr: false,
            auto_merge: false,
            cleanup_worktree: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PostSuccessConfig {
    #[serde(default)]
    pub merge: Option<MergeConfig>,
}

impl WorkflowDefinition {
    pub fn phase_ids(&self) -> Vec<String> {
        self.phases
            .iter()
            .map(|entry| entry.phase_id().trim().to_owned())
            .filter(|id| !id.is_empty())
            .collect()
    }

    pub fn on_verdict_for_phase(
        &self,
        phase_id: &str,
    ) -> Option<&HashMap<String, PhaseTransitionConfig>> {
        self.phases
            .iter()
            .find(|entry| entry.phase_id().eq_ignore_ascii_case(phase_id))
            .and_then(|entry| entry.on_verdict())
    }
}

pub fn expand_workflow_phases(
    workflows: &[WorkflowDefinition],
    workflow_ref: &str,
) -> Result<Vec<WorkflowPhaseEntry>> {
    let mut visited = HashSet::new();
    expand_workflow_phases_inner(workflows, workflow_ref, &mut visited)
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
                let sub_phases =
                    expand_workflow_phases_inner(workflows, &sub.workflow_ref, visited)?;
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
        return Err(anyhow!(
            "missing required workflow variable(s): {}",
            missing.join(", ")
        ));
    }

    Ok(resolved)
}

pub fn expand_variables(text: &str, vars: &HashMap<String, String>) -> String {
    let mut result = text.to_string();
    for (key, value) in vars {
        let pattern = format!("{{{{{}}}}}", key);
        result = result.replace(&pattern, value);
    }
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
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub executable: String,
    #[serde(default)]
    pub supports_mcp: bool,
    #[serde(default)]
    pub supports_write: bool,
    #[serde(default)]
    pub context_window: Option<usize>,
    #[serde(default)]
    pub base_args: Vec<String>,
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
    pub auto_pr: bool,
    #[serde(default)]
    pub auto_merge: bool,
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

pub(crate) fn default_target_branch() -> String {
    "main".to_string()
}

pub(crate) fn default_visible() -> bool {
    true
}

pub(crate) fn default_keep_last_per_phase() -> usize {
    crate::workflow::DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE
}

pub(crate) fn phase_ui_definition(
    label: &str,
    description: &str,
    category: &str,
    tags: &[&str],
) -> PhaseUiDefinition {
    PhaseUiDefinition {
        label: label.to_string(),
        description: description.to_string(),
        category: category.to_string(),
        icon: None,
        docs_url: None,
        tags: tags.iter().map(|tag| tag.to_string()).collect(),
        visible: true,
    }
}

pub(crate) fn merge_strategy_is_valid(strategy: &MergeStrategy) -> bool {
    matches!(
        strategy,
        MergeStrategy::Squash | MergeStrategy::Merge | MergeStrategy::Rebase
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default)]
    pub interval_secs: Option<u64>,
    #[serde(default)]
    pub max_agents: Option<u32>,
    #[serde(default)]
    pub active_hours: Option<String>,
    #[serde(default)]
    pub auto_run_ready: bool,
    #[serde(default)]
    pub max_task_retries: Option<u32>,
    #[serde(default)]
    pub retry_cooldown_secs: Option<u64>,
    #[serde(default)]
    pub auto_merge: Option<bool>,
    #[serde(default)]
    pub auto_pr: Option<bool>,
    #[serde(default)]
    pub auto_commit_before_merge: Option<bool>,
    #[serde(default)]
    pub auto_prune_worktrees: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_routing: Option<protocol::PhaseRoutingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<protocol::McpRuntimeConfig>,
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
    pub agent_profiles: BTreeMap<String, AgentProfile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_servers: BTreeMap<String, McpServerDefinition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrations: Option<IntegrationsConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<WorkflowSchedule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<DaemonConfig>,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        super::builtins::builtin_workflow_config()
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
