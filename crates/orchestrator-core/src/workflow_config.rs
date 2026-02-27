use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::agent_runtime_config::AgentRuntimeConfig;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineDefinition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub phases: Vec<String>,
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
                        "requirements".to_string(),
                        "implementation".to_string(),
                        "code-review".to_string(),
                        "testing".to_string(),
                    ],
                },
                PipelineDefinition {
                    id: "ui-ux-standard".to_string(),
                    name: "UI UX Standard".to_string(),
                    description:
                        "Frontend-oriented flow with UX research, wireframes, and mockup review gates."
                            .to_string(),
                    phases: vec![
                        "requirements".to_string(),
                        "ux-research".to_string(),
                        "wireframe".to_string(),
                        "mockup-review".to_string(),
                        "implementation".to_string(),
                        "code-review".to_string(),
                        "testing".to_string(),
                    ],
                },
            ],
        })
        .clone()
}

pub fn workflow_config_path(project_root: &Path) -> PathBuf {
    project_root
        .join(".ao")
        .join("state")
        .join(WORKFLOW_CONFIG_FILE_NAME)
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let payload = serde_json::to_string_pretty(config)?;
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workflow-config"),
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

    let pipeline = config
        .pipelines
        .iter()
        .find(|pipeline| pipeline.id.eq_ignore_ascii_case(requested))?;

    let phases: Vec<String> = pipeline
        .phases
        .iter()
        .map(String::as_str)
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

pub fn validate_workflow_and_runtime_configs(
    workflow: &WorkflowConfig,
    runtime: &AgentRuntimeConfig,
) -> Result<()> {
    validate_workflow_config(workflow)?;

    let mut errors = Vec::new();
    for pipeline in &workflow.pipelines {
        for phase in &pipeline.phases {
            let phase_id = phase.trim();
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

            if !runtime.has_phase_definition(phase_id) {
                errors.push(format!(
                    "pipeline '{}' phase '{}' is missing from agent-runtime phases",
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

        for phase in &pipeline.phases {
            let phase_id = phase.trim();
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

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
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
}
