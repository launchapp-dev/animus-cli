use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

use super::builtins::builtin_workflow_config;
use super::types::*;
use super::validation::validate_workflow_config_with_project_root;
use super::yaml_compiler::{compile_yaml_workflow_files, merge_yaml_into_config, yaml_workflows_dir};
use super::yaml_scaffold::ensure_workflow_yaml_scaffold;
use super::yaml_types::GENERATED_WORKFLOW_OVERLAY_FILE_NAME;

pub fn workflow_config_path(project_root: &Path) -> PathBuf {
    let base = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".ao"));
    base.join("state").join(WORKFLOW_CONFIG_FILE_NAME)
}

pub fn legacy_workflow_config_paths(project_root: &Path) -> [PathBuf; 2] {
    [
        project_root.join(".ao").join("state").join("workflow-config.json"),
        project_root.join(".ao").join("workflow-config.json"),
    ]
}

pub fn ensure_workflow_config_file(project_root: &Path) -> Result<()> {
    ensure_workflow_yaml_scaffold(project_root).map(|_| ())
}

pub fn ensure_workflow_config_compiled(project_root: &Path) -> Result<()> {
    if let Some(yaml_config) = compile_yaml_workflow_files(project_root)? {
        let config = merge_yaml_into_config(builtin_workflow_config(), yaml_config);
        validate_workflow_config_with_project_root(&config, Some(project_root))?;
    }
    Ok(())
}

pub fn load_workflow_config(project_root: &Path) -> Result<WorkflowConfig> {
    Ok(load_workflow_config_with_metadata(project_root)?.config)
}

pub fn load_workflow_config_with_metadata(project_root: &Path) -> Result<LoadedWorkflowConfig> {
    if let Some(yaml_config) = compile_yaml_workflow_files(project_root)? {
        let config = merge_yaml_into_config(builtin_workflow_config(), yaml_config);
        validate_workflow_config_with_project_root(&config, Some(project_root))?;

        let single_file = project_root.join(".ao").join("workflows.yaml");
        let workflows_dir = yaml_workflows_dir(project_root);
        let path = if single_file.exists() { single_file } else { workflows_dir };

        return Ok(LoadedWorkflowConfig {
            metadata: WorkflowConfigMetadata {
                schema: config.schema.clone(),
                version: config.version,
                hash: workflow_config_hash(&config),
                source: WorkflowConfigSource::Yaml,
            },
            config,
            path,
        });
    }

    let path = workflow_config_path(project_root);
    if let Some(legacy_path) = legacy_workflow_config_paths(project_root).iter().find(|candidate| candidate.exists()) {
        return Err(anyhow!(
            "workflow config v2 JSON is no longer supported at {} (found unsupported legacy file at {}). Remove the JSON config and define workflows in .ao/workflows.yaml or .ao/workflows/*.yaml",
            path.display(),
            legacy_path.display()
        ));
    }

    if path.exists() {
        return Err(anyhow!(
            "workflow config JSON is no longer supported at {}. Remove the JSON config and define workflows in .ao/workflows.yaml or .ao/workflows/*.yaml",
            path.display()
        ));
    }

    Err(anyhow!("workflow config is missing. Define workflows in .ao/workflows.yaml or .ao/workflows/*.yaml"))
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
    validate_workflow_config_with_project_root(config, Some(project_root))?;
    super::yaml_compiler::write_workflow_yaml_overlay(project_root, GENERATED_WORKFLOW_OVERLAY_FILE_NAME, config)
        .map(|_| ())
}

pub fn workflow_config_hash(config: &WorkflowConfig) -> String {
    let bytes = serde_json::to_vec(config).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
