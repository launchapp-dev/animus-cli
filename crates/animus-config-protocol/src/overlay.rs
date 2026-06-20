//! Generated-overlay readers/writers: serialize a `WorkflowConfig` back into
//! the `.animus/workflows/generated-*.yaml` overlay files (used by the CLI's
//! `workflow phases add` / pipeline authoring surface).
//!
//! These live with the YAML types (which are crate-private) rather than in the
//! kernel, so the kernel re-exports them. They round-trip the overlay raw so
//! unresolved `${VAR}` / `${secret.X}` references are preserved verbatim;
//! compiled values never reach disk.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::agent_types::PhaseExecutionDefinition;
use crate::parse::yaml_workflows_dir;
use crate::workflow_types::*;
use crate::yaml_parser::{
    phase_execution_definition_to_yaml, workflow_config_to_yaml_file, workflow_definition_to_yaml,
};
use crate::yaml_types::*;

pub(crate) fn write_yaml_workflow_overlay(
    project_root: &Path,
    file_name: &str,
    yaml_file: &YamlWorkflowFile,
) -> Result<PathBuf> {
    let workflows_dir = yaml_workflows_dir(project_root);
    fs::create_dir_all(&workflows_dir).with_context(|| format!("failed to create {}", workflows_dir.display()))?;
    let path = workflows_dir.join(file_name);
    let content = serde_yaml::to_string(yaml_file).context("failed to serialize workflow YAML overlay")?;
    fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

pub fn write_workflow_yaml_overlay(project_root: &Path, file_name: &str, config: &WorkflowConfig) -> Result<PathBuf> {
    let yaml_file = workflow_config_to_yaml_file(config);
    write_yaml_workflow_overlay(project_root, file_name, &yaml_file)
}

/// Read a generated overlay file as raw, uninterpolated YAML. Missing files
/// parse as an empty overlay. `${VAR}` / `${secret.X}` references in the
/// file survive verbatim, so round-tripping through this reader never bakes
/// resolved values into the project tree.
fn read_yaml_workflow_overlay_raw(project_root: &Path, file_name: &str) -> Result<YamlWorkflowFile> {
    let path = yaml_workflows_dir(project_root).join(file_name);
    if !path.exists() {
        return Ok(YamlWorkflowFile::default());
    }
    let content = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_yaml::from_str(&content).with_context(|| format!("failed to parse generated overlay {}", path.display()))
}

/// Load the generated workflow overlay, keeping only the blocks the
/// phase/pipeline authoring surface owns (`phases`, `phase_catalog`,
/// `workflows`). Older Animus versions dumped the entire COMPILED config
/// (with `${VAR}` / `${secret.X}` references replaced by plaintext) into
/// this file; dropping the other blocks on rewrite restores the hand-written
/// YAML sources as their single source of truth and stops re-serializing
/// resolved secret values.
fn read_generated_workflow_authored_blocks(project_root: &Path) -> Result<YamlWorkflowFile> {
    let raw = read_yaml_workflow_overlay_raw(project_root, GENERATED_WORKFLOW_OVERLAY_FILE_NAME)?;
    Ok(YamlWorkflowFile {
        phase_catalog: raw.phase_catalog.filter(|catalog| !catalog.is_empty()),
        workflows: raw.workflows,
        phases: raw.phases,
        ..YamlWorkflowFile::default()
    })
}

/// Upsert a single phase definition (and optional catalog entry) into the
/// generated workflow overlay. The existing overlay is round-tripped raw so
/// unresolved `${VAR}` / `${secret.X}` references are preserved; compiled
/// values never reach disk.
pub fn upsert_generated_workflow_phase(
    project_root: &Path,
    phase_id: &str,
    definition: &PhaseExecutionDefinition,
    catalog_entry: Option<&PhaseUiDefinition>,
) -> Result<PathBuf> {
    let mut file = read_generated_workflow_authored_blocks(project_root)?;
    file.phases.retain(|existing, _| !existing.eq_ignore_ascii_case(phase_id));
    file.phases.insert(phase_id.to_string(), phase_execution_definition_to_yaml(definition));
    if let Some(entry) = catalog_entry {
        file.phase_catalog.get_or_insert_with(BTreeMap::new).insert(phase_id.to_string(), entry.clone());
    }
    write_yaml_workflow_overlay(project_root, GENERATED_WORKFLOW_OVERLAY_FILE_NAME, &file)
}

/// Upsert a single pipeline definition into the generated workflow overlay.
/// Same unresolved round-trip contract as [`upsert_generated_workflow_phase`].
pub fn upsert_generated_workflow_pipeline(project_root: &Path, pipeline: &WorkflowDefinition) -> Result<PathBuf> {
    let mut file = read_generated_workflow_authored_blocks(project_root)?;
    let yaml_pipeline = workflow_definition_to_yaml(pipeline);
    if let Some(existing) = file.workflows.iter_mut().find(|existing| existing.id.eq_ignore_ascii_case(&pipeline.id)) {
        *existing = yaml_pipeline;
    } else {
        file.workflows.push(yaml_pipeline);
    }
    write_yaml_workflow_overlay(project_root, GENERATED_WORKFLOW_OVERLAY_FILE_NAME, &file)
}

/// Whether any generated overlay file defines `phase_id` (case-insensitive).
/// `animus workflow phases remove` can only prune overlay-defined phases, so
/// the dry-run preview uses this to report removability accurately.
pub fn generated_workflow_phase_is_defined(project_root: &Path, phase_id: &str) -> Result<bool> {
    for file_name in [GENERATED_WORKFLOW_OVERLAY_FILE_NAME, GENERATED_RUNTIME_OVERLAY_FILE_NAME] {
        if !yaml_workflows_dir(project_root).join(file_name).exists() {
            continue;
        }
        let file = read_yaml_workflow_overlay_raw(project_root, file_name)?;
        if file.phases.keys().any(|existing| existing.eq_ignore_ascii_case(phase_id)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Remove a phase definition (and its catalog entry) from the generated
/// overlay files. Returns `true` when at least one overlay contained the
/// phase; `false` means the phase is defined in a hand-authored YAML source
/// or pack and must be removed there.
pub fn remove_generated_workflow_phase(project_root: &Path, phase_id: &str) -> Result<bool> {
    let mut removed = false;
    for file_name in [GENERATED_WORKFLOW_OVERLAY_FILE_NAME, GENERATED_RUNTIME_OVERLAY_FILE_NAME] {
        if !yaml_workflows_dir(project_root).join(file_name).exists() {
            continue;
        }
        let mut file = read_yaml_workflow_overlay_raw(project_root, file_name)?;
        let phase_count = file.phases.len();
        file.phases.retain(|existing, _| !existing.eq_ignore_ascii_case(phase_id));
        let mut changed = file.phases.len() != phase_count;
        if let Some(catalog) = file.phase_catalog.as_mut() {
            let catalog_count = catalog.len();
            catalog.retain(|existing, _| !existing.eq_ignore_ascii_case(phase_id));
            changed |= catalog.len() != catalog_count;
        }
        if changed {
            write_yaml_workflow_overlay(project_root, file_name, &file)?;
            removed = true;
        }
    }
    Ok(removed)
}
