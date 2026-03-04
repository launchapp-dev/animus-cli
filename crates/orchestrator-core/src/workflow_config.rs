use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::agent_runtime_config::{
    AgentProfile, AgentRuntimeConfig, CommandCwdMode, PhaseCommandDefinition, PhaseExecutionMode,
    PhaseManualDefinition,
};
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePhaseConfig {
    pub id: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub on_verdict: HashMap<String, PhaseTransitionConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_if: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubPipelineRef {
    pub pipeline: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PipelinePhaseEntry {
    SubPipeline(SubPipelineRef),
    Simple(String),
    Rich(PipelinePhaseConfig),
}

impl PipelinePhaseEntry {
    pub fn phase_id(&self) -> &str {
        match self {
            PipelinePhaseEntry::Simple(id) => id.as_str(),
            PipelinePhaseEntry::Rich(config) => config.id.as_str(),
            PipelinePhaseEntry::SubPipeline(sub) => sub.pipeline.as_str(),
        }
    }

    pub fn on_verdict(&self) -> Option<&HashMap<String, PhaseTransitionConfig>> {
        match self {
            PipelinePhaseEntry::Simple(_) | PipelinePhaseEntry::SubPipeline(_) => None,
            PipelinePhaseEntry::Rich(config) => {
                if config.on_verdict.is_empty() {
                    None
                } else {
                    Some(&config.on_verdict)
                }
            }
        }
    }

    pub fn skip_if(&self) -> &[String] {
        match self {
            PipelinePhaseEntry::Simple(_) | PipelinePhaseEntry::SubPipeline(_) => &[],
            PipelinePhaseEntry::Rich(config) => &config.skip_if,
        }
    }

    pub fn is_sub_pipeline(&self) -> bool {
        matches!(self, PipelinePhaseEntry::SubPipeline(_))
    }
}

impl From<String> for PipelinePhaseEntry {
    fn from(id: String) -> Self {
        PipelinePhaseEntry::Simple(id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineDefinition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub phases: Vec<PipelinePhaseEntry>,
}

impl PipelineDefinition {
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

pub fn expand_pipeline_phases(
    pipelines: &[PipelineDefinition],
    pipeline_id: &str,
) -> Result<Vec<PipelinePhaseEntry>> {
    let mut visited = HashSet::new();
    expand_pipeline_phases_inner(pipelines, pipeline_id, &mut visited)
}

fn expand_pipeline_phases_inner(
    pipelines: &[PipelineDefinition],
    pipeline_id: &str,
    visited: &mut HashSet<String>,
) -> Result<Vec<PipelinePhaseEntry>> {
    let normalized = pipeline_id.to_ascii_lowercase();
    if !visited.insert(normalized.clone()) {
        let chain: Vec<&str> = visited.iter().map(String::as_str).collect();
        return Err(anyhow!(
            "circular sub-pipeline reference detected: '{}' (visited: {})",
            pipeline_id,
            chain.join(" -> ")
        ));
    }

    let pipeline = pipelines
        .iter()
        .find(|p| p.id.eq_ignore_ascii_case(pipeline_id))
        .ok_or_else(|| anyhow!("sub-pipeline '{}' not found", pipeline_id))?;

    let mut expanded = Vec::new();
    for entry in &pipeline.phases {
        match entry {
            PipelinePhaseEntry::SubPipeline(sub) => {
                let sub_phases =
                    expand_pipeline_phases_inner(pipelines, &sub.pipeline, visited)?;
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
    pub pipeline: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub input: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    pub enabled: bool,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfig {
    pub schema: String,
    pub version: u32,
    pub default_pipeline_id: String,
    #[serde(default)]
    pub phase_catalog: BTreeMap<String, PhaseUiDefinition>,
    #[serde(default)]
    pub pipelines: Vec<PipelineDefinition>,
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
        builtin_workflow_config()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowConfigSource {
    Json,
    Builtin,
    BuiltinFallback,
}

impl WorkflowConfigSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Json => "json",
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
    pub path: PathBuf,
}

fn default_visible() -> bool {
    true
}

fn default_keep_last_per_phase() -> usize {
    crate::workflow::DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE
}

fn phase_ui_definition(
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

pub fn builtin_workflow_config() -> WorkflowConfig {
    static BUILTIN_CONFIG: OnceLock<WorkflowConfig> = OnceLock::new();
    BUILTIN_CONFIG
        .get_or_init(|| WorkflowConfig {
            schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
            version: WORKFLOW_CONFIG_VERSION,
            default_pipeline_id: "standard".to_string(),
            checkpoint_retention: WorkflowCheckpointRetentionConfig::default(),
            phase_catalog: BTreeMap::from([
                (
                    "requirements".to_string(),
                    phase_ui_definition(
                        "Requirements",
                        "Clarify scope, constraints, and acceptance criteria.",
                        "planning",
                        &["planning", "scope"],
                    ),
                ),
                (
                    "research".to_string(),
                    phase_ui_definition(
                        "Research",
                        "Gather implementation evidence and references for execution.",
                        "planning",
                        &["research"],
                    ),
                ),
                (
                    "ux-research".to_string(),
                    phase_ui_definition(
                        "UX Research",
                        "Document interaction patterns, user journeys, and accessibility constraints.",
                        "design",
                        &["design", "ux"],
                    ),
                ),
                (
                    "wireframe".to_string(),
                    phase_ui_definition(
                        "Wireframe",
                        "Produce concrete wireframes and interaction states.",
                        "design",
                        &["design", "wireframe"],
                    ),
                ),
                (
                    "mockup-review".to_string(),
                    phase_ui_definition(
                        "Mockup Review",
                        "Validate mockups against requirements and UX constraints.",
                        "review",
                        &["design", "review"],
                    ),
                ),
                (
                    "implementation".to_string(),
                    phase_ui_definition(
                        "Implementation",
                        "Deliver production-quality implementation changes.",
                        "build",
                        &["build", "code"],
                    ),
                ),
                (
                    "code-review".to_string(),
                    phase_ui_definition(
                        "Code Review",
                        "Review quality, risks, and maintainability before completion.",
                        "review",
                        &["review", "quality"],
                    ),
                ),
                (
                    "testing".to_string(),
                    phase_ui_definition(
                        "Testing",
                        "Run and update test coverage for the delivered changes.",
                        "qa",
                        &["qa", "testing"],
                    ),
                ),
            ]),
            pipelines: vec![
                PipelineDefinition {
                    id: "standard".to_string(),
                    name: "Standard".to_string(),
                    description:
                        "Default execution flow across requirements, implementation, review, and testing."
                            .to_string(),
                    phases: vec![
                        "requirements".to_string().into(),
                        "implementation".to_string().into(),
                        "code-review".to_string().into(),
                        "testing".to_string().into(),
                    ],
                },
                PipelineDefinition {
                    id: "ui-ux-standard".to_string(),
                    name: "UI UX Standard".to_string(),
                    description:
                        "Frontend-oriented flow with UX research, wireframes, and mockup review gates."
                            .to_string(),
                    phases: vec![
                        "requirements".to_string().into(),
                        "ux-research".to_string().into(),
                        "wireframe".to_string().into(),
                        "mockup-review".to_string().into(),
                        "implementation".to_string().into(),
                        "code-review".to_string().into(),
                        "testing".to_string().into(),
                    ],
                },
            ],
            phase_definitions: BTreeMap::new(),
            agent_profiles: BTreeMap::new(),
            tools_allowlist: Vec::new(),
            mcp_servers: BTreeMap::new(),
            tools: BTreeMap::new(),
            integrations: None,
            schedules: Vec::new(),
            daemon: None,
        })
        .clone()
}

pub fn workflow_config_path(project_root: &Path) -> PathBuf {
    let base = protocol::scoped_state_root(project_root)
        .unwrap_or_else(|| project_root.join(".ao"));
    base.join("state").join(WORKFLOW_CONFIG_FILE_NAME)
}

pub fn legacy_workflow_config_paths(project_root: &Path) -> [PathBuf; 2] {
    [
        project_root
            .join(".ao")
            .join("state")
            .join("workflow-config.json"),
        project_root.join(".ao").join("workflow-config.json"),
    ]
}

pub fn ensure_workflow_config_file(project_root: &Path) -> Result<()> {
    let path = workflow_config_path(project_root);
    if path.exists() {
        return Ok(());
    }

    write_workflow_config(project_root, &builtin_workflow_config())
}

pub fn load_workflow_config(project_root: &Path) -> Result<WorkflowConfig> {
    Ok(load_workflow_config_with_metadata(project_root)?.config)
}

pub fn load_workflow_config_with_metadata(project_root: &Path) -> Result<LoadedWorkflowConfig> {
    let path = workflow_config_path(project_root);
    if !path.exists() {
        if let Some(legacy_path) = legacy_workflow_config_paths(project_root)
            .iter()
            .find(|candidate| candidate.exists())
        {
            return Err(anyhow!(
                "workflow config v2 is required at {} (found legacy file at {}). Run `ao workflow config migrate-v2 --json`",
                path.display(),
                legacy_path.display()
            ));
        }

        return Err(anyhow!(
            "workflow config v2 file is missing at {}. Run `ao workflow config migrate-v2 --json` or initialize a new project",
            path.display()
        ));
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read workflow config at {}", path.display()))?;
    let config = serde_json::from_str::<WorkflowConfig>(&content)
        .with_context(|| format!("invalid workflow config JSON at {}", path.display()))?;
    validate_workflow_config(&config)?;

    Ok(LoadedWorkflowConfig {
        metadata: WorkflowConfigMetadata {
            schema: config.schema.clone(),
            version: config.version,
            hash: workflow_config_hash(&config),
            source: WorkflowConfigSource::Json,
        },
        config,
        path,
    })
}

pub fn load_workflow_config_or_default(project_root: &Path) -> LoadedWorkflowConfig {
    match load_workflow_config_with_metadata(project_root) {
        Ok(loaded) => loaded,
        Err(_) => {
            let config = builtin_workflow_config();
            LoadedWorkflowConfig {
                metadata: WorkflowConfigMetadata {
                    schema: config.schema.clone(),
                    version: config.version,
                    hash: workflow_config_hash(&config),
                    source: WorkflowConfigSource::BuiltinFallback,
                },
                config,
                path: workflow_config_path(project_root),
            }
        }
    }
}

pub fn write_workflow_config(project_root: &Path, config: &WorkflowConfig) -> Result<()> {
    validate_workflow_config(config)?;
    let path = workflow_config_path(project_root);
    crate::domain_state::write_json_pretty(&path, config)
}

pub fn workflow_config_hash(config: &WorkflowConfig) -> String {
    let bytes = serde_json::to_vec(config).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub fn resolve_pipeline_phase_plan(
    config: &WorkflowConfig,
    pipeline_id: Option<&str>,
) -> Option<Vec<String>> {
    let requested = pipeline_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(config.default_pipeline_id.trim());

    if requested.is_empty() {
        return None;
    }

    config
        .pipelines
        .iter()
        .find(|pipeline| pipeline.id.eq_ignore_ascii_case(requested))?;

    let expanded = expand_pipeline_phases(&config.pipelines, requested).ok()?;

    let phases: Vec<String> = expanded
        .iter()
        .map(|entry| entry.phase_id())
        .map(str::trim)
        .filter(|phase| !phase.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    if phases.is_empty() {
        None
    } else {
        Some(phases)
    }
}

pub fn resolve_pipeline_verdict_routing(
    config: &WorkflowConfig,
    pipeline_id: Option<&str>,
) -> HashMap<String, HashMap<String, PhaseTransitionConfig>> {
    let requested = pipeline_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(config.default_pipeline_id.trim());

    if requested.is_empty() {
        return HashMap::new();
    }

    let expanded = match expand_pipeline_phases(&config.pipelines, requested) {
        Ok(phases) => phases,
        Err(_) => return HashMap::new(),
    };

    let mut routing = HashMap::new();
    for entry in &expanded {
        if let Some(verdicts) = entry.on_verdict() {
            if !verdicts.is_empty() {
                routing.insert(entry.phase_id().to_owned(), verdicts.clone());
            }
        }
    }
    routing
}

pub fn resolve_pipeline_skip_guards(
    config: &WorkflowConfig,
    pipeline_id: Option<&str>,
) -> HashMap<String, Vec<String>> {
    let requested = pipeline_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(config.default_pipeline_id.trim());

    if requested.is_empty() {
        return HashMap::new();
    }

    let expanded = match expand_pipeline_phases(&config.pipelines, requested) {
        Ok(phases) => phases,
        Err(_) => return HashMap::new(),
    };

    let mut guards = HashMap::new();
    for entry in &expanded {
        let skip_if = entry.skip_if();
        if !skip_if.is_empty() {
            guards.insert(
                entry.phase_id().trim().to_owned(),
                skip_if.to_vec(),
            );
        }
    }
    guards
}

pub fn validate_workflow_and_runtime_configs(
    workflow: &WorkflowConfig,
    runtime: &AgentRuntimeConfig,
) -> Result<()> {
    validate_workflow_config(workflow)?;

    let mut errors = Vec::new();
    for pipeline in &workflow.pipelines {
        let expanded = match expand_pipeline_phases(&workflow.pipelines, &pipeline.id) {
            Ok(phases) => phases,
            Err(_) => continue,
        };

        for entry in &expanded {
            let phase_id = entry.phase_id().trim();
            if phase_id.is_empty() {
                continue;
            }

            if workflow
                .phase_catalog
                .keys()
                .all(|candidate| !candidate.eq_ignore_ascii_case(phase_id))
            {
                errors.push(format!(
                    "pipeline '{}' phase '{}' is missing from phase_catalog",
                    pipeline.id, phase_id
                ));
            }

            let in_workflow = workflow
                .phase_definitions
                .keys()
                .any(|k| k.eq_ignore_ascii_case(phase_id));
            if !in_workflow && !runtime.has_phase_definition(phase_id) {
                errors.push(format!(
                    "pipeline '{}' phase '{}' is missing from agent-runtime phases and workflow phase_definitions",
                    pipeline.id, phase_id
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

pub fn validate_workflow_config(config: &WorkflowConfig) -> Result<()> {
    let mut errors = Vec::new();

    if config.schema.trim() != WORKFLOW_CONFIG_SCHEMA_ID {
        errors.push(format!(
            "schema must be '{}' (got '{}')",
            WORKFLOW_CONFIG_SCHEMA_ID, config.schema
        ));
    }

    if config.version != WORKFLOW_CONFIG_VERSION {
        errors.push(format!(
            "version must be {} (got {})",
            WORKFLOW_CONFIG_VERSION, config.version
        ));
    }

    if config.default_pipeline_id.trim().is_empty() {
        errors.push("default_pipeline_id must not be empty".to_string());
    }

    if config.checkpoint_retention.keep_last_per_phase == 0 {
        errors
            .push("checkpoint_retention.keep_last_per_phase must be greater than zero".to_string());
    }

    if config.phase_catalog.is_empty() {
        errors.push("phase_catalog must include at least one phase".to_string());
    }

    for (phase_id, definition) in &config.phase_catalog {
        if phase_id.trim().is_empty() {
            errors.push("phase_catalog contains an empty phase id".to_string());
            continue;
        }

        if definition.label.trim().is_empty() {
            errors.push(format!(
                "phase_catalog['{}'].label must not be empty",
                phase_id
            ));
        }

        if definition.tags.iter().any(|tag| tag.trim().is_empty()) {
            errors.push(format!(
                "phase_catalog['{}'].tags must not contain empty values",
                phase_id
            ));
        }
    }

    if config.pipelines.is_empty() {
        errors.push("pipelines must include at least one pipeline".to_string());
    }

    let mut pipeline_ids = BTreeMap::<String, usize>::new();
    for pipeline in &config.pipelines {
        let pipeline_id = pipeline.id.trim();
        if pipeline_id.is_empty() {
            errors.push("pipelines contains a pipeline with an empty id".to_string());
            continue;
        }

        let normalized = pipeline_id.to_ascii_lowercase();
        if let Some(existing) = pipeline_ids.insert(normalized.clone(), 1) {
            let _ = existing;
            errors.push(format!("duplicate pipeline id '{}'", pipeline_id));
        }

        if pipeline.name.trim().is_empty() {
            errors.push(format!("pipeline '{}' name must not be empty", pipeline_id));
        }

        if pipeline.phases.is_empty() {
            errors.push(format!(
                "pipeline '{}' must include at least one phase",
                pipeline_id
            ));
            continue;
        }

        for entry in &pipeline.phases {
            if let PipelinePhaseEntry::SubPipeline(sub) = entry {
                let ref_id = sub.pipeline.trim();
                if ref_id.is_empty() {
                    errors.push(format!(
                        "pipeline '{}' contains a sub-pipeline reference with an empty id",
                        pipeline_id
                    ));
                    continue;
                }
                if !config
                    .pipelines
                    .iter()
                    .any(|p| p.id.eq_ignore_ascii_case(ref_id))
                {
                    errors.push(format!(
                        "pipeline '{}' references unknown sub-pipeline '{}'",
                        pipeline_id, ref_id
                    ));
                }
                continue;
            }

            let phase_id = entry.phase_id().trim();
            if phase_id.is_empty() {
                errors.push(format!(
                    "pipeline '{}' contains an empty phase id",
                    pipeline_id
                ));
                continue;
            }

            if config
                .phase_catalog
                .keys()
                .all(|candidate| !candidate.eq_ignore_ascii_case(phase_id))
            {
                errors.push(format!(
                    "pipeline '{}' references unknown phase '{}'; add it to phase_catalog",
                    pipeline_id, phase_id
                ));
            }
        }

        match expand_pipeline_phases(&config.pipelines, pipeline_id) {
            Ok(expanded) => {
                if expanded.is_empty() {
                    errors.push(format!(
                        "pipeline '{}' expands to zero phases",
                        pipeline_id
                    ));
                }

                let expanded_phase_ids: Vec<String> = expanded
                    .iter()
                    .map(|e| e.phase_id().trim().to_owned())
                    .filter(|id| !id.is_empty())
                    .collect();

                for entry in &expanded {
                    if let Some(verdicts) = entry.on_verdict() {
                        let phase_id = entry.phase_id().trim();
                        for (verdict_key, transition) in verdicts {
                            let target = transition.target.trim();
                            if target.is_empty() {
                                errors.push(format!(
                                    "pipeline '{}' phase '{}' on_verdict '{}' has an empty target",
                                    pipeline_id, phase_id, verdict_key
                                ));
                                continue;
                            }
                            if !expanded_phase_ids
                                .iter()
                                .any(|id| id.eq_ignore_ascii_case(target))
                            {
                                errors.push(format!(
                                    "pipeline '{}' phase '{}' on_verdict '{}' targets unknown phase '{}'",
                                    pipeline_id, phase_id, verdict_key, target
                                ));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                errors.push(format!(
                    "pipeline '{}' sub-pipeline expansion failed: {}",
                    pipeline_id, e
                ));
            }
        }
    }

    if config.pipelines.iter().all(|pipeline| {
        !pipeline
            .id
            .eq_ignore_ascii_case(config.default_pipeline_id.as_str())
    }) {
        errors.push(format!(
            "default_pipeline_id '{}' must reference an existing pipeline",
            config.default_pipeline_id
        ));
    }

    for (phase_id, definition) in &config.phase_definitions {
        if phase_id.trim().is_empty() {
            errors.push("phase_definitions contains an empty phase id".to_string());
            continue;
        }
        match definition.mode {
            PhaseExecutionMode::Command => {
                let Some(command) = definition.command.as_ref() else {
                    errors.push(format!(
                        "phase_definitions['{}'] mode 'command' requires command block",
                        phase_id
                    ));
                    continue;
                };
                if command.program.trim().is_empty() {
                    errors.push(format!(
                        "phase_definitions['{}'].command.program must not be empty",
                        phase_id
                    ));
                }
                if command.success_exit_codes.is_empty() {
                    errors.push(format!(
                        "phase_definitions['{}'].command.success_exit_codes must include at least one code",
                        phase_id
                    ));
                }
                if !config.tools_allowlist.is_empty()
                    && !config
                        .tools_allowlist
                        .iter()
                        .any(|t| t.eq_ignore_ascii_case(&command.program))
                {
                    errors.push(format!(
                        "phase_definitions['{}'].command.program '{}' is not in tools_allowlist",
                        phase_id, command.program
                    ));
                }
                if definition.manual.is_some() {
                    errors.push(format!(
                        "phase_definitions['{}'] mode 'command' must not include manual block",
                        phase_id
                    ));
                }
            }
            PhaseExecutionMode::Manual => {
                let Some(manual) = definition.manual.as_ref() else {
                    errors.push(format!(
                        "phase_definitions['{}'] mode 'manual' requires manual block",
                        phase_id
                    ));
                    continue;
                };
                if manual.instructions.trim().is_empty() {
                    errors.push(format!(
                        "phase_definitions['{}'].manual.instructions must not be empty",
                        phase_id
                    ));
                }
                if definition.command.is_some() {
                    errors.push(format!(
                        "phase_definitions['{}'] mode 'manual' must not include command block",
                        phase_id
                    ));
                }
            }
            PhaseExecutionMode::Agent => {
                if definition.agent_id.is_some() {
                    if let Some(agent_id) = definition.agent_id.as_deref() {
                        if !agent_id.trim().is_empty()
                            && !config.agent_profiles.contains_key(agent_id)
                        {
                            errors.push(format!(
                                "phase_definitions['{}'] references agent '{}' not found in agent_profiles (will check runtime config at execution time)",
                                phase_id, agent_id
                            ));
                        }
                    }
                }
            }
        }
    }

    for (name, definition) in &config.mcp_servers {
        if name.trim().is_empty() {
            errors.push("mcp_servers contains an empty server name".to_string());
            continue;
        }
        if definition.command.trim().is_empty() {
            errors.push(format!("mcp_servers['{}'].command must not be empty", name));
        }
        if definition
            .args
            .iter()
            .any(|arg| arg.trim().is_empty())
        {
            errors.push(format!(
                "mcp_servers['{}'].args must not contain empty values",
                name
            ));
        }
        if definition
            .tools
            .iter()
            .any(|tool| tool.trim().is_empty())
        {
            errors.push(format!(
                "mcp_servers['{}'].tools must not contain empty values",
                name
            ));
        }
        if definition.transport.as_deref().is_some_and(|transport| {
            transport.trim().is_empty()
        }) {
            errors.push(format!(
                "mcp_servers['{}'].transport must not be empty when set",
                name
            ));
        }
        if definition
            .env
            .iter()
            .any(|(key, value)| key.trim().is_empty() || value.trim().is_empty())
        {
            errors.push(format!(
                "mcp_servers['{}'].env must not contain empty keys or values",
                name
            ));
        }
    }

    for (agent_id, profile) in &config.agent_profiles {
        for server in &profile.mcp_servers {
            if server.trim().is_empty() {
                errors.push(format!(
                    "agent_profiles['{}'].mcp_servers must not contain empty values",
                    agent_id
                ));
                continue;
            }
            if !config.mcp_servers.contains_key(server) {
                errors.push(format!(
                    "agent_profiles['{}'].mcp_servers references unknown MCP server '{}'",
                    agent_id, server
                ));
            }
        }
    }

    for (name, definition) in &config.tools {
        if name.trim().is_empty() {
            errors.push("tools contains an empty tool name".to_string());
            continue;
        }
        if definition.executable.trim().is_empty() {
            errors.push(format!("tools['{}'].executable must not be empty", name));
        }
        if definition
            .base_args
            .iter()
            .any(|arg| arg.trim().is_empty())
        {
            errors.push(format!(
                "tools['{}'].base_args must not contain empty values",
                name
            ));
        }
        if definition
            .context_window
            .is_some_and(|value| value == 0)
        {
            errors.push(format!(
                "tools['{}'].context_window must be greater than 0 when set",
                name
            ));
        }
    }

    if let Some(integrations) = &config.integrations {
        if let Some(tasks) = &integrations.tasks {
            if tasks.provider.trim().is_empty() {
                errors.push("integrations.tasks.provider must not be empty".to_string());
            }
        }
        if let Some(git) = &integrations.git {
            if git.provider.trim().is_empty() {
                errors.push("integrations.git.provider must not be empty".to_string());
            }
            if let Some(base_branch) = git.base_branch.as_deref() {
                if base_branch.trim().is_empty() {
                    errors.push("integrations.git.base_branch must not be empty when set".to_string());
                }
            }
        }
    }

    let mut schedule_ids = BTreeMap::<String, usize>::new();
    for schedule in &config.schedules {
        if schedule.id.trim().is_empty() {
            errors.push("schedules contains an empty schedule id".to_string());
            continue;
        }

        let schedule_id = schedule.id.trim();
        let normalized = schedule_id.to_ascii_lowercase();
        if let Some(existing) = schedule_ids.insert(normalized.clone(), 1) {
            let _ = existing;
            errors.push(format!("duplicate schedule id '{}'", schedule_id));
        }

        if schedule.cron.trim().is_empty() {
            errors.push(format!(
                "schedules['{}'].cron must not be empty",
                schedule_id
            ));
        }
        if schedule.pipeline.is_none() && schedule.command.is_none() {
            errors.push(format!(
                "schedules['{}'] must define a pipeline or command",
                schedule_id
            ));
        }
        if let Some(pipeline) = schedule.pipeline.as_deref() {
            if pipeline.trim().is_empty() {
                errors.push(format!(
                    "schedules['{}'].pipeline must not be empty",
                    schedule_id
                ));
            } else if !config
                .pipelines
                .iter()
                .any(|p| p.id.eq_ignore_ascii_case(pipeline))
            {
                errors.push(format!(
                    "schedules['{}'].pipeline '{}' does not exist",
                    schedule_id, pipeline
                ));
            }
        }
        if let Some(command) = schedule.command.as_deref() {
            if command.trim().is_empty() {
                errors.push(format!(
                    "schedules['{}'].command must not be empty",
                    schedule_id
                ));
            }
        }
    }

    if let Some(daemon) = &config.daemon {
        if daemon.interval_secs == Some(0) {
            errors.push("daemon.interval_secs must be greater than zero when set".to_string());
        }
        if daemon.max_agents == Some(0) {
            errors.push("daemon.max_agents must be greater than zero when set".to_string());
        }
        if daemon
            .active_hours
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            errors.push("daemon.active_hours must not be empty when set".to_string());
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

pub const YAML_WORKFLOWS_DIR: &str = "workflows";

#[derive(Debug, Clone, Deserialize)]
struct YamlPhaseRichConfig {
    #[serde(default)]
    skip_if: Vec<String>,
    #[serde(default)]
    on_verdict: HashMap<String, PhaseTransitionConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlSubPipelineRef {
    pipeline: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum YamlPhaseEntry {
    SubPipeline(YamlSubPipelineRef),
    Simple(String),
    Rich(HashMap<String, YamlPhaseRichConfig>),
}

#[derive(Debug, Clone, Deserialize)]
struct YamlPipelineDefinition {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    phases: Vec<YamlPhaseEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct YamlCommandDefinition {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd_mode: Option<String>,
    #[serde(default)]
    cwd_path: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    success_exit_codes: Option<Vec<i32>>,
    #[serde(default)]
    parse_json_output: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct YamlManualDefinition {
    instructions: String,
    #[serde(default)]
    approval_note_required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct YamlPhaseDefinition {
    mode: String,
    #[serde(default)]
    command: Option<YamlCommandDefinition>,
    #[serde(default)]
    manual: Option<YamlManualDefinition>,
    #[serde(default)]
    directive: Option<String>,
}

fn parse_cwd_mode(value: &str) -> Result<CommandCwdMode> {
    match value.to_ascii_lowercase().replace('-', "_").as_str() {
        "project_root" => Ok(CommandCwdMode::ProjectRoot),
        "task_root" => Ok(CommandCwdMode::TaskRoot),
        "path" => Ok(CommandCwdMode::Path),
        other => Err(anyhow!("unknown cwd_mode '{}' (expected project_root, task_root, or path)", other)),
    }
}

fn yaml_phase_to_execution_definition(
    phase_id: &str,
    yaml: YamlPhaseDefinition,
) -> Result<PhaseExecutionDefinition> {
    let mode = match yaml.mode.to_ascii_lowercase().as_str() {
        "command" => PhaseExecutionMode::Command,
        "manual" => PhaseExecutionMode::Manual,
        "agent" => {
            return Err(anyhow!(
                "phases['{}'] mode 'agent' is not supported in YAML — agent phases belong in agent-runtime-config",
                phase_id
            ));
        }
        other => {
            return Err(anyhow!(
                "phases['{}'] has unknown mode '{}' (expected command or manual)",
                phase_id,
                other
            ));
        }
    };

    let command = match (&mode, yaml.command) {
        (PhaseExecutionMode::Command, Some(cmd)) => Some(PhaseCommandDefinition {
            program: cmd.program,
            args: cmd.args,
            env: cmd.env,
            cwd_mode: cmd
                .cwd_mode
                .as_deref()
                .map(parse_cwd_mode)
                .transpose()?
                .unwrap_or(CommandCwdMode::ProjectRoot),
            cwd_path: cmd.cwd_path,
            timeout_secs: cmd.timeout_secs,
            success_exit_codes: cmd.success_exit_codes.unwrap_or_else(|| vec![0]),
            parse_json_output: cmd.parse_json_output.unwrap_or(false),
            expected_result_kind: None,
            expected_schema: None,
        }),
        (PhaseExecutionMode::Command, None) => {
            return Err(anyhow!(
                "phases['{}'] mode 'command' requires a command block",
                phase_id
            ));
        }
        (_, Some(_)) => {
            return Err(anyhow!(
                "phases['{}'] mode '{}' must not include a command block",
                phase_id,
                yaml.mode
            ));
        }
        _ => None,
    };

    let manual = match (&mode, yaml.manual) {
        (PhaseExecutionMode::Manual, Some(m)) => Some(PhaseManualDefinition {
            instructions: m.instructions,
            approval_note_required: m.approval_note_required.unwrap_or(false),
        }),
        (PhaseExecutionMode::Manual, None) => {
            return Err(anyhow!(
                "phases['{}'] mode 'manual' requires a manual block",
                phase_id
            ));
        }
        (_, Some(_)) => {
            return Err(anyhow!(
                "phases['{}'] mode '{}' must not include a manual block",
                phase_id,
                yaml.mode
            ));
        }
        _ => None,
    };

    Ok(PhaseExecutionDefinition {
        mode,
        agent_id: None,
        directive: yaml.directive,
        runtime: None,
        capabilities: None,
        output_contract: None,
        output_json_schema: None,
        decision_contract: None,
        retry: None,
        command,
        manual,
    })
}

fn title_case_phase_id(phase_id: &str) -> String {
    phase_id
        .split(['-', '_'])
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut label = first.to_ascii_uppercase().to_string();
                    label.push_str(chars.as_str());
                    label
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Deserialize)]
struct YamlWorkflowFile {
    #[serde(default)]
    default_pipeline_id: Option<String>,
    #[serde(default)]
    phase_catalog: Option<BTreeMap<String, PhaseUiDefinition>>,
    #[serde(default)]
    pipelines: Vec<YamlPipelineDefinition>,
    #[serde(default)]
    phases: BTreeMap<String, YamlPhaseDefinition>,
    #[serde(default)]
    agents: BTreeMap<String, AgentProfile>,
    #[serde(default)]
    tools_allowlist: Vec<String>,
    #[serde(default)]
    mcp_servers: BTreeMap<String, McpServerDefinition>,
    #[serde(default)]
    tools: BTreeMap<String, ToolDefinition>,
    #[serde(default)]
    integrations: Option<IntegrationsConfig>,
    #[serde(default)]
    schedules: Vec<WorkflowSchedule>,
    #[serde(default)]
    daemon: Option<DaemonConfig>,
}

fn yaml_phase_entry_to_pipeline_phase_entry(entry: YamlPhaseEntry) -> Result<PipelinePhaseEntry> {
    match entry {
        YamlPhaseEntry::Simple(id) => Ok(PipelinePhaseEntry::Simple(id)),
        YamlPhaseEntry::SubPipeline(sub) => Ok(PipelinePhaseEntry::SubPipeline(SubPipelineRef {
            pipeline: sub.pipeline,
        })),
        YamlPhaseEntry::Rich(map) => {
            if map.len() != 1 {
                return Err(anyhow!(
                    "rich phase entry must have exactly one key (the phase id), got {}",
                    map.len()
                ));
            }
            let (id, config) = map.into_iter().next().unwrap();
            Ok(PipelinePhaseEntry::Rich(PipelinePhaseConfig {
                id,
                on_verdict: config.on_verdict,
                skip_if: config.skip_if,
            }))
        }
    }
}

fn yaml_pipeline_to_pipeline_definition(
    yaml: YamlPipelineDefinition,
) -> Result<PipelineDefinition> {
    let phases = yaml
        .phases
        .into_iter()
        .map(yaml_phase_entry_to_pipeline_phase_entry)
        .collect::<Result<Vec<_>>>()?;
    Ok(PipelineDefinition {
        id: yaml.id.clone(),
        name: yaml.name.unwrap_or_else(|| yaml.id.clone()),
        description: yaml.description.unwrap_or_default(),
        phases,
    })
}

pub fn parse_yaml_workflow_config(yaml_str: &str) -> Result<WorkflowConfig> {
    let yaml_file: YamlWorkflowFile =
        serde_yaml::from_str(yaml_str).context("failed to parse YAML workflow config")?;

    let pipelines = yaml_file
        .pipelines
        .into_iter()
        .map(yaml_pipeline_to_pipeline_definition)
        .collect::<Result<Vec<_>>>()?;

    let mut phase_definitions = BTreeMap::new();
    let mut auto_phase_catalog = BTreeMap::new();
    for (phase_id, yaml_phase) in yaml_file.phases {
        let definition = yaml_phase_to_execution_definition(&phase_id, yaml_phase)
            .with_context(|| format!("error converting YAML phase '{}'", phase_id))?;
        if !auto_phase_catalog.contains_key(&phase_id) {
            auto_phase_catalog.insert(
                phase_id.clone(),
                PhaseUiDefinition {
                    label: title_case_phase_id(&phase_id),
                    description: String::new(),
                    category: match definition.mode {
                        PhaseExecutionMode::Command => "build".to_string(),
                        PhaseExecutionMode::Manual => "manual".to_string(),
                        PhaseExecutionMode::Agent => "agent".to_string(),
                    },
                    icon: None,
                    docs_url: None,
                    tags: Vec::new(),
                    visible: true,
                },
            );
        }
        phase_definitions.insert(phase_id, definition);
    }

    let base = builtin_workflow_config();

    let default_pipeline_id = yaml_file
        .default_pipeline_id
        .unwrap_or(base.default_pipeline_id);
    let mut phase_catalog = yaml_file.phase_catalog.unwrap_or(base.phase_catalog);
    for (id, ui_def) in auto_phase_catalog {
        phase_catalog.entry(id).or_insert(ui_def);
    }

    Ok(WorkflowConfig {
        schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: WORKFLOW_CONFIG_VERSION,
        default_pipeline_id,
        phase_catalog,
        pipelines: if pipelines.is_empty() {
            base.pipelines
        } else {
            pipelines
        },
        checkpoint_retention: WorkflowCheckpointRetentionConfig::default(),
        phase_definitions,
        agent_profiles: yaml_file.agents,
        tools_allowlist: yaml_file.tools_allowlist,
        mcp_servers: yaml_file.mcp_servers,
        tools: yaml_file.tools,
        integrations: yaml_file.integrations,
        schedules: yaml_file.schedules,
        daemon: yaml_file.daemon,
    })
}

pub fn yaml_workflows_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ao").join(YAML_WORKFLOWS_DIR)
}

pub fn compile_yaml_workflow_files(project_root: &Path) -> Result<Option<WorkflowConfig>> {
    let workflows_dir = yaml_workflows_dir(project_root);
    let single_file = project_root.join(".ao").join("workflows.yaml");

    let mut yaml_sources: Vec<(PathBuf, String)> = Vec::new();

    if single_file.exists() {
        let content = fs::read_to_string(&single_file)
            .with_context(|| format!("failed to read {}", single_file.display()))?;
        yaml_sources.push((single_file, content));
    }

    if workflows_dir.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(&workflows_dir)
            .with_context(|| format!("failed to read directory {}", workflows_dir.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .map(|ext| ext == "yaml" || ext == "yml")
                    .unwrap_or(false)
            })
            .collect();
        entries.sort_by_key(|e| e.path());

        for entry in entries {
            let path = entry.path();
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            yaml_sources.push((path, content));
        }
    }

    if yaml_sources.is_empty() {
        return Ok(None);
    }

    let mut merged_config: Option<WorkflowConfig> = None;
    for (path, content) in &yaml_sources {
        let parsed = parse_yaml_workflow_config(content)
            .with_context(|| format!("error in YAML file {}", path.display()))?;
        merged_config = Some(match merged_config {
            None => parsed,
            Some(base) => merge_yaml_into_config(base, parsed),
        });
    }

    Ok(merged_config)
}

pub fn merge_yaml_into_config(base: WorkflowConfig, yaml: WorkflowConfig) -> WorkflowConfig {
    let mut pipelines = base.pipelines;

    for yaml_pipeline in yaml.pipelines {
        if let Some(pos) = pipelines
            .iter()
            .position(|p| p.id.eq_ignore_ascii_case(&yaml_pipeline.id))
        {
            pipelines[pos] = yaml_pipeline;
        } else {
            pipelines.push(yaml_pipeline);
        }
    }

    let mut phase_catalog = base.phase_catalog;
    for (key, value) in yaml.phase_catalog {
        phase_catalog.insert(key, value);
    }

    let mut phase_definitions = base.phase_definitions;
    for (key, value) in yaml.phase_definitions {
        phase_definitions.insert(key, value);
    }

    let mut agent_profiles = base.agent_profiles;
    for (key, value) in yaml.agent_profiles {
        agent_profiles.insert(key, value);
    }

    let mut tools_set: HashSet<String> = base.tools_allowlist.into_iter().collect();
    for tool in yaml.tools_allowlist {
        tools_set.insert(tool);
    }
    let mut tools_allowlist: Vec<String> = tools_set.into_iter().collect();
    tools_allowlist.sort();

    let mut mcp_servers = base.mcp_servers;
    for (name, definition) in yaml.mcp_servers {
        mcp_servers.insert(name, definition);
    }

    let mut tools = base.tools;
    for (name, definition) in yaml.tools {
        tools.insert(name, definition);
    }

    let mut schedules = base.schedules;
    for overlay_schedule in yaml.schedules {
        if let Some(pos) = schedules.iter().position(|schedule| {
            schedule.id.eq_ignore_ascii_case(overlay_schedule.id.as_str())
        }) {
            schedules[pos] = overlay_schedule;
        } else {
            schedules.push(overlay_schedule);
        }
    }

    let integrations = match (base.integrations, yaml.integrations) {
        (None, None) => None,
        (Some(mut base), Some(overlay)) => {
            if let Some(tasks) = overlay.tasks {
                base.tasks = Some(tasks);
            }
            if let Some(git) = overlay.git {
                base.git = Some(git);
            }
            Some(base)
        }
        (Some(base), None) => Some(base),
        (None, Some(overlay)) => Some(overlay),
    };

    let default_pipeline_id = if yaml.default_pipeline_id != base.default_pipeline_id
        && !yaml.default_pipeline_id.is_empty()
    {
        yaml.default_pipeline_id
    } else {
        base.default_pipeline_id
    };

    WorkflowConfig {
        schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: WORKFLOW_CONFIG_VERSION,
        default_pipeline_id,
        phase_catalog,
        pipelines,
        checkpoint_retention: base.checkpoint_retention,
        phase_definitions,
        agent_profiles,
        tools_allowlist,
        mcp_servers,
        tools,
        integrations,
        schedules,
        daemon: yaml.daemon.or(base.daemon),
    }
}

pub struct CompileYamlResult {
    pub config: WorkflowConfig,
    pub source_files: Vec<PathBuf>,
    pub output_path: PathBuf,
}

pub fn compile_and_write_yaml_workflows(project_root: &Path) -> Result<Option<CompileYamlResult>> {
    let workflows_dir = yaml_workflows_dir(project_root);
    let single_file = project_root.join(".ao").join("workflows.yaml");

    let mut source_files: Vec<PathBuf> = Vec::new();
    if single_file.exists() {
        source_files.push(single_file);
    }
    if workflows_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&workflows_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path
                    .extension()
                    .map(|ext| ext == "yaml" || ext == "yml")
                    .unwrap_or(false)
                {
                    source_files.push(path);
                }
            }
        }
    }
    source_files.sort();

    if source_files.is_empty() {
        return Ok(None);
    }

    let existing_config = load_workflow_config(project_root).ok();
    let yaml_config = compile_yaml_workflow_files(project_root)?
        .ok_or_else(|| anyhow!("no YAML workflow files found"))?;

    let final_config = match existing_config {
        Some(base) => merge_yaml_into_config(base, yaml_config),
        None => yaml_config,
    };

    validate_workflow_config(&final_config)?;
    write_workflow_config(project_root, &final_config)?;

    let output_path = workflow_config_path(project_root);
    Ok(Some(CompileYamlResult {
        config: final_config,
        source_files,
        output_path,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_workflow_config_is_valid() {
        let config = builtin_workflow_config();
        validate_workflow_config(&config).expect("builtin config should validate");
    }

    #[test]
    fn missing_v2_file_reports_actionable_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let err = load_workflow_config(temp.path()).expect_err("missing config should fail");
        let message = err.to_string();
        assert!(message.contains("workflow config v2 file is missing"));
        assert!(message.contains("migrate-v2"));
    }

    #[test]
    fn checkpoint_retention_requires_positive_keep_last_per_phase() {
        let mut config = builtin_workflow_config();
        config.checkpoint_retention.keep_last_per_phase = 0;
        let err = validate_workflow_config(&config).expect_err("invalid retention should fail");
        assert!(
            err.to_string()
                .contains("checkpoint_retention.keep_last_per_phase"),
            "validation error should mention checkpoint retention"
        );
    }

    #[test]
    fn validation_rejects_on_verdict_targeting_nonexistent_phase() {
        let mut config = builtin_workflow_config();
        let standard_pipeline = config
            .pipelines
            .iter_mut()
            .find(|p| p.id == "standard")
            .expect("standard pipeline");

        let mut on_verdict = HashMap::new();
        on_verdict.insert(
            "rework".to_string(),
            PhaseTransitionConfig {
                target: "nonexistent-phase".to_string(),
                guard: None,
            },
        );
        standard_pipeline.phases[0] = PipelinePhaseEntry::Rich(PipelinePhaseConfig {
            id: "requirements".to_string(),
            on_verdict,
            skip_if: Vec::new(),
        });

        let err = validate_workflow_config(&config)
            .expect_err("on_verdict with nonexistent target should fail validation");
        let message = err.to_string();
        assert!(
            message.contains("targets unknown phase 'nonexistent-phase'"),
            "error should mention the unknown target phase: {}",
            message
        );
    }

    #[test]
    fn serde_round_trips_simple_string_phases() {
        let config = builtin_workflow_config();
        let json = serde_json::to_string(&config).expect("serialize");
        let deserialized: WorkflowConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.pipelines.len(), config.pipelines.len());
        for (orig, deser) in config.pipelines.iter().zip(deserialized.pipelines.iter()) {
            let orig_ids: Vec<&str> = orig.phases.iter().map(|e| e.phase_id()).collect();
            let deser_ids: Vec<&str> = deser.phases.iter().map(|e| e.phase_id()).collect();
            assert_eq!(orig_ids, deser_ids);
        }
    }

    #[test]
    fn serde_deserializes_rich_phase_config() {
        let json = r#"{
            "id": "code-review",
            "on_verdict": {
                "rework": { "target": "implementation" }
            }
        }"#;
        let entry: PipelinePhaseEntry = serde_json::from_str(json).expect("deserialize rich entry");
        assert_eq!(entry.phase_id(), "code-review");
        let verdicts = entry.on_verdict().expect("should have on_verdict");
        assert!(verdicts.contains_key("rework"));
        assert_eq!(verdicts["rework"].target, "implementation");
    }

    #[test]
    fn serde_deserializes_simple_string_phase() {
        let json = r#""requirements""#;
        let entry: PipelinePhaseEntry =
            serde_json::from_str(json).expect("deserialize simple string");
        assert_eq!(entry.phase_id(), "requirements");
        assert!(entry.on_verdict().is_none());
    }

    #[test]
    fn serde_deserializes_mixed_pipeline_phases() {
        let json = r#"{
            "id": "test-pipeline",
            "name": "Test",
            "description": "",
            "phases": [
                "requirements",
                { "id": "implementation", "on_verdict": { "rework": { "target": "requirements" } } },
                "testing"
            ]
        }"#;
        let pipeline: PipelineDefinition = serde_json::from_str(json).expect("deserialize");
        assert_eq!(pipeline.phases.len(), 3);
        assert_eq!(pipeline.phases[0].phase_id(), "requirements");
        assert!(pipeline.phases[0].on_verdict().is_none());
        assert_eq!(pipeline.phases[1].phase_id(), "implementation");
        let verdicts = pipeline.phases[1].on_verdict().expect("should have verdicts");
        assert_eq!(verdicts["rework"].target, "requirements");
        assert_eq!(pipeline.phases[2].phase_id(), "testing");
        assert!(pipeline.phases[2].on_verdict().is_none());
    }

    #[test]
    fn pipeline_phase_entry_deserializes_from_string() {
        let json = r#""requirements""#;
        let entry: PipelinePhaseEntry = serde_json::from_str(json).expect("parse string entry");
        assert_eq!(entry.phase_id(), "requirements");
        assert!(entry.skip_if().is_empty());
    }

    #[test]
    fn pipeline_phase_entry_deserializes_from_object_with_skip_if() {
        let json = r#"{"id": "testing", "skip_if": ["task_type == 'docs'"]}"#;
        let entry: PipelinePhaseEntry = serde_json::from_str(json).expect("parse config entry");
        assert_eq!(entry.phase_id(), "testing");
        assert_eq!(entry.skip_if(), &["task_type == 'docs'"]);
    }

    #[test]
    fn pipeline_phase_entry_deserializes_from_object_without_skip_if() {
        let json = r#"{"id": "implementation"}"#;
        let entry: PipelinePhaseEntry = serde_json::from_str(json).expect("parse config entry");
        assert_eq!(entry.phase_id(), "implementation");
        assert!(entry.skip_if().is_empty());
    }

    #[test]
    fn pipeline_definition_deserializes_mixed_phase_entries() {
        let json = r#"{
            "id": "test-pipeline",
            "name": "Test",
            "phases": [
                "requirements",
                {"id": "testing", "skip_if": ["task_type == 'docs'"]},
                "implementation"
            ]
        }"#;
        let pipeline: PipelineDefinition =
            serde_json::from_str(json).expect("parse mixed pipeline");
        assert_eq!(pipeline.phases.len(), 3);
        assert_eq!(pipeline.phases[0].phase_id(), "requirements");
        assert!(pipeline.phases[0].skip_if().is_empty());
        assert_eq!(pipeline.phases[1].phase_id(), "testing");
        assert_eq!(pipeline.phases[1].skip_if(), &["task_type == 'docs'"]);
        assert_eq!(pipeline.phases[2].phase_id(), "implementation");
    }

    #[test]
    fn resolve_pipeline_skip_guards_extracts_guards_from_config() {
        let mut config = builtin_workflow_config();
        let standard_pipeline = config
            .pipelines
            .iter_mut()
            .find(|p| p.id == "standard")
            .expect("standard pipeline");
        standard_pipeline.phases = vec![
            "requirements".to_string().into(),
            PipelinePhaseEntry::Rich(PipelinePhaseConfig {
                id: "testing".to_string(),
                on_verdict: HashMap::new(),
                skip_if: vec!["task_type == 'docs'".to_string()],
            }),
            "implementation".to_string().into(),
        ];

        let guards = resolve_pipeline_skip_guards(&config, Some("standard"));
        assert_eq!(guards.len(), 1);
        assert_eq!(
            guards.get("testing").unwrap(),
            &vec!["task_type == 'docs'".to_string()]
        );
    }

    #[test]
    fn yaml_parses_simple_pipeline() {
        let yaml = r#"
pipelines:
  - id: standard
    name: Standard Pipeline
    description: Default development workflow
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse simple YAML");
        let standard = config
            .pipelines
            .iter()
            .find(|p| p.id == "standard")
            .expect("should have standard pipeline");
        assert_eq!(standard.name, "Standard Pipeline");
        assert_eq!(standard.phases.len(), 4);
        assert_eq!(standard.phases[0].phase_id(), "requirements");
        assert_eq!(standard.phases[1].phase_id(), "implementation");
        assert_eq!(standard.phases[2].phase_id(), "code-review");
        assert_eq!(standard.phases[3].phase_id(), "testing");
    }

    #[test]
    fn yaml_parses_rich_phase_with_skip_if() {
        let yaml = r#"
pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing:
          skip_if:
            - "task_type == 'docs'"
      - code-review
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with skip_if");
        let standard = config
            .pipelines
            .iter()
            .find(|p| p.id == "standard")
            .expect("should have standard pipeline");
        assert_eq!(standard.phases.len(), 4);
        assert_eq!(standard.phases[2].phase_id(), "testing");
        assert_eq!(standard.phases[2].skip_if(), &["task_type == 'docs'"]);
    }

    #[test]
    fn yaml_parses_rich_phase_with_on_verdict() {
        let yaml = r#"
pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - code-review:
          on_verdict:
            rework:
              target: implementation
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with on_verdict");
        let standard = config
            .pipelines
            .iter()
            .find(|p| p.id == "standard")
            .expect("should have standard pipeline");
        assert_eq!(standard.phases[2].phase_id(), "code-review");
        let verdicts = standard.phases[2]
            .on_verdict()
            .expect("should have on_verdict");
        assert_eq!(verdicts["rework"].target, "implementation");
    }

    #[test]
    fn yaml_parses_mixed_simple_and_rich_phases() {
        let yaml = r#"
pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing:
          skip_if:
            - "task_type == 'docs'"
      - code-review:
          on_verdict:
            rework:
              target: implementation
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse mixed phases");
        let standard = config
            .pipelines
            .iter()
            .find(|p| p.id == "standard")
            .expect("should have standard pipeline");
        assert_eq!(standard.phases.len(), 4);
        assert_eq!(standard.phases[0].phase_id(), "requirements");
        assert!(standard.phases[0].on_verdict().is_none());
        assert!(standard.phases[0].skip_if().is_empty());
        assert_eq!(standard.phases[2].phase_id(), "testing");
        assert_eq!(standard.phases[2].skip_if(), &["task_type == 'docs'"]);
        assert_eq!(standard.phases[3].phase_id(), "code-review");
        let verdicts = standard.phases[3]
            .on_verdict()
            .expect("should have on_verdict");
        assert_eq!(verdicts["rework"].target, "implementation");
    }

    #[test]
    fn yaml_merge_replaces_pipeline_by_id() {
        let base = builtin_workflow_config();
        let yaml = r#"
pipelines:
  - id: standard
    name: Overridden Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
        let yaml_config = parse_yaml_workflow_config(yaml).expect("parse yaml");
        let merged = merge_yaml_into_config(base.clone(), yaml_config);
        let standard = merged
            .pipelines
            .iter()
            .find(|p| p.id == "standard")
            .expect("standard pipeline");
        assert_eq!(standard.name, "Overridden Standard");
        assert_eq!(standard.phases.len(), 3);
        assert!(
            merged.pipelines.iter().any(|p| p.id == "ui-ux-standard"),
            "non-overridden pipeline should be preserved"
        );
    }

    #[test]
    fn yaml_merge_adds_new_pipeline() {
        let base = builtin_workflow_config();
        let base_count = base.pipelines.len();
        let yaml = r#"
pipelines:
  - id: quick-fix
    name: Quick Fix
    phases:
      - implementation
      - testing
"#;
        let yaml_config = parse_yaml_workflow_config(yaml).expect("parse yaml");
        let merged = merge_yaml_into_config(base, yaml_config);
        assert_eq!(merged.pipelines.len(), base_count + 1);
        assert!(merged.pipelines.iter().any(|p| p.id == "quick-fix"));
    }

    #[test]
    fn yaml_missing_files_returns_none() {
        let temp = tempfile::tempdir().expect("tempdir");
        let result = compile_yaml_workflow_files(temp.path()).expect("should not error");
        assert!(result.is_none());
    }

    #[test]
    fn yaml_invalid_syntax_returns_error() {
        let yaml = "pipelines:\n  - id: [invalid";
        let result = parse_yaml_workflow_config(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to parse YAML"),
            "error should mention YAML parsing: {}",
            err
        );
    }

    #[test]
    fn yaml_pipeline_name_defaults_to_id() {
        let yaml = r#"
pipelines:
  - id: quick-fix
    phases:
      - implementation
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("parse");
        let pipeline = config
            .pipelines
            .iter()
            .find(|p| p.id == "quick-fix")
            .expect("pipeline");
        assert_eq!(pipeline.name, "quick-fix");
    }

    #[test]
    fn yaml_compile_reads_from_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join(".ao").join("state");
        fs::create_dir_all(&state_dir).expect("create state dir");
        let builtin = builtin_workflow_config();
        crate::domain_state::write_json_pretty(
            &state_dir.join(WORKFLOW_CONFIG_FILE_NAME),
            &builtin,
        )
        .expect("write base config");

        let workflows_dir = temp.path().join(".ao").join("workflows");
        fs::create_dir_all(&workflows_dir).expect("create workflows dir");
        fs::write(
            workflows_dir.join("pipelines.yaml"),
            r#"
pipelines:
  - id: standard
    name: YAML Standard
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#,
        )
        .expect("write yaml");

        let result =
            compile_yaml_workflow_files(temp.path()).expect("compile should succeed");
        let config = result.expect("should have config");
        let standard = config
            .pipelines
            .iter()
            .find(|p| p.id == "standard")
            .expect("standard pipeline");
        assert_eq!(standard.name, "YAML Standard");
    }

    #[test]
    fn yaml_compile_reads_single_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ao_dir = temp.path().join(".ao");
        fs::create_dir_all(&ao_dir).expect("create .ao dir");
        fs::write(
            ao_dir.join("workflows.yaml"),
            r#"
pipelines:
  - id: standard
    name: Single File Standard
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#,
        )
        .expect("write yaml");

        let result =
            compile_yaml_workflow_files(temp.path()).expect("compile should succeed");
        let config = result.expect("should have config");
        let standard = config
            .pipelines
            .iter()
            .find(|p| p.id == "standard")
            .expect("standard pipeline");
        assert_eq!(standard.name, "Single File Standard");
    }

    #[test]
    fn yaml_compile_and_write_validates_and_writes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join(".ao").join("state");
        fs::create_dir_all(&state_dir).expect("create state dir");
        let builtin = builtin_workflow_config();
        crate::domain_state::write_json_pretty(
            &state_dir.join(WORKFLOW_CONFIG_FILE_NAME),
            &builtin,
        )
        .expect("write base config");

        let workflows_dir = temp.path().join(".ao").join("workflows");
        fs::create_dir_all(&workflows_dir).expect("create workflows dir");
        fs::write(
            workflows_dir.join("pipelines.yaml"),
            r#"
pipelines:
  - id: standard
    name: Compiled Standard
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#,
        )
        .expect("write yaml");

        let result =
            compile_and_write_yaml_workflows(temp.path()).expect("compile and write should succeed");
        let compile_result = result.expect("should have result");
        assert_eq!(compile_result.source_files.len(), 1);

        let reloaded = load_workflow_config(temp.path()).expect("reload config");
        let standard = reloaded
            .pipelines
            .iter()
            .find(|p| p.id == "standard")
            .expect("standard pipeline");
        assert_eq!(standard.name, "Compiled Standard");
    }

    fn make_pipeline(id: &str, phases: Vec<PipelinePhaseEntry>) -> PipelineDefinition {
        PipelineDefinition {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            phases,
        }
    }

    #[test]
    fn expand_basic_sub_pipeline() {
        let pipelines = vec![
            make_pipeline(
                "review-cycle",
                vec![
                    PipelinePhaseEntry::Simple("code-review".into()),
                    PipelinePhaseEntry::Simple("testing".into()),
                ],
            ),
            make_pipeline(
                "standard",
                vec![
                    PipelinePhaseEntry::Simple("requirements".into()),
                    PipelinePhaseEntry::Simple("implementation".into()),
                    PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                        pipeline: "review-cycle".into(),
                    }),
                    PipelinePhaseEntry::Simple("merge".into()),
                ],
            ),
        ];

        let expanded = expand_pipeline_phases(&pipelines, "standard").expect("should expand");
        let ids: Vec<&str> = expanded.iter().map(|e| e.phase_id()).collect();
        assert_eq!(
            ids,
            vec!["requirements", "implementation", "code-review", "testing", "merge"]
        );
    }

    #[test]
    fn expand_nested_sub_pipelines() {
        let pipelines = vec![
            make_pipeline(
                "lint",
                vec![PipelinePhaseEntry::Simple("code-review".into())],
            ),
            make_pipeline(
                "review-cycle",
                vec![
                    PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                        pipeline: "lint".into(),
                    }),
                    PipelinePhaseEntry::Simple("testing".into()),
                ],
            ),
            make_pipeline(
                "standard",
                vec![
                    PipelinePhaseEntry::Simple("requirements".into()),
                    PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                        pipeline: "review-cycle".into(),
                    }),
                ],
            ),
        ];

        let expanded = expand_pipeline_phases(&pipelines, "standard").expect("should expand");
        let ids: Vec<&str> = expanded.iter().map(|e| e.phase_id()).collect();
        assert_eq!(ids, vec!["requirements", "code-review", "testing"]);
    }

    #[test]
    fn expand_detects_circular_reference() {
        let pipelines = vec![
            make_pipeline(
                "a",
                vec![PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                    pipeline: "b".into(),
                })],
            ),
            make_pipeline(
                "b",
                vec![PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                    pipeline: "a".into(),
                })],
            ),
        ];

        let err = expand_pipeline_phases(&pipelines, "a").expect_err("should detect cycle");
        assert!(
            err.to_string().contains("circular sub-pipeline reference"),
            "error should mention circular reference: {}",
            err
        );
    }

    #[test]
    fn expand_detects_self_reference() {
        let pipelines = vec![make_pipeline(
            "self-ref",
            vec![PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                pipeline: "self-ref".into(),
            })],
        )];

        let err =
            expand_pipeline_phases(&pipelines, "self-ref").expect_err("should detect self-ref");
        assert!(
            err.to_string().contains("circular sub-pipeline reference"),
            "error should mention circular reference: {}",
            err
        );
    }

    #[test]
    fn expand_errors_on_missing_pipeline_reference() {
        let pipelines = vec![make_pipeline(
            "standard",
            vec![PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                pipeline: "nonexistent".into(),
            })],
        )];

        let err = expand_pipeline_phases(&pipelines, "standard")
            .expect_err("should error on missing ref");
        assert!(
            err.to_string().contains("sub-pipeline 'nonexistent' not found"),
            "error should mention missing pipeline: {}",
            err
        );
    }

    #[test]
    fn expand_preserves_rich_phase_config() {
        let mut on_verdict = HashMap::new();
        on_verdict.insert(
            "rework".to_string(),
            PhaseTransitionConfig {
                target: "implementation".to_string(),
                guard: None,
            },
        );

        let pipelines = vec![
            make_pipeline(
                "review",
                vec![PipelinePhaseEntry::Rich(PipelinePhaseConfig {
                    id: "code-review".into(),
                    on_verdict: on_verdict.clone(),
                    skip_if: vec!["task_type == 'docs'".into()],
                })],
            ),
            make_pipeline(
                "standard",
                vec![
                    PipelinePhaseEntry::Simple("implementation".into()),
                    PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                        pipeline: "review".into(),
                    }),
                ],
            ),
        ];

        let expanded = expand_pipeline_phases(&pipelines, "standard").expect("should expand");
        assert_eq!(expanded.len(), 2);
        assert_eq!(expanded[1].phase_id(), "code-review");
        let verdicts = expanded[1].on_verdict().expect("should have on_verdict");
        assert_eq!(verdicts["rework"].target, "implementation");
        assert_eq!(expanded[1].skip_if(), &["task_type == 'docs'"]);
    }

    #[test]
    fn serde_deserializes_sub_pipeline_ref() {
        let json = r#"{"pipeline": "review-cycle"}"#;
        let entry: PipelinePhaseEntry =
            serde_json::from_str(json).expect("deserialize sub-pipeline");
        assert!(entry.is_sub_pipeline());
        assert_eq!(entry.phase_id(), "review-cycle");
    }

    #[test]
    fn serde_round_trips_sub_pipeline_entry() {
        let entry = PipelinePhaseEntry::SubPipeline(SubPipelineRef {
            pipeline: "review-cycle".into(),
        });
        let json = serde_json::to_string(&entry).expect("serialize");
        let deserialized: PipelinePhaseEntry = serde_json::from_str(&json).expect("deserialize");
        assert!(deserialized.is_sub_pipeline());
        assert_eq!(deserialized.phase_id(), "review-cycle");
    }

    #[test]
    fn serde_deserializes_pipeline_with_mixed_entries() {
        let json = r#"{
            "id": "full",
            "name": "Full Pipeline",
            "description": "",
            "phases": [
                "requirements",
                {"pipeline": "review-cycle"},
                {"id": "testing", "skip_if": ["task_type == 'docs'"]},
                "merge"
            ]
        }"#;
        let pipeline: PipelineDefinition = serde_json::from_str(json).expect("deserialize");
        assert_eq!(pipeline.phases.len(), 4);
        assert!(!pipeline.phases[0].is_sub_pipeline());
        assert!(pipeline.phases[1].is_sub_pipeline());
        assert_eq!(pipeline.phases[1].phase_id(), "review-cycle");
        assert!(!pipeline.phases[2].is_sub_pipeline());
        assert_eq!(pipeline.phases[2].phase_id(), "testing");
        assert!(!pipeline.phases[3].is_sub_pipeline());
    }

    #[test]
    fn yaml_parses_sub_pipeline_ref() {
        let yaml = r#"
pipelines:
  - id: review-cycle
    name: Review Cycle
    phases:
      - code-review
      - testing
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - pipeline: review-cycle
      - merge
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with sub-pipeline");
        let standard = config
            .pipelines
            .iter()
            .find(|p| p.id == "standard")
            .expect("should have standard pipeline");
        assert_eq!(standard.phases.len(), 4);
        assert!(standard.phases[2].is_sub_pipeline());
        assert_eq!(standard.phases[2].phase_id(), "review-cycle");
    }

    #[test]
    fn resolve_phase_plan_expands_sub_pipelines() {
        let mut config = builtin_workflow_config();
        config.pipelines.push(PipelineDefinition {
            id: "review-cycle".into(),
            name: "Review Cycle".into(),
            description: String::new(),
            phases: vec![
                PipelinePhaseEntry::Simple("code-review".into()),
                PipelinePhaseEntry::Simple("testing".into()),
            ],
        });

        let standard = config
            .pipelines
            .iter_mut()
            .find(|p| p.id == "standard")
            .expect("standard pipeline");
        standard.phases = vec![
            PipelinePhaseEntry::Simple("requirements".into()),
            PipelinePhaseEntry::Simple("implementation".into()),
            PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                pipeline: "review-cycle".into(),
            }),
        ];

        let phases =
            resolve_pipeline_phase_plan(&config, Some("standard")).expect("should resolve");
        assert_eq!(
            phases,
            vec!["requirements", "implementation", "code-review", "testing"]
        );
    }

    #[test]
    fn validate_rejects_missing_sub_pipeline_reference() {
        let mut config = builtin_workflow_config();
        let standard = config
            .pipelines
            .iter_mut()
            .find(|p| p.id == "standard")
            .expect("standard pipeline");
        standard.phases = vec![
            PipelinePhaseEntry::Simple("requirements".into()),
            PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                pipeline: "nonexistent".into(),
            }),
        ];

        let err = validate_workflow_config(&config)
            .expect_err("should reject missing sub-pipeline ref");
        let message = err.to_string();
        assert!(
            message.contains("references unknown sub-pipeline 'nonexistent'"),
            "error should mention missing sub-pipeline: {}",
            message
        );
    }

    #[test]
    fn validate_rejects_circular_sub_pipeline() {
        let mut config = builtin_workflow_config();
        config.pipelines = vec![
            PipelineDefinition {
                id: "standard".into(),
                name: "Standard".into(),
                description: String::new(),
                phases: vec![PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                    pipeline: "review".into(),
                })],
            },
            PipelineDefinition {
                id: "review".into(),
                name: "Review".into(),
                description: String::new(),
                phases: vec![PipelinePhaseEntry::SubPipeline(SubPipelineRef {
                    pipeline: "standard".into(),
                })],
            },
        ];

        let err =
            validate_workflow_config(&config).expect_err("should reject circular sub-pipeline");
        let message = err.to_string();
        assert!(
            message.contains("sub-pipeline expansion failed"),
            "error should mention expansion failure: {}",
            message
        );
    }

    #[test]
    fn expand_pipeline_not_found_at_top_level() {
        let pipelines = vec![make_pipeline(
            "standard",
            vec![PipelinePhaseEntry::Simple("requirements".into())],
        )];

        let err = expand_pipeline_phases(&pipelines, "nonexistent")
            .expect_err("should error on missing pipeline");
        assert!(
            err.to_string().contains("sub-pipeline 'nonexistent' not found"),
            "error should mention missing pipeline: {}",
            err
        );
    }

    #[test]
    fn yaml_parses_command_phase() {
        let yaml = r#"
phases:
  build:
    mode: command
    command:
      program: cargo
      args: ["build", "--release"]
      timeout_secs: 300

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - build
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with command phase");
        assert!(config.phase_definitions.contains_key("build"));
        let build = &config.phase_definitions["build"];
        assert_eq!(build.mode, PhaseExecutionMode::Command);
        let cmd = build.command.as_ref().expect("should have command");
        assert_eq!(cmd.program, "cargo");
        assert_eq!(cmd.args, vec!["build", "--release"]);
        assert_eq!(cmd.timeout_secs, Some(300));
        assert_eq!(cmd.cwd_mode, CommandCwdMode::ProjectRoot);
        assert_eq!(cmd.success_exit_codes, vec![0]);
    }

    #[test]
    fn yaml_parses_manual_phase() {
        let yaml = r#"
phases:
  approval:
    mode: manual
    manual:
      instructions: "Review and approve the deployment plan"
      approval_note_required: true

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - approval
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with manual phase");
        assert!(config.phase_definitions.contains_key("approval"));
        let approval = &config.phase_definitions["approval"];
        assert_eq!(approval.mode, PhaseExecutionMode::Manual);
        let manual = approval.manual.as_ref().expect("should have manual");
        assert_eq!(manual.instructions, "Review and approve the deployment plan");
        assert!(manual.approval_note_required);
    }

    #[test]
    fn yaml_parses_agent_profile() {
        let yaml = r#"
agents:
  researcher:
    system_prompt: "You are a research agent focused on code analysis"
    model: gemini-3.1-pro-preview
    web_search: true
    skills:
      - deep-search
    capabilities:
      code_execution: false

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with agent profile");
        assert!(config.agent_profiles.contains_key("researcher"));
        let researcher = &config.agent_profiles["researcher"];
        assert_eq!(researcher.system_prompt, "You are a research agent focused on code analysis");
        assert_eq!(researcher.model.as_deref(), Some("gemini-3.1-pro-preview"));
        assert_eq!(researcher.web_search, Some(true));
        assert_eq!(researcher.skills, vec!["deep-search"]);
        assert_eq!(researcher.capabilities.get("code_execution"), Some(&false));
    }

    #[test]
    fn yaml_auto_registers_command_phase_in_catalog() {
        let yaml = r#"
phases:
  cargo-build:
    mode: command
    command:
      program: cargo
      args: ["build"]

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - cargo-build
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse");
        assert!(config.phase_catalog.contains_key("cargo-build"));
        let catalog_entry = &config.phase_catalog["cargo-build"];
        assert_eq!(catalog_entry.label, "Cargo Build");
        assert_eq!(catalog_entry.category, "build");
    }

    #[test]
    fn yaml_collects_tools_allowlist() {
        let yaml = r#"
tools_allowlist:
  - cargo
  - npm

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse");
        assert!(config.tools_allowlist.contains(&"cargo".to_string()));
        assert!(config.tools_allowlist.contains(&"npm".to_string()));
    }

    #[test]
    fn yaml_parses_unified_config_sections() {
        let yaml = r#"
mcp_servers:
  mcp-go:
    command: "node"
    args: ["server.js"]
    transport: "stdio"
    config:
      endpoint: "stdio://local"
    tools:
      - search
      - shell
    env:
      MCP_TOKEN: "token"
tools:
  cli-gpt:
    executable: "gpt-cli"
    supports_mcp: true
    supports_write: false
    context_window: 64000
    base_args: ["--json"]
integrations:
  tasks:
    provider: github
    config:
      scope: "org"
  git:
    provider: github
    auto_pr: true
    auto_merge: false
    base_branch: "main"
    config:
      organization: "acme"
schedules:
  - id: nightly
    cron: "0 2 * * *"
    command: "echo nightly"
    enabled: true
daemon:
  interval_secs: 300
  max_agents: 2
  active_hours: "00:00-06:00"
  auto_run_ready: true
pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse unified config sections");
        let server = config
            .mcp_servers
            .get("mcp-go")
            .expect("mcp server should be parsed");
        assert_eq!(server.command, "node");
        assert_eq!(server.args, vec!["server.js"]);
        assert_eq!(server.transport.as_deref(), Some("stdio"));
        assert_eq!(server.tools, vec!["search", "shell"]);
        let tool = config
            .tools
            .get("cli-gpt")
            .expect("tool definition should be parsed");
        assert_eq!(tool.executable, "gpt-cli");
        assert!(tool.supports_mcp);
        assert_eq!(tool.context_window, Some(64000));
        assert_eq!(tool.base_args, vec!["--json"]);
        let integrations = config
            .integrations
            .as_ref()
            .expect("integrations should be parsed");
        let task_integration = integrations
            .tasks
            .as_ref()
            .expect("task integration should be parsed");
        assert_eq!(task_integration.provider, "github");
        let git_integration = integrations
            .git
            .as_ref()
            .expect("git integration should be parsed");
        assert_eq!(git_integration.provider, "github");
        assert!(git_integration.auto_pr);
        assert!(!git_integration.auto_merge);
        assert_eq!(git_integration.base_branch.as_deref(), Some("main"));
        assert_eq!(config.schedules.len(), 1);
        assert_eq!(config.schedules[0].id, "nightly");
        assert_eq!(config.schedules[0].cron, "0 2 * * *");
        assert_eq!(
            config.schedules[0].command.as_deref(),
            Some("echo nightly")
        );
        assert!(config.schedules[0].enabled);
        let daemon = config.daemon.as_ref().expect("daemon config should be parsed");
        assert_eq!(daemon.interval_secs, Some(300));
        assert_eq!(daemon.max_agents, Some(2));
        assert_eq!(daemon.active_hours.as_deref(), Some("00:00-06:00"));
        assert!(daemon.auto_run_ready);
    }

    #[test]
    fn yaml_merge_overrides_new_sections() {
        let base_yaml = r#"
mcp_servers:
  mcp-go:
    command: "node"
    args: ["server.js"]
    tools: ["search"]

tools:
  cli-gpt:
    executable: "gpt-cli"
    context_window: 32000
    base_args: []

schedules:
  - id: nightly
    cron: "0 2 * * *"
    command: "echo nightly"

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
        let overlay_yaml = r#"
mcp_servers:
  mcp-go:
    command: "bun"
    args: ["run", "server.js"]
    tools: ["search"]

schedules:
  - id: nightly
    cron: "0 3 * * *"
    command: "echo updated nightly"
  - id: weekly
    cron: "0 4 * * 0"
    command: "echo weekly"

integrations:
  git:
    provider: github
    auto_pr: true
    base_branch: main
"#;
        let base = parse_yaml_workflow_config(base_yaml).expect("parse base");
        let overlay = parse_yaml_workflow_config(overlay_yaml).expect("parse overlay");
        let merged = merge_yaml_into_config(base, overlay);
        let server = merged
            .mcp_servers
            .get("mcp-go")
            .expect("mcp server should be merged");
        assert_eq!(server.command, "bun");
        assert_eq!(merged.schedules.len(), 2);
        let nightly = merged
            .schedules
            .iter()
            .find(|schedule| schedule.id == "nightly")
            .expect("nightly should be merged");
        assert_eq!(nightly.cron, "0 3 * * *");
        assert!(
            merged.integrations.is_some()
        );
        assert_eq!(
            merged.integrations.unwrap().git.as_ref().and_then(|git| git.base_branch.as_deref()),
            Some("main")
        );
    }

    #[test]
    fn yaml_parses_top_level_mcp_servers() {
        let yaml = r#"
mcp_servers:
  ao:
    command: "node"
    args: ["server.js"]
    tools:
      - search

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse MCP servers");
        let server = config
            .mcp_servers
            .get("ao")
            .expect("MCP server should be parsed");
        assert_eq!(server.command, "node");
        assert_eq!(server.args, vec!["server.js"]);
        assert_eq!(server.tools, vec!["search"]);
    }

    #[test]
    fn yaml_parses_agent_profile_referencing_top_level_mcp_server() {
        let yaml = r#"
mcp_servers:
  ao:
    command: "node"
    args: ["server.js"]
    tools:
      - search
agents:
  researcher:
    system_prompt: "You are a research agent focused on code analysis"
    mcp_servers:
      - ao

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse");
        let profile = &config.agent_profiles["researcher"];
        assert_eq!(profile.mcp_servers, vec!["ao".to_string()]);
        assert!(validate_workflow_config(&config).is_ok());
    }

    #[test]
    fn validate_rejects_agent_profile_unknown_mcp_server_reference() {
        let yaml = r#"
mcp_servers:
  ao:
    command: "node"
    args: ["server.js"]
    tools:
      - search
agents:
  researcher:
    system_prompt: "You are a research agent focused on code analysis"
    mcp_servers:
      - missing

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse");
        let err = validate_workflow_config(&config).expect_err("should reject missing MCP reference");
        let message = err.to_string();
        assert!(
            message.contains(
                "agent_profiles['researcher'].mcp_servers references unknown MCP server 'missing'"
            ),
            "error should mention unknown MCP server reference: {}",
            message
        );
    }

    #[test]
    fn yaml_rejects_agent_mode_phase() {
        let yaml = r#"
phases:
  research:
    mode: agent

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
        let err = parse_yaml_workflow_config(yaml)
            .expect_err("should reject agent mode in YAML phases");
        let message = format!("{:#}", err);
        assert!(
            message.contains("not supported in YAML"),
            "error should mention YAML restriction: {}",
            message
        );
    }

    #[test]
    fn yaml_rejects_missing_command_block() {
        let yaml = r#"
phases:
  build:
    mode: command

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
        let err = parse_yaml_workflow_config(yaml)
            .expect_err("should reject command mode without command block");
        let message = format!("{:#}", err);
        assert!(
            message.contains("requires a command block"),
            "error should mention missing command block: {}",
            message
        );
    }

    #[test]
    fn yaml_rejects_missing_manual_block() {
        let yaml = r#"
phases:
  approval:
    mode: manual

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
        let err = parse_yaml_workflow_config(yaml)
            .expect_err("should reject manual mode without manual block");
        let message = format!("{:#}", err);
        assert!(
            message.contains("requires a manual block"),
            "error should mention missing manual block: {}",
            message
        );
    }

    #[test]
    fn yaml_merge_combines_phase_definitions() {
        let base_yaml = r#"
phases:
  build:
    mode: command
    command:
      program: cargo
      args: ["build"]

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - build
      - testing
"#;
        let overlay_yaml = r#"
phases:
  lint:
    mode: command
    command:
      program: cargo
      args: ["clippy"]
"#;
        let base = parse_yaml_workflow_config(base_yaml).expect("parse base");
        let overlay = parse_yaml_workflow_config(overlay_yaml).expect("parse overlay");
        let merged = merge_yaml_into_config(base, overlay);
        assert!(merged.phase_definitions.contains_key("build"));
        assert!(merged.phase_definitions.contains_key("lint"));
    }

    #[test]
    fn yaml_merge_combines_agent_profiles() {
        let base_yaml = r#"
agents:
  researcher:
    system_prompt: "Research agent"
    model: gemini-3.1-pro-preview

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - testing
"#;
        let overlay_yaml = r#"
agents:
  implementer:
    system_prompt: "Implementation agent"
    model: claude-sonnet-4-6
"#;
        let base = parse_yaml_workflow_config(base_yaml).expect("parse base");
        let overlay = parse_yaml_workflow_config(overlay_yaml).expect("parse overlay");
        let merged = merge_yaml_into_config(base, overlay);
        assert!(merged.agent_profiles.contains_key("researcher"));
        assert!(merged.agent_profiles.contains_key("implementer"));
    }

    #[test]
    fn yaml_merge_deduplicates_tools_allowlist() {
        let base_yaml = r#"
tools_allowlist:
  - cargo
  - npm

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
        let overlay_yaml = r#"
tools_allowlist:
  - cargo
  - python
"#;
        let base = parse_yaml_workflow_config(base_yaml).expect("parse base");
        let overlay = parse_yaml_workflow_config(overlay_yaml).expect("parse overlay");
        let merged = merge_yaml_into_config(base, overlay);
        assert!(merged.tools_allowlist.contains(&"cargo".to_string()));
        assert!(merged.tools_allowlist.contains(&"npm".to_string()));
        assert!(merged.tools_allowlist.contains(&"python".to_string()));
        let cargo_count = merged
            .tools_allowlist
            .iter()
            .filter(|t| *t == "cargo")
            .count();
        assert_eq!(cargo_count, 1, "cargo should appear only once after merge");
    }

    #[test]
    fn cross_validation_accepts_workflow_defined_phases() {
        let yaml = r#"
phases:
  build:
    mode: command
    command:
      program: cargo
      args: ["build"]

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - build
      - testing
"#;
        let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
        let runtime = crate::agent_runtime_config::builtin_agent_runtime_config();
        let result = validate_workflow_and_runtime_configs(&config, &runtime);
        assert!(
            result.is_ok(),
            "cross-validation should pass for workflow-defined phase: {:?}",
            result.err()
        );
    }

    #[test]
    fn validate_rejects_command_program_not_in_allowlist() {
        let mut config = builtin_workflow_config();
        config.tools_allowlist = vec!["npm".to_string()];
        config.phase_definitions.insert(
            "build".to_string(),
            PhaseExecutionDefinition {
                mode: PhaseExecutionMode::Command,
                agent_id: None,
                directive: None,
                runtime: None,
                capabilities: None,
                output_contract: None,
                output_json_schema: None,
                decision_contract: None,
                retry: None,
                command: Some(PhaseCommandDefinition {
                    program: "cargo".to_string(),
                    args: vec!["build".to_string()],
                    env: BTreeMap::new(),
                    cwd_mode: CommandCwdMode::ProjectRoot,
                    cwd_path: None,
                    timeout_secs: None,
                    success_exit_codes: vec![0],
                    parse_json_output: false,
                    expected_result_kind: None,
                    expected_schema: None,
                }),
                manual: None,
            },
        );
        let err = validate_workflow_config(&config)
            .expect_err("should reject program not in allowlist");
        let message = err.to_string();
        assert!(
            message.contains("not in tools_allowlist"),
            "error should mention allowlist: {}",
            message
        );
    }

    #[test]
    fn validate_rejects_invalid_unified_sections() {
        let mut config = builtin_workflow_config();
        config.schedules.push(WorkflowSchedule {
            id: "nightly".to_string(),
            cron: "".to_string(),
            pipeline: None,
            command: None,
            input: None,
            enabled: true,
        });
        config.tools.insert(
            "cli-gpt".to_string(),
            ToolDefinition {
                executable: "".to_string(),
                supports_mcp: true,
                supports_write: false,
                context_window: Some(0),
                base_args: vec!["".to_string()],
            },
        );
        config.mcp_servers.insert(
            "example".to_string(),
            McpServerDefinition {
                command: "".to_string(),
                args: vec!["".to_string()],
                transport: Some(" ".to_string()),
                config: BTreeMap::new(),
                tools: vec!["".to_string()],
                env: BTreeMap::from([("".to_string(), "value".to_string())]),
            },
        );
        let err = validate_workflow_config(&config).expect_err("invalid unified config should fail");
        let message = err.to_string();
        assert!(
            message.contains("schedules['nightly'] must define a pipeline or command"),
            "error should mention missing schedule target: {}",
            message
        );
        assert!(
            message.contains("schedules['nightly'].cron must not be empty"),
            "error should mention empty schedule cron: {}",
            message
        );
        assert!(
            message.contains("tools['cli-gpt'].executable must not be empty"),
            "error should mention invalid tool executable: {}",
            message
        );
        assert!(
            message.contains("tools['cli-gpt'].context_window must be greater than 0 when set"),
            "error should mention tool context window: {}",
            message
        );
        assert!(
            message.contains("tools['cli-gpt'].base_args must not contain empty values"),
            "error should mention tool args: {}",
            message
        );
        assert!(
            message.contains("mcp_servers['example'].command must not be empty"),
            "error should mention MCP command: {}",
            message
        );
    }

    #[test]
    fn yaml_agent_profile_with_all_fields_deserializes() {
        let yaml = r#"
agents:
  full-agent:
    description: "A fully configured agent"
    system_prompt: "You are a specialized agent"
    role: "researcher"
    tool: claude
    model: claude-sonnet-4-6
    fallback_models:
      - claude-haiku-4-5
    reasoning_effort: high
    web_search: true
    network_access: false
    timeout_secs: 600
    max_attempts: 3
    skills:
      - deep-search
      - code-analysis
    capabilities:
      code_execution: true
      file_write: false
    tool_policy:
      allow:
        - Read
        - Grep
      deny:
        - Write

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse full agent profile");
        let agent = &config.agent_profiles["full-agent"];
        assert_eq!(agent.description, "A fully configured agent");
        assert_eq!(agent.system_prompt, "You are a specialized agent");
        assert_eq!(agent.role.as_deref(), Some("researcher"));
        assert_eq!(agent.tool.as_deref(), Some("claude"));
        assert_eq!(agent.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(agent.fallback_models, vec!["claude-haiku-4-5"]);
        assert_eq!(agent.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(agent.web_search, Some(true));
        assert_eq!(agent.network_access, Some(false));
        assert_eq!(agent.timeout_secs, Some(600));
        assert_eq!(agent.max_attempts, Some(3));
        assert_eq!(agent.skills, vec!["deep-search", "code-analysis"]);
        assert_eq!(agent.capabilities.get("code_execution"), Some(&true));
        assert_eq!(agent.capabilities.get("file_write"), Some(&false));
        assert_eq!(agent.tool_policy.allow, vec!["Read", "Grep"]);
        assert_eq!(agent.tool_policy.deny, vec!["Write"]);
    }

    #[test]
    fn yaml_command_phase_with_all_options() {
        let yaml = r#"
phases:
  custom-build:
    mode: command
    directive: "Build with custom settings"
    command:
      program: make
      args: ["all", "-j4"]
      env:
        CC: gcc
        CFLAGS: "-O2"
      cwd_mode: task_root
      timeout_secs: 600
      success_exit_codes: [0, 2]
      parse_json_output: true

pipelines:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
        let config = parse_yaml_workflow_config(yaml).expect("should parse");
        let phase = &config.phase_definitions["custom-build"];
        assert_eq!(phase.directive.as_deref(), Some("Build with custom settings"));
        let cmd = phase.command.as_ref().expect("command");
        assert_eq!(cmd.program, "make");
        assert_eq!(cmd.args, vec!["all", "-j4"]);
        assert_eq!(cmd.env.get("CC"), Some(&"gcc".to_string()));
        assert_eq!(cmd.cwd_mode, CommandCwdMode::TaskRoot);
        assert_eq!(cmd.timeout_secs, Some(600));
        assert_eq!(cmd.success_exit_codes, vec![0, 2]);
        assert!(cmd.parse_json_output);
    }

    #[test]
    fn existing_configs_without_new_fields_deserialize() {
        let json = serde_json::json!({
            "schema": WORKFLOW_CONFIG_SCHEMA_ID,
            "version": WORKFLOW_CONFIG_VERSION,
            "default_pipeline_id": "standard",
            "phase_catalog": {
                "requirements": {
                    "label": "Requirements",
                    "description": "",
                    "category": "planning",
                    "visible": true,
                    "tags": []
                }
            },
            "pipelines": [{
                "id": "standard",
                "name": "Standard",
                "description": "",
                "phases": ["requirements"]
            }]
        });
        let config: WorkflowConfig =
            serde_json::from_value(json).expect("should deserialize without new fields");
        assert!(config.phase_definitions.is_empty());
        assert!(config.agent_profiles.is_empty());
        assert!(config.tools_allowlist.is_empty());
        assert!(config.mcp_servers.is_empty());
        assert!(config.tools.is_empty());
        assert!(config.schedules.is_empty());
        assert!(config.integrations.is_none());
        assert!(config.daemon.is_none());
    }

    #[test]
    fn new_fields_skip_serializing_when_empty() {
        let config = builtin_workflow_config();
        let json = serde_json::to_value(&config).expect("serialize");
        let obj = json.as_object().expect("should be object");
        assert!(
            !obj.contains_key("phase_definitions"),
            "empty phase_definitions should not be serialized"
        );
        assert!(
            !obj.contains_key("agent_profiles"),
            "empty agent_profiles should not be serialized"
        );
        assert!(
            !obj.contains_key("tools_allowlist"),
            "empty tools_allowlist should not be serialized"
        );
        assert!(
            !obj.contains_key("mcp_servers"),
            "empty mcp_servers should not be serialized"
        );
        assert!(
            !obj.contains_key("tools"),
            "empty tools should not be serialized"
        );
        assert!(
            !obj.contains_key("schedules"),
            "empty schedules should not be serialized"
        );
        assert!(
            !obj.contains_key("integrations"),
            "empty integrations should not be serialized"
        );
        assert!(
            !obj.contains_key("daemon"),
            "empty daemon should not be serialized"
        );
    }
}
