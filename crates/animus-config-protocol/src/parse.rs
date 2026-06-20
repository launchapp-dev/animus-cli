//! The standardized YAML parser entry points: read project `.animus` YAML,
//! interpolate env (`${VAR}`), parse onto a base, and merge multi-file overlays
//! into one canonical [`WorkflowConfig`].
//!
//! This is the library the kernel (`orchestrator-config`) and the
//! `animus-config-yaml` plugin both consume. `${secret.<name>}` references are
//! NOT resolved here — they survive verbatim into the parsed config and are
//! resolved later (at consume/spawn time) from the OS keychain.
//!
//! `merge_yaml_into_config` is the pure `WorkflowConfig`-overlay merge used both
//! to combine multiple YAML sources during parse AND by the kernel's
//! pack-overlay compiler (which re-exports it).

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::builtins::builtin_workflow_config;
use crate::env_interp::{interpolate_env, lint_sensitive_interpolations};
use crate::workflow_types::*;
use crate::yaml_parser::{
    parse_yaml_workflow_config_confined_to_pack, parse_yaml_workflow_config_with_base_source_and_original,
};

pub const YAML_WORKFLOWS_DIR: &str = "workflows";

/// `.animus/workflows/` directory under `project_root`.
pub fn yaml_workflows_dir(project_root: &Path) -> PathBuf {
    project_root.join(".animus").join(YAML_WORKFLOWS_DIR)
}

/// Collect the project's `.animus` YAML sources in deterministic order:
/// `.animus/workflows.yaml` first, then `.animus/workflows/*.{yaml,yml}` sorted.
pub fn collect_project_yaml_workflow_sources(project_root: &Path) -> Result<Vec<(PathBuf, String)>> {
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

/// Compile YAML sources onto `base`, merging multiple files left-to-right.
pub fn compile_yaml_sources_with_base(
    base: &WorkflowConfig,
    yaml_sources: &[(PathBuf, String)],
) -> Result<Option<WorkflowConfig>> {
    compile_yaml_sources_with_base_inner(base, yaml_sources, None)
}

/// Compile pack YAML sources onto `base`, confining file references to `pack_root`.
pub fn compile_yaml_sources_confined_to_pack(
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

    // `${secret.*}` is no longer resolved at parse time, so there is nothing to
    // collect for diagnostic redaction — pass an empty map to the parser.
    let no_resolved_secrets: BTreeMap<String, String> = BTreeMap::new();

    let mut merged_config: Option<WorkflowConfig> = None;
    for (path, content) in yaml_sources {
        let overlay_base = merged_config.as_ref().unwrap_or(base);
        let source_label = path.display().to_string();
        for warning in lint_sensitive_interpolations(content, &source_label) {
            eprintln!("warning: {}", warning);
        }
        // Env-only interpolation; `${secret.*}` survives verbatim into the
        // parsed config.
        let resolved = interpolate_env(content, &source_label)
            .with_context(|| format!("env-var interpolation failed for {}", source_label))?;
        let parsed = match pack_root {
            Some(root) => parse_yaml_workflow_config_confined_to_pack(
                &resolved,
                overlay_base,
                path.as_path(),
                root,
                content,
                &no_resolved_secrets,
            )
            .with_context(|| format!("error in pack YAML file {}", source_label))?,
            None => parse_yaml_workflow_config_with_base_source_and_original(
                &resolved,
                overlay_base,
                Some(path.as_path()),
                content,
                &no_resolved_secrets,
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

/// Read + compile the project's `.animus` YAML onto the builtin base.
/// Returns `Ok(None)` when the project has no YAML sources.
pub fn compile_yaml_workflow_files(project_root: &Path) -> Result<Option<WorkflowConfig>> {
    let yaml_sources = collect_project_yaml_workflow_sources(project_root)?;
    compile_yaml_sources_with_base(&builtin_workflow_config(), &yaml_sources)
}

/// Merge a parsed YAML `WorkflowConfig` overlay onto `base`. Pure
/// `WorkflowConfig` → `WorkflowConfig`. Used to combine multiple YAML sources
/// during parse and by the kernel's pack-overlay compiler.
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
            if overlay.active_hours.is_some() {
                base.active_hours = overlay.active_hours;
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
