use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use std::collections::BTreeMap;

use super::builtins::builtin_workflow_config;
use super::env_interp::{interpolate_env, interpolate_env_and_secrets_collecting, lint_sensitive_interpolations};
use super::types::*;
use super::yaml_parser::{
    parse_yaml_workflow_config_confined_to_pack, parse_yaml_workflow_config_with_base_source_and_original,
    phase_execution_definition_to_yaml, workflow_config_to_yaml_file, workflow_definition_to_yaml,
};
use super::yaml_types::*;
use crate::agent_runtime_config::PhaseExecutionDefinition;

/// Minimal deserialization shape used to extract just the `secrets:` block
/// from a YAML source before the full file is parsed. Used so that
/// `${secret.<name>}` interpolation can resolve against declared secrets
/// before the rest of the file is interpreted.
#[derive(serde::Deserialize)]
struct SecretsExtract {
    #[serde(default)]
    secrets: BTreeMap<String, SecretRef>,
}

fn extract_declared_secrets(yaml: &str) -> BTreeMap<String, SecretRef> {
    serde_yaml::from_str::<SecretsExtract>(yaml).map(|s| s.secrets).unwrap_or_default()
}

pub fn yaml_workflows_dir(project_root: &Path) -> PathBuf {
    project_root.join(".animus").join(YAML_WORKFLOWS_DIR)
}

pub(crate) fn collect_project_yaml_workflow_sources(project_root: &Path) -> Result<Vec<(PathBuf, String)>> {
    let workflows_dir = yaml_workflows_dir(project_root);
    let single_file = project_root.join(".animus").join("workflows.yaml");

    let mut yaml_sources: Vec<(PathBuf, String)> = Vec::new();

    if single_file.exists() {
        let content =
            fs::read_to_string(&single_file).with_context(|| format!("failed to read {}", single_file.display()))?;
        yaml_sources.push((single_file, content));
    }

    if workflows_dir.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(&workflows_dir)
            .with_context(|| format!("failed to read directory {}", workflows_dir.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().map(|ext| ext == "yaml" || ext == "yml").unwrap_or(false))
            .collect();
        entries.sort_by_key(|e| e.path());

        for entry in entries {
            let path = entry.path();
            let content = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
            yaml_sources.push((path, content));
        }
    }

    if yaml_sources.is_empty() {
        return Ok(Vec::new());
    }

    Ok(yaml_sources)
}

pub(crate) fn compile_yaml_sources_with_base(
    base: &WorkflowConfig,
    yaml_sources: &[(PathBuf, String)],
) -> Result<Option<WorkflowConfig>> {
    compile_yaml_sources_with_base_inner(base, yaml_sources, None)
}

pub(crate) fn compile_yaml_sources_confined_to_pack(
    base: &WorkflowConfig,
    yaml_sources: &[(PathBuf, String)],
    pack_root: &Path,
) -> Result<Option<WorkflowConfig>> {
    compile_yaml_sources_with_base_inner(base, yaml_sources, Some(pack_root))
}

fn compile_yaml_sources_with_base_inner(
    base: &WorkflowConfig,
    yaml_sources: &[(PathBuf, String)],
    pack_root: Option<&Path>,
) -> Result<Option<WorkflowConfig>> {
    if yaml_sources.is_empty() {
        return Ok(None);
    }

    let mut merged_config: Option<WorkflowConfig> = None;
    for (path, content) in yaml_sources {
        let overlay_base = merged_config.as_ref().unwrap_or(base);
        let source_label = path.display().to_string();
        for warning in lint_sensitive_interpolations(content, &source_label) {
            eprintln!("warning: {}", warning);
        }
        for warning in super::validation::unenforced_yaml_field_warnings(content, &source_label) {
            eprintln!("warning: {}", warning);
        }
        // The env-only pass exists solely to make the `secrets:` block
        // parseable (its `env:` mappings may themselves use `${VAR}`). The
        // final document is produced by a single combined pass over the
        // ORIGINAL content so substituted values are never re-scanned.
        let substituted = interpolate_env(content, &source_label)
            .with_context(|| format!("env-var interpolation failed for {}", source_label))?;
        // Allow secrets declared in earlier-merged overlays (or the base
        // config) to satisfy ${secret.X} references in this file. Locally
        // declared secrets take precedence on collision so an overlay can
        // remap a name if it really needs to.
        let mut declared_secrets = overlay_base.secrets.clone();
        for (key, value) in extract_declared_secrets(&substituted) {
            declared_secrets.insert(key, value);
        }
        let (resolved, resolved_secrets) =
            interpolate_env_and_secrets_collecting(content, &source_label, &declared_secrets)
                .with_context(|| format!("secret interpolation failed for {}", source_label))?;
        let parsed = match pack_root {
            Some(root) => parse_yaml_workflow_config_confined_to_pack(
                &resolved,
                overlay_base,
                path.as_path(),
                root,
                content,
                &resolved_secrets,
            )
            .with_context(|| format!("error in pack YAML file {}", source_label))?,
            None => parse_yaml_workflow_config_with_base_source_and_original(
                &resolved,
                overlay_base,
                Some(path.as_path()),
                content,
                &resolved_secrets,
            )
            .with_context(|| format!("error in YAML file {}", source_label))?,
        };
        merged_config = Some(match merged_config {
            None => parsed,
            Some(base) => merge_yaml_into_config(base, parsed),
        });
    }

    Ok(merged_config)
}

pub fn compile_yaml_workflow_files(project_root: &Path) -> Result<Option<WorkflowConfig>> {
    let yaml_sources = collect_project_yaml_workflow_sources(project_root)?;
    compile_yaml_sources_with_base(&builtin_workflow_config(), &yaml_sources)
}

pub fn merge_yaml_into_config(base: WorkflowConfig, yaml: WorkflowConfig) -> WorkflowConfig {
    let mut workflows = base.workflows;

    for yaml_pipeline in yaml.workflows {
        if let Some(pos) = workflows.iter().position(|p| p.id.eq_ignore_ascii_case(&yaml_pipeline.id)) {
            workflows[pos] = yaml_pipeline;
        } else {
            workflows.push(yaml_pipeline);
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
        match agent_profiles.get_mut(&key) {
            Some(existing) => existing.merge_from(&value),
            None => {
                agent_profiles.insert(key, value);
            }
        }
    }

    let mut agent_channels = base.agent_channels;
    for (key, value) in yaml.agent_channels {
        agent_channels.insert(key, value);
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

    let mut phase_mcp_bindings = base.phase_mcp_bindings;
    for (phase_id, binding) in yaml.phase_mcp_bindings {
        phase_mcp_bindings.insert(phase_id, binding);
    }

    let mut tools = base.tools;
    for (name, definition) in yaml.tools {
        tools.insert(name, definition);
    }

    let mut schedules = base.schedules;
    for overlay_schedule in yaml.schedules {
        if let Some(pos) =
            schedules.iter().position(|schedule| schedule.id.eq_ignore_ascii_case(overlay_schedule.id.as_str()))
        {
            schedules[pos] = overlay_schedule;
        } else {
            schedules.push(overlay_schedule);
        }
    }

    let mut triggers = base.triggers;
    for overlay_trigger in yaml.triggers {
        if let Some(pos) =
            triggers.iter().position(|trigger| trigger.id.eq_ignore_ascii_case(overlay_trigger.id.as_str()))
        {
            triggers[pos] = overlay_trigger;
        } else {
            triggers.push(overlay_trigger);
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

    let default_workflow_ref =
        if yaml.default_workflow_ref != base.default_workflow_ref && !yaml.default_workflow_ref.is_empty() {
            yaml.default_workflow_ref
        } else {
            base.default_workflow_ref
        };

    let daemon = match (base.daemon, yaml.daemon) {
        (None, None) => None,
        (Some(base), None) => Some(base),
        (None, Some(overlay)) => Some(overlay),
        (Some(mut base), Some(overlay)) => {
            if overlay.interval_secs.is_some() {
                base.interval_secs = overlay.interval_secs;
            }
            if overlay.pool_size.is_some() {
                base.pool_size = overlay.pool_size;
            }
            if overlay.active_hours.is_some() {
                base.active_hours = overlay.active_hours;
            }
            if overlay.auto_run_ready {
                base.auto_run_ready = true;
            }
            if overlay.max_task_retries.is_some() {
                base.max_task_retries = overlay.max_task_retries;
            }
            if overlay.retry_cooldown_secs.is_some() {
                base.retry_cooldown_secs = overlay.retry_cooldown_secs;
            }
            if overlay.phase_routing.is_some() {
                base.phase_routing = overlay.phase_routing;
            }
            if overlay.mcp.is_some() {
                base.mcp = overlay.mcp;
            }
            if overlay.budget.is_some() {
                base.budget = overlay.budget;
            }
            Some(base)
        }
    };

    let mut secrets = base.secrets;
    for (key, value) in yaml.secrets {
        secrets.insert(key, value);
    }

    WorkflowConfig {
        schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: WORKFLOW_CONFIG_VERSION,
        default_workflow_ref,
        phase_catalog,
        workflows,
        checkpoint_retention: base.checkpoint_retention,
        phase_definitions,
        agent_profiles,
        agent_channels,
        tools_allowlist,
        mcp_servers,
        phase_mcp_bindings,
        tools,
        integrations,
        schedules,
        triggers,
        daemon,
        secrets,
    }
}

pub(super) fn write_yaml_workflow_overlay(
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

pub struct CompileYamlResult {
    pub config: WorkflowConfig,
    pub source_files: Vec<PathBuf>,
    pub output_path: PathBuf,
}

pub fn validate_and_compile_yaml_workflows(project_root: &Path) -> Result<Option<CompileYamlResult>> {
    let workflows_dir = yaml_workflows_dir(project_root);
    let single_file = project_root.join(".animus").join("workflows.yaml");

    let mut source_files: Vec<PathBuf> = Vec::new();
    if single_file.exists() {
        source_files.push(single_file.clone());
    }
    if workflows_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&workflows_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map(|ext| ext == "yaml" || ext == "yml").unwrap_or(false) {
                    source_files.push(path);
                }
            }
        }
    }
    source_files.sort();

    if source_files.is_empty() {
        return Ok(None);
    }

    let final_config = super::loading::load_workflow_config_with_metadata(project_root)?.config;
    let output_path = if single_file.exists() { single_file } else { workflows_dir };
    Ok(Some(CompileYamlResult { config: final_config, source_files, output_path }))
}
