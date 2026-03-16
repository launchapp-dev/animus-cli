use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

use super::builtins::{builtin_workflow_config, builtin_workflow_yaml_overlays, bundled_kernel_workflow_config_base};
use super::types::*;
use super::validation::validate_workflow_config_with_project_root;
use super::yaml_compiler::{merge_yaml_into_config, yaml_workflows_dir};
use super::yaml_parser::parse_yaml_workflow_config_with_base;
use super::yaml_scaffold::ensure_workflow_yaml_scaffold;
use super::yaml_types::GENERATED_WORKFLOW_OVERLAY_FILE_NAME;
use crate::{
    load_pack_workflow_overlay, machine_installed_packs_dir, resolve_pack_registry, validate_active_pack_configuration,
    PackRegistrySource,
};

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
    let yaml_sources = super::collect_project_yaml_workflow_sources(project_root)?;
    let registry = resolve_pack_registry(project_root)?;
    if yaml_sources.is_empty() && !registry.has_pack_overlays() {
        return Ok(());
    }

    load_workflow_config_with_metadata(project_root).map(|_| ())
}

pub fn load_workflow_config(project_root: &Path) -> Result<WorkflowConfig> {
    Ok(load_workflow_config_with_metadata(project_root)?.config)
}

pub fn load_workflow_config_with_metadata(project_root: &Path) -> Result<LoadedWorkflowConfig> {
    let yaml_sources = super::collect_project_yaml_workflow_sources(project_root)?;
    let registry = resolve_pack_registry(project_root)?;
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

    if !yaml_sources.is_empty() || registry.has_pack_overlays() {
        validate_active_pack_configuration(&registry)?;
        let (mut config, mut path) = build_pack_aware_builtin_workflow_config(project_root, &registry)?;

        for entry in registry.entries_for_source(PackRegistrySource::Installed) {
            let Some(pack) = entry.loaded_manifest() else {
                continue;
            };
            if let Some(overlay) = load_pack_workflow_overlay(pack, &config)? {
                config = merge_yaml_into_config(config, overlay);
                path = entry.pack_root.clone().unwrap_or_else(machine_installed_packs_dir);
            }
        }

        if let Some(yaml_config) = super::compile_yaml_sources_with_base(&config, &yaml_sources)? {
            config = merge_yaml_into_config(config, yaml_config);
            let single_file = project_root.join(".ao").join("workflows.yaml");
            let workflows_dir = yaml_workflows_dir(project_root);
            path = if single_file.exists() { single_file } else { workflows_dir };
        }

        for entry in registry.entries_for_source(PackRegistrySource::ProjectOverride) {
            let Some(pack) = entry.loaded_manifest() else {
                continue;
            };
            if let Some(overlay) = load_pack_workflow_overlay(pack, &config)? {
                config = merge_yaml_into_config(config, overlay);
                path = entry.pack_root.clone().unwrap_or_else(|| project_root.join(".ao").join("plugins"));
            }
        }

        validate_workflow_config_with_project_root(&config, Some(project_root))?;

        let source = if yaml_sources.is_empty() && !registry.has_external_packs() {
            WorkflowConfigSource::Builtin
        } else {
            WorkflowConfigSource::Yaml
        };

        return Ok(LoadedWorkflowConfig {
            metadata: WorkflowConfigMetadata {
                schema: config.schema.clone(),
                version: config.version,
                hash: workflow_config_hash(&config),
                source,
            },
            config,
            path,
        });
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

fn build_pack_aware_builtin_workflow_config(
    project_root: &Path,
    registry: &crate::ResolvedPackRegistry,
) -> Result<(WorkflowConfig, PathBuf)> {
    let mut config = bundled_kernel_workflow_config_base();
    let mut path = workflow_config_path(project_root);

    for (name, yaml) in builtin_workflow_yaml_overlays() {
        let overlay = parse_yaml_workflow_config_with_base(yaml, &config)
            .map_err(|error| anyhow!("invalid builtin workflow YAML '{name}': {error}"))?;
        config = merge_yaml_into_config(config, overlay);
    }

    for entry in registry.entries_for_source(PackRegistrySource::Bundled) {
        let Some(pack) = entry.loaded_manifest() else {
            continue;
        };
        if let Some(overlay) = load_pack_workflow_overlay(pack, &config)? {
            config = merge_yaml_into_config(config, overlay);
            if let Some(pack_root) = entry.pack_root.as_ref() {
                path = pack_root.clone();
            }
        }
    }

    Ok((config, path))
}

pub fn workflow_config_hash(config: &WorkflowConfig) -> String {
    let bytes = serde_json::to_vec(config).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
