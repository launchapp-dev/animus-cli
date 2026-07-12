use std::path::{Path, PathBuf};

use animus_actor::Actor;
use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

use super::types::*;
use super::validation::validate_workflow_config_with_project_root;
use super::yaml_scaffold::ensure_workflow_yaml_scaffold;
use crate::{
    load_pack_workflow_overlay, resolve_pack_registry, validate_active_pack_configuration, PackRegistrySource,
};
use animus_config_protocol::builtins::builtin_workflow_config_base;
use animus_config_protocol::parse::merge_yaml_into_config;
use animus_config_protocol::yaml_types::GENERATED_WORKFLOW_OVERLAY_FILE_NAME;

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
    let registry = resolve_pack_registry(project_root)?;
    // v0.6: the base workflow config is sourced exclusively by an installed
    // `config_source` plugin. There is nothing to compile unless a config_source
    // plugin is installed OR the pack registry contributes overlays.
    if !super::config_source_client::config_source_installed(project_root) && !registry.has_pack_overlays() {
        return Ok(());
    }

    load_workflow_config_with_metadata(project_root, None).map(|_| ())
}

pub fn load_workflow_config(project_root: &Path, actor: Option<&Actor>) -> Result<WorkflowConfig> {
    Ok(load_workflow_config_with_metadata(project_root, actor)?.config)
}

pub fn load_workflow_config_with_metadata(project_root: &Path, actor: Option<&Actor>) -> Result<LoadedWorkflowConfig> {
    // v0.6: the project's base `WorkflowConfig` is sourced EXCLUSIVELY by an
    // installed `config_source` plugin. The kernel no longer parses
    // `.animus/*.yaml` in its runtime load path — the YAML parser lives on as a
    // library consumed by the `launchapp-dev/animus-config-yaml` plugin (which
    // path-deps this crate and calls `compile_yaml_workflow_files`). The kernel
    // stays the compiler: it merges pack overlays onto the plugin-sourced base
    // and validates.
    let plugin_base = super::config_source_client::resolve_plugin_base(project_root, actor)?;
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

    let Some((plugin_config, cache_version)) = plugin_base else {
        return Err(anyhow!(
            "no config_source plugin installed: the kernel sources its workflow/agent config exclusively from an \
             installed `config_source` plugin. Install one with \
             `animus plugin install launchapp-dev/animus-config-yaml` (reads .animus/workflows.yaml + \
             .animus/workflows/*.yaml) or run `animus plugin install-defaults`."
        ));
    };

    // CacheToken short-circuit: if the config_source plugin's CacheToken
    // version AND the resolved pack registry both match what we last compiled
    // for this root, the inputs are unchanged, so skip the whole pack-overlay
    // merge + validate and reuse the cached compiled config. A `config/write`
    // (or a changed token / pack set) invalidates the entry, so a real config
    // OR pack change always recompiles. Correctness over cleverness: any
    // mismatch ALWAYS does the full compile below. The pack registry is folded
    // into the key because the source token reflects ONLY the source, not which
    // packs are installed/active.
    //
    // SECURITY: the actor identity is folded into the token AND the cache map key
    // (see `config_source_client::cached_compiled`) so a per-user `config_source`
    // result is never served across actors. Belt-and-suspenders: the map key
    // already partitions by actor; folding it into the token keeps a stale
    // cross-actor compile from ever matching even if a future refactor relaxes
    // the map key.
    let compile_token = format!(
        "{cache_version}\u{1f}{}\u{1f}{}",
        pack_registry_fingerprint(&registry),
        super::config_source_client::actor_cache_key(actor),
    );
    if let Some(cached) = super::config_source_client::cached_compiled(project_root, actor, &compile_token) {
        return Ok(cached);
    }

    // The config_source plugin owns its own caching via the CacheToken it
    // returns; the kernel's YAML-bytes/mtime disk cache never applied to the
    // plugin path. Validate pack configuration here so active-pack input
    // changes surface exactly as they did on the cold compile path.
    validate_active_pack_configuration(&registry)?;

    let (mut config, _base_path) = build_installed_pack_workflow_config_base(project_root)?;

    for entry in registry.entries_for_source(PackRegistrySource::Installed) {
        let Some(pack) = entry.loaded_manifest() else {
            continue;
        };
        if let Some(overlay) = load_pack_workflow_overlay(pack, &config)? {
            config = merge_yaml_into_config(config, overlay);
        }
    }

    // v0.6: the base overlay is the canonical model produced by the
    // config_source plugin. It is the authoritative source, so the loaded
    // config's `path` points at the project root the plugin sourced from.
    config = merge_yaml_into_config(config, plugin_config);
    let mut path = project_root.to_path_buf();

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

    let loaded = LoadedWorkflowConfig {
        metadata: WorkflowConfigMetadata {
            schema: config.schema.clone(),
            version: config.version,
            hash: workflow_config_hash(&config),
            // Plugin-sourced base (config_source plugin: Postgres / API / YAML-on-disk).
            // `path` is the project root the plugin sourced from, not a real YAML file.
            source: WorkflowConfigSource::Plugin,
        },
        config,
        path,
    };

    // Cache the compiled result under the source-token + pack-registry key so
    // an unchanged source AND pack set on the next load short-circuits the
    // merge + validate above.
    super::config_source_client::store_compiled(project_root, actor, compile_token, loaded.clone());
    Ok(loaded)
}

/// Stable fingerprint of the resolved pack registry — `(pack_id, version,
/// source, manifest_path, manifest mtime)` per entry, hashed. Folded into the
/// compiled-config cache key so installing/removing/re-pinning a pack
/// invalidates the short-circuit even when the config_source token is
/// unchanged. The manifest mtime also catches an in-place edit of an active
/// pack (common for project-override packs) that keeps the same id/version/path.
///
/// TODO(codex-p2): the mtime covers the manifest file but not the pack's
/// workflow-overlay YAML files that `load_pack_workflow_overlay` reads. An
/// in-place edit of ONLY an overlay file (without touching the manifest) can
/// still be served stale until some other token changes. A full fix hashes the
/// overlay bytes, which reintroduces the per-load IO the resident cache was
/// built to avoid; deferred as a P2 — pack overlays are rarely hot-edited in
/// the daemon's lifetime, and `animus pack` operations rewrite the manifest.
fn pack_registry_fingerprint(registry: &crate::ResolvedPackRegistry) -> String {
    let mut hasher = Sha256::new();
    for entry in &registry.entries {
        hasher.update(entry.pack_id.as_bytes());
        hasher.update([0x1f]);
        hasher.update(entry.version.as_bytes());
        hasher.update([0x1f]);
        hasher.update(format!("{:?}", entry.source).as_bytes());
        hasher.update([0x1f]);
        if let Some(path) = &entry.manifest_path {
            hasher.update(path.as_os_str().as_encoded_bytes());
            hasher.update([0x1f]);
            hasher.update(file_mtime_nanos(path).to_le_bytes());
        }
        hasher.update([0x1e]);
    }
    format!("{:x}", hasher.finalize())
}

/// Best-effort mtime (nanos since epoch) for a path; `0` when unavailable. Used
/// only as a cache-invalidation signal, so an unreadable mtime conservatively
/// collides to `0` (a later successful read differs and invalidates).
fn file_mtime_nanos(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Bootstrap / dev compile path: source the project's base `WorkflowConfig`
/// from the in-tree YAML library parser (`compile_yaml_workflow_files`) instead
/// of requiring an installed `config_source` plugin, then run the SAME kernel
/// compiler (pack-overlay merge + validate). This is what `animus workflow
/// config compile` and `animus init` use so they work with no plugin installed.
///
/// The DAEMON RUNTIME load path (`load_workflow_config_with_metadata`) still
/// requires a `config_source` plugin — only this dev/bootstrap/pack path uses
/// the library parser directly. Returns `Ok(None)` when the project has no
/// `.animus` YAML sources (and no pack overlays would apply on the empty base).
pub fn compile_workflow_config_from_library(project_root: &Path) -> Result<Option<LoadedWorkflowConfig>> {
    let Some(library_base) = super::compile_yaml_workflow_files(project_root)? else {
        return Ok(None);
    };
    let loaded = compile_workflow_config_onto_base(project_root, library_base)?;
    Ok(Some(loaded))
}

/// Shared compiler tail: merge installed + project-override pack overlays onto
/// `base_overlay` (layered on the runtime base), then validate. Used by both the
/// plugin-sourced runtime path and the library-sourced bootstrap path.
pub(crate) fn compile_workflow_config_onto_base(
    project_root: &Path,
    base_overlay: WorkflowConfig,
) -> Result<LoadedWorkflowConfig> {
    let registry = resolve_pack_registry(project_root)?;
    validate_active_pack_configuration(&registry)?;

    let (mut config, _base_path) = build_installed_pack_workflow_config_base(project_root)?;

    for entry in registry.entries_for_source(PackRegistrySource::Installed) {
        let Some(pack) = entry.loaded_manifest() else {
            continue;
        };
        if let Some(overlay) = load_pack_workflow_overlay(pack, &config)? {
            config = merge_yaml_into_config(config, overlay);
        }
    }

    config = merge_yaml_into_config(config, base_overlay);
    let mut path = project_root.to_path_buf();

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

    Ok(LoadedWorkflowConfig {
        metadata: WorkflowConfigMetadata {
            schema: config.schema.clone(),
            version: config.version,
            hash: workflow_config_hash(&config),
            source: WorkflowConfigSource::Yaml,
        },
        config,
        path,
    })
}

/// Three-way outcome of attempting to source the base workflow config,
/// distinguishing "no config source is configured" (benign — callers may fall
/// back to other config) from "the config source failed to load" (transient /
/// erroring — the config could NOT be determined).
///
/// This is the non-swallowing counterpart to
/// [`load_workflow_config_or_default`]: the `_or_default` path degrades ANY
/// load failure into an empty builtin base, which is correct for system
/// reconcilers but catastrophic for credential/OAuth resolution — a transient
/// `config_source` plugin failure (DB overload under bulk `mcp call`) would be
/// misreported as "server not defined" with an empty `mcp_servers`. Credential
/// resolvers route through [`try_load_workflow_config`] instead so a source
/// outage propagates as [`WorkflowConfigAvailability::SourceUnavailable`]
/// rather than silently emptying the config.
pub enum WorkflowConfigAvailability {
    /// The config was sourced and compiled successfully. Boxed because
    /// `LoadedWorkflowConfig` is large relative to the other (small) variants.
    Loaded(Box<LoadedWorkflowConfig>),
    /// No `config_source` plugin is installed (and no injected test base): the
    /// project simply has no workflow-config source. Benign — callers fall back
    /// to other config (e.g. project `.animus/config.json`).
    NoSource,
    /// A `config_source` plugin IS present but base acquisition or compilation
    /// failed (spawn / handshake / RPC / DB overload, or a validation error).
    /// The config could not be determined; callers must NOT treat this as an
    /// empty/absent config.
    SourceUnavailable(anyhow::Error),
}

/// Non-swallowing load: classify base acquisition into
/// [`WorkflowConfigAvailability`] so credential/OAuth resolvers can tell a
/// genuinely-absent config source apart from a transient source failure.
///
/// Unlike [`load_workflow_config_with_metadata`] (which collapses "no plugin
/// installed" into an error) and [`load_workflow_config_or_default`] (which
/// collapses every error into an empty base), this inspects the
/// `config_source` base result directly:
/// - `Ok(None)` (no plugin / no injected base) => [`WorkflowConfigAvailability::NoSource`].
/// - `Ok(Some(base))` => compile it (pack overlays + validate); a compile
///   failure surfaces as [`WorkflowConfigAvailability::SourceUnavailable`].
/// - `Err(_)` (plugin present but its load failed) =>
///   [`WorkflowConfigAvailability::SourceUnavailable`].
pub fn try_load_workflow_config(project_root: &Path, actor: Option<&Actor>) -> WorkflowConfigAvailability {
    let (base, cache_version) = match super::config_source_client::resolve_plugin_base(project_root, actor) {
        Ok(None) => return WorkflowConfigAvailability::NoSource,
        Ok(Some(plugin_base)) => plugin_base,
        Err(err) => return WorkflowConfigAvailability::SourceUnavailable(err),
    };

    // Reuse the SAME compiled-config cache as `load_workflow_config_with_metadata`
    // (keyed by source token + pack fingerprint + actor) so an unchanged source
    // and pack set short-circuits the pack-overlay merge + validate below.
    // `resolve_server_url` routes OAuth resolution through this path once per
    // `mcp call`, so honoring the cache keeps a bulk burst from recompiling pack
    // overlays on every call.
    let registry = match resolve_pack_registry(project_root) {
        Ok(registry) => registry,
        Err(err) => return WorkflowConfigAvailability::SourceUnavailable(err),
    };
    let compile_token = format!(
        "{cache_version}\u{1f}{}\u{1f}{}",
        pack_registry_fingerprint(&registry),
        super::config_source_client::actor_cache_key(actor),
    );
    if let Some(cached) = super::config_source_client::cached_compiled(project_root, actor, &compile_token) {
        return WorkflowConfigAvailability::Loaded(Box::new(cached));
    }

    match compile_workflow_config_onto_base(project_root, base) {
        Ok(mut loaded) => {
            // The base is plugin-sourced (identical to
            // `load_workflow_config_with_metadata`), so label it `Plugin` rather
            // than `Yaml`; this also keeps the shared cache entry consistent
            // across both loaders.
            loaded.metadata.source = WorkflowConfigSource::Plugin;
            super::config_source_client::store_compiled(project_root, actor, compile_token, loaded.clone());
            WorkflowConfigAvailability::Loaded(Box::new(loaded))
        }
        Err(err) => WorkflowConfigAvailability::SourceUnavailable(err),
    }
}

pub fn load_workflow_config_or_default(project_root: &Path) -> LoadedWorkflowConfig {
    // The default/fallback path is system-initiated (daemon reconcilers,
    // schedulers, CLI inspection) with no authenticated actor → the global
    // (`actor = None`) config partition.
    load_workflow_config_or_default_for_actor(project_root, None)
}

/// Actor-scoped variant of [`load_workflow_config_or_default`]: resolves the
/// config from the `actor`'s partition (a per-user `config_source` returns that
/// user's global∪private∪shared set), falling back to the builtin base on error.
/// `actor = None` is identical to [`load_workflow_config_or_default`] (global).
pub fn load_workflow_config_or_default_for_actor(project_root: &Path, actor: Option<&Actor>) -> LoadedWorkflowConfig {
    match load_workflow_config_with_metadata(project_root, actor) {
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
    animus_config_protocol::overlay::write_workflow_yaml_overlay(
        project_root,
        GENERATED_WORKFLOW_OVERLAY_FILE_NAME,
        config,
    )
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
