use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

use super::builtins::builtin_workflow_config_base;
use super::types::*;
use super::validation::validate_workflow_config_with_project_root;
use super::yaml_compiler::{merge_yaml_into_config, yaml_workflows_dir};
use super::yaml_scaffold::ensure_workflow_yaml_scaffold;
use super::yaml_types::GENERATED_WORKFLOW_OVERLAY_FILE_NAME;
use crate::cache::WorkflowCacheInput;
use crate::pack_config::LoadedPackManifest;
use crate::{
    load_pack_workflow_overlay, machine_installed_packs_dir, resolve_pack_registry, validate_active_pack_configuration,
    PackRegistrySource, ResolvedPackRegistry,
};

pub fn workflow_config_path(project_root: &Path) -> PathBuf {
    let base = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".animus"));
    base.join("config").join(WORKFLOW_CONFIG_FILE_NAME)
}

pub fn legacy_workflow_config_paths(project_root: &Path) -> [PathBuf; 2] {
    [
        project_root.join(".animus").join("state").join("workflow-config.json"),
        project_root.join(".animus").join("workflow-config.json"),
    ]
}

pub fn ensure_workflow_config_file(project_root: &Path) -> Result<()> {
    ensure_workflow_yaml_scaffold(project_root).map(|_| ())
}

pub fn ensure_workflow_config_compiled(project_root: &Path) -> Result<()> {
    let yaml_sources = super::collect_project_yaml_workflow_sources(project_root)?;
    let registry = resolve_pack_registry(project_root)?;
    if yaml_sources.is_empty()
        && !registry.has_pack_overlays()
        && !super::config_source_client::config_source_installed(project_root)
    {
        return Ok(());
    }

    load_workflow_config_with_metadata(project_root).map(|_| ())
}

pub fn load_workflow_config(project_root: &Path) -> Result<WorkflowConfig> {
    Ok(load_workflow_config_with_metadata(project_root)?.config)
}

pub fn load_workflow_config_with_metadata(project_root: &Path) -> Result<LoadedWorkflowConfig> {
    // v0.6: if a `config_source` plugin is installed, it produces the base config;
    // otherwise fall back to the in-tree YAML acquisition (the default, unchanged).
    let plugin_base = super::config_source_client::resolve_plugin_base(project_root)?;
    let yaml_sources = if plugin_base.is_some() {
        Vec::new()
    } else {
        super::collect_project_yaml_workflow_sources(project_root)?
    };
    let registry = resolve_pack_registry(project_root)?;
    let path = workflow_config_path(project_root);
    if let Some(legacy_path) = legacy_workflow_config_paths(project_root).iter().find(|candidate| candidate.exists()) {
        return Err(anyhow!(
            "workflow config v2 JSON is no longer supported at {} (found unsupported legacy file at {}). Remove the JSON config and define workflows in .animus/workflows.yaml or .animus/workflows/*.yaml",
            path.display(),
            legacy_path.display()
        ));
    }

    if path.exists() {
        return Err(anyhow!(
            "workflow config JSON is no longer supported at {}. Remove the JSON config and define workflows in .animus/workflows.yaml or .animus/workflows/*.yaml",
            path.display()
        ));
    }

    if !yaml_sources.is_empty() || registry.has_pack_overlays() || plugin_base.is_some() {
        // v0.5.9: bypass the disk cache when sources reference external
        // inputs not captured in the hash (env vars, system_prompt_file:
        // references, declared secrets). Those inputs change without
        // mutating the YAML source files, so a content+mtime hash alone
        // could otherwise serve stale compiled output.
        // Plugin-sourced config bypasses the YAML disk cache (it's keyed on YAML bytes/mtime).
        let cache_disabled_for_run =
            plugin_base.is_some() || sources_have_external_inputs(&yaml_sources, &registry);
        let cache_input = build_workflow_cache_input(project_root, &yaml_sources, &registry);
        let cache_hash = cache_input.hash();
        // Validate pack configuration *before* returning a cache hit so
        // changes in active-pack inputs that aren't part of the cache
        // hash still surface as errors, matching the cold-path
        // behavior exactly.
        //
        // TODO(codex-p2): cache hits currently skip
        // `load_pack_workflow_overlay` and `resolve_pack_workflow_assets`,
        // so a referenced pack asset removed after the cache was written
        // is masked until the YAML/manifest changes or the user passes
        // `--no-cache`. Either fold an asset-existence probe into
        // `WorkflowCacheInput` or run a thin asset-presence validator on
        // every cache hit.
        validate_active_pack_configuration(&registry)?;
        if !cache_disabled_for_run {
            if let Some(cached) = crate::cache::read_workflow_cache(project_root, &cache_hash) {
                return Ok(cached);
            }
        }

        let (mut config, mut path) = build_installed_pack_workflow_config_base(project_root)?;

        for entry in registry.entries_for_source(PackRegistrySource::Installed) {
            let Some(pack) = entry.loaded_manifest() else {
                continue;
            };
            if let Some(overlay) = load_pack_workflow_overlay(pack, &config)? {
                config = merge_yaml_into_config(config, overlay);
                path = entry.pack_root.clone().unwrap_or_else(machine_installed_packs_dir);
            }
        }

        // Explicit `skills:` declarations that don't resolve against the
        // project's skill sources warn (never error): a typo'd skill name
        // must not be a silent no-op. Cold-compile path only, matching the
        // unenforced-field warnings emitted inside the compiler.
        for warning in super::validation::missing_skill_reference_warnings_for_sources(project_root, &yaml_sources) {
            eprintln!("warning: {warning}");
        }
        // v0.6: the base overlay comes from the config_source plugin when one is
        // installed; otherwise from compiling the in-tree YAML sources.
        let base_overlay = if let Some((base, _version)) = &plugin_base {
            Some(base.clone())
        } else {
            super::compile_yaml_sources_with_base(&config, &yaml_sources)?
        };
        if let Some(yaml_config) = base_overlay {
            config = merge_yaml_into_config(config, yaml_config);
            if plugin_base.is_some() {
                path = project_root.to_path_buf();
            } else {
                let single_file = project_root.join(".animus").join("workflows.yaml");
                let workflows_dir = yaml_workflows_dir(project_root);
                path = if single_file.exists() { single_file } else { workflows_dir };
            }
        }

        for entry in registry.entries_for_source(PackRegistrySource::ProjectOverride) {
            let Some(pack) = entry.loaded_manifest() else {
                continue;
            };
            if let Some(overlay) = load_pack_workflow_overlay(pack, &config)? {
                config = merge_yaml_into_config(config, overlay);
                path = entry.pack_root.clone().unwrap_or_else(|| project_root.join(".animus").join("plugins"));
            }
        }

        validate_workflow_config_with_project_root(&config, Some(project_root))?;

        let source = if plugin_base.is_some() {
            // Plugin-sourced (Postgres/API/...) — treated as a non-builtin source.
            WorkflowConfigSource::Yaml
        } else if yaml_sources.is_empty() && !registry.has_external_packs() {
            WorkflowConfigSource::Builtin
        } else {
            WorkflowConfigSource::Yaml
        };

        let loaded = LoadedWorkflowConfig {
            metadata: WorkflowConfigMetadata {
                schema: config.schema.clone(),
                version: config.version,
                hash: workflow_config_hash(&config),
                source,
            },
            config,
            path,
        };
        if !cache_disabled_for_run {
            let _ = crate::cache::write_workflow_cache(project_root, &cache_hash, &loaded);
        }
        return Ok(loaded);
    }

    Err(anyhow!("workflow config is missing. Define workflows in .animus/workflows.yaml or .animus/workflows/*.yaml"))
}

pub fn load_workflow_config_or_default(project_root: &Path) -> LoadedWorkflowConfig {
    match load_workflow_config_with_metadata(project_root) {
        Ok(loaded) => loaded,
        Err(_) => {
            let config = runtime_workflow_config_base();
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

fn build_installed_pack_workflow_config_base(project_root: &Path) -> Result<(WorkflowConfig, PathBuf)> {
    let config = runtime_workflow_config_base();
    let path = workflow_config_path(project_root);

    Ok((config, path))
}

fn runtime_workflow_config_base() -> WorkflowConfig {
    let mut config = builtin_workflow_config_base();
    config.default_workflow_ref.clear();
    config.workflows.clear();
    config
}

pub fn workflow_config_hash(config: &WorkflowConfig) -> String {
    let bytes = serde_json::to_vec(config).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn build_workflow_cache_input(
    project_root: &Path,
    yaml_sources: &[(PathBuf, String)],
    registry: &ResolvedPackRegistry,
) -> WorkflowCacheInput {
    let mut input = WorkflowCacheInput::new();

    for (path, content) in yaml_sources {
        input.push(path.clone(), content.as_bytes().to_vec());
    }

    for entry in &registry.entries {
        if let Some(loaded) = entry.loaded_manifest() {
            append_pack_inputs(&mut input, loaded);
        }
    }

    let _ = project_root;
    input
}

fn sources_have_external_inputs(yaml_sources: &[(PathBuf, String)], registry: &ResolvedPackRegistry) -> bool {
    if yaml_sources.iter().any(|(_, content)| content_references_external_inputs(content)) {
        return true;
    }
    for entry in &registry.entries {
        let Some(loaded) = entry.loaded_manifest() else {
            continue;
        };
        if let Some(workflows) = loaded.manifest.workflows.as_ref() {
            let root = loaded.pack_root.join(&workflows.root);
            if root.is_dir() {
                if let Ok(read_dir) = std::fs::read_dir(&root) {
                    for entry in read_dir.flatten() {
                        let path = entry.path();
                        if path.extension().map(|ext| ext == "yaml" || ext == "yml").unwrap_or(false) {
                            if let Ok(s) = std::fs::read_to_string(&path) {
                                if content_references_external_inputs(&s) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(overlay_path) = loaded.manifest.runtime.workflow_overlay.as_deref() {
            let path = loaded.pack_root.join(overlay_path);
            if let Ok(s) = std::fs::read_to_string(&path) {
                if content_references_external_inputs(&s) {
                    return true;
                }
            }
        }
    }
    false
}

fn content_references_external_inputs(content: &str) -> bool {
    // ${VAR}, ${VAR:-default}, ${VAR:?msg}: env-var interpolation reads
    // process env at compile time.
    // ${secret.NAME}: secret-store reads, identical hazard.
    // system_prompt_file: arbitrary path read whose contents are inlined
    // into the compiled config but not hashed by `WorkflowCacheInput`.
    // The match is a substring scan so any YAML spelling (quoted,
    // whitespace-padded, or block-style) reliably trips the bypass —
    // overshooting into a comment costs us a cache miss, not
    // correctness. Stale-prompt bugs are silent and cost more.
    content.contains("${") || content.contains("system_prompt_file")
}

fn append_pack_inputs(input: &mut WorkflowCacheInput, pack: &LoadedPackManifest) {
    let manifest_path = pack.manifest_path.clone();
    let manifest_bytes = std::fs::read(&manifest_path).unwrap_or_default();
    input.push(manifest_path, manifest_bytes);

    if let Some(workflows) = pack.manifest.workflows.as_ref() {
        let root = pack.pack_root.join(&workflows.root);
        if root.is_dir() {
            if let Ok(read_dir) = std::fs::read_dir(&root) {
                let mut paths: Vec<PathBuf> = read_dir
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|ext| ext == "yaml" || ext == "yml").unwrap_or(false))
                    .collect();
                paths.sort();
                for path in paths {
                    let bytes = std::fs::read(&path).unwrap_or_default();
                    input.push(path, bytes);
                }
            }
        }
    }

    if let Some(overlay_path) = pack.manifest.runtime.workflow_overlay.as_deref() {
        let path = pack.pack_root.join(overlay_path);
        let bytes = std::fs::read(&path).unwrap_or_default();
        input.push(path, bytes);
    }

    // v0.5.9: also key on the agent_overlay (runtime profiles + system
    // prompt files referenced from it) so editing the overlay or the
    // prompt file it points at invalidates the cache. Asset
    // existence/canonicalization checks still happen on the cold path,
    // but this covers the common edit-pack-prompt loop.
    if let Some(overlay_path) = pack.manifest.runtime.agent_overlay.as_deref() {
        let path = pack.pack_root.join(overlay_path);
        let bytes = std::fs::read(&path).unwrap_or_default();
        input.push(path, bytes);
    }
}
