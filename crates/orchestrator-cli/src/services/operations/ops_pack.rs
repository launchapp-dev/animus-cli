use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use orchestrator_config::{
    add_marketplace_registry, check_pack_runtime_requirements, clone_marketplace_pack, load_marketplace_state,
    load_pack_inventory, load_pack_manifest, load_pack_selection_state, machine_installed_packs_dir,
    project_pack_overrides_dir, register_pack_in_registry, remove_marketplace_registry, save_pack_selection_state,
    search_marketplace_packs, sync_all_registries, sync_registry, LoadedPackManifest, PackDependency, PackInventory,
    PackInventoryEntry, PackPluginRequirement, PackRegistrySource, PackSelectionEntry, PackSelectionSource,
};
use serde::Serialize;

use crate::{
    conflict_error, invalid_input_error, not_found_error, print_ok, print_value, render_table, PackCommand,
    PackInspectArgs, PackPinArgs, PackPublishArgs, PackRegistryCommand, PackUninstallArgs,
};

/// Render `pack list` results as a human-readable table matching the
/// `animus plugin list` style.
fn print_pack_list_table(rows: &[PackListRow]) {
    if rows.is_empty() {
        println!("No packs installed. Browse available packs with: animus pack search");
        return;
    }
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.pack_id.clone(),
                r.version.clone(),
                r.source.clone(),
                if r.active { "yes".to_string() } else { "no".to_string() },
                r.title.clone().unwrap_or_else(|| "--".to_string()),
            ]
        })
        .collect();
    render_table(&["NAME", "VERSION", "SOURCE", "ACTIVE", "TITLE"], &table_rows);
}

#[derive(Debug, Serialize)]
struct PackListRow {
    pack_id: String,
    version: String,
    source: String,
    active: bool,
    title: Option<String>,
    description: Option<String>,
    pack_root: Option<String>,
    selection: Option<PackSelectionSummary>,
}

#[derive(Debug, Serialize)]
struct PackSelectionSummary {
    enabled: bool,
    version: Option<String>,
    source: Option<String>,
}

#[derive(Debug, Serialize)]
struct PackInspectOutput {
    pack_id: String,
    version: String,
    source: String,
    active: Option<bool>,
    pack_root: Option<String>,
    manifest_path: Option<String>,
    selection: Option<PackSelectionSummary>,
    runtime_report: orchestrator_config::PackRuntimeReport,
    /// Declared pack dependencies annotated with installed/missing status.
    dependencies: Vec<PackDependencyStatus>,
    /// Declared `[[requires_plugins]]` entries annotated with installed/missing status.
    required_plugins: Vec<RequiredPluginStatus>,
    manifest: orchestrator_config::PackManifest,
}

#[derive(Debug, Serialize)]
struct PackDependencyStatus {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_version: Option<String>,
}

#[derive(Debug, Serialize)]
struct RequiredPluginStatus {
    repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    install_command: Option<String>,
}

fn dependency_statuses(
    manifest: &orchestrator_config::PackManifest,
    inventory: &PackInventory,
) -> Vec<PackDependencyStatus> {
    manifest
        .dependencies
        .iter()
        .map(|dependency| {
            let installed_version =
                satisfying_installed_version(inventory, &dependency.id, dependency.version.as_deref());
            PackDependencyStatus {
                id: dependency.id.clone(),
                version: dependency.version.clone(),
                optional: dependency.optional,
                reason: dependency.reason.clone(),
                installed: installed_version.is_some(),
                installed_version,
            }
        })
        .collect()
}

fn required_plugin_statuses(
    manifest: &orchestrator_config::PackManifest,
    installed_plugins: &BTreeMap<String, super::ops_plugin::InstalledPlugin>,
) -> Vec<RequiredPluginStatus> {
    manifest
        .requires_plugins
        .iter()
        .map(|requirement| {
            let installed = plugin_requirement_installed(installed_plugins, &requirement.repo);
            RequiredPluginStatus {
                repo: requirement.repo.clone(),
                tag: requirement.tag.clone(),
                role: requirement.role.clone(),
                optional: requirement.optional,
                reason: requirement.reason.clone(),
                installed,
                install_command: if installed { None } else { Some(plugin_install_command(requirement)) },
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub(crate) struct PackInstallOutput {
    pub(crate) pack_id: String,
    pub(crate) version: String,
    pub(crate) installed_root: String,
    pub(crate) activated: bool,
}

/// Install a pack from a local source directory into
/// `~/.animus/packs/<id>/<version>/`, optionally activating it for the
/// project. This is the shared install path used by `animus pack install`
/// and the `animus init` recommended-pack installer.
pub(crate) fn install_pack_from_source_root(
    project_root: &Path,
    source_root: &Path,
    activate: bool,
    force: bool,
) -> Result<PackInstallOutput> {
    let loaded = load_pack_manifest(source_root)?;
    let target_root = machine_installed_packs_dir().join(&loaded.manifest.id).join(&loaded.manifest.version);

    if target_root.exists() {
        if !force {
            return Err(anyhow!(
                "pack '{}' version '{}' already exists at {} (use --force to overwrite)",
                loaded.manifest.id,
                loaded.manifest.version,
                target_root.display()
            ));
        }
        fs::remove_dir_all(&target_root).with_context(|| format!("failed to remove {}", target_root.display()))?;
    }

    copy_dir_recursive(source_root, &target_root)?;

    if activate {
        let mut state = load_pack_selection_state(project_root)?;
        state.upsert(PackSelectionEntry {
            pack_id: loaded.manifest.id.clone(),
            version: Some(format!("={}", loaded.manifest.version)),
            source: Some(PackSelectionSource::Installed),
            enabled: true,
        })?;
        save_pack_selection_state(project_root, &state)?;
    }

    Ok(PackInstallOutput {
        pack_id: loaded.manifest.id,
        version: loaded.manifest.version,
        installed_root: target_root.display().to_string(),
        activated: activate,
    })
}

/// Maximum pack-dependency recursion depth (deps of deps). The direct
/// dependencies of the requested pack sit at depth 1.
const MAX_PACK_DEP_DEPTH: usize = 5;

/// How the pack being installed was resolved. Dependencies are resolved
/// through the same path their parent came from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PackInstallOrigin {
    /// Installed via `--name` from a marketplace registry. `explicit` is true
    /// when the registry was selected with `--registry`: dependency resolution
    /// then stays inside that registry instead of searching all of them.
    Marketplace { registry_id: String, explicit: bool },
    /// Installed via `--path` from a local directory.
    LocalPath,
}

#[derive(Debug, Serialize)]
struct PackDependencyResult {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    optional: bool,
    requested_by: String,
    depth: usize,
    /// "installed" | "already_installed" | "would_install" | "optional_suggestion"
    /// | "skipped_depth_cap" | "failed"
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct RequiredPluginResult {
    repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    optional: bool,
    /// "installed" | "installed_now" | "missing" | "optional_suggestion" | "failed"
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    install_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct PackInstallReport {
    pack_id: String,
    version: String,
    dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_root: Option<String>,
    activated: bool,
    dependencies: Vec<PackDependencyResult>,
    required_plugins: Vec<RequiredPluginResult>,
}

/// Return the first installed pack version that satisfies `version_req`
/// (`None` matches any version of the pack id).
fn satisfying_installed_version(inventory: &PackInventory, pack_id: &str, version_req: Option<&str>) -> Option<String> {
    // An unparseable requirement was already rejected by manifest
    // validation; treat it as "match any" rather than failing here.
    let requirement = version_req
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|raw| semver::VersionReq::parse(raw).ok());
    inventory
        .entries
        .iter()
        .filter(|entry| entry.pack_id.eq_ignore_ascii_case(pack_id))
        .filter(|entry| match (&requirement, semver::Version::parse(&entry.version)) {
            (Some(requirement), Ok(version)) => requirement.matches(&version),
            (Some(_), Err(_)) => false,
            (None, _) => true,
        })
        .map(|entry| entry.version.clone())
        .next()
}

fn find_inventory_manifest<'a>(
    inventory: &'a PackInventory,
    pack_id: &str,
    version: &str,
) -> Option<&'a LoadedPackManifest> {
    inventory
        .entries
        .iter()
        .find(|entry| entry.pack_id.eq_ignore_ascii_case(pack_id) && entry.version == version)
        .and_then(|entry| entry.loaded_manifest())
}

/// Manual recovery command for a pack dependency that could not be installed
/// automatically. Recommended packs get the pinned git-clone command; other
/// packs get the marketplace install command.
fn manual_dependency_install_command(dep_id: &str) -> String {
    if let Some(pin) = super::ops_init::recommended_packs().iter().find(|pin| pin.id.eq_ignore_ascii_case(dep_id)) {
        return super::ops_init::manual_pack_install_command(pin);
    }
    format!("animus pack install --name {dep_id} --activate")
}

/// Resolve a local source directory for a dependency pack through the same
/// resolution path its parent came from:
///
/// 1. `ANIMUS_INIT_PACK_SOURCE_DIR/<dep-id>` (offline override, always wins)
/// 2. the marketplace registries (when the parent was installed via `--name`)
/// 3. the pinned GitHub release from `default-install.json` when the dep is a
///    recommended pack (shallow clone of the pinned tag)
fn resolve_dependency_source(dep_id: &str, origin: &PackInstallOrigin, scratch: &Path) -> Result<PathBuf> {
    if let Ok(source_dir) = std::env::var("ANIMUS_INIT_PACK_SOURCE_DIR") {
        let candidate = PathBuf::from(source_dir).join(dep_id);
        if !candidate.is_dir() {
            return Err(anyhow!("ANIMUS_INIT_PACK_SOURCE_DIR is set but {} does not exist", candidate.display()));
        }
        return Ok(candidate);
    }

    if let PackInstallOrigin::Marketplace { registry_id, explicit } = origin {
        // Prefer the registry the parent came from; an explicitly selected
        // `--registry` is a trust boundary, so dependency resolution never
        // leaves it for other registries.
        let results = search_marketplace_packs(Some(dep_id), None, Some(registry_id))?;
        if let Some(hit) = results.iter().find(|result| result.name.eq_ignore_ascii_case(dep_id)) {
            return clone_marketplace_pack(&hit.registry_id, &hit.name);
        }
        if !explicit {
            let results = search_marketplace_packs(Some(dep_id), None, None)?;
            if let Some(hit) = results.iter().find(|result| result.name.eq_ignore_ascii_case(dep_id)) {
                return clone_marketplace_pack(&hit.registry_id, &hit.name);
            }
        }
    }

    if let Some(pin) = super::ops_init::recommended_packs().iter().find(|pin| pin.id.eq_ignore_ascii_case(dep_id)) {
        return super::ops_init::resolve_recommended_pack_source(pin, scratch);
    }

    Err(anyhow!(
        "no source found for dependency pack '{dep_id}' (not in any marketplace registry or the recommended set)"
    ))
}

/// Resolve and (unless `dry_run`) install the non-optional dependency closure
/// of `parent`. Per-dependency failures never abort the run: they are recorded
/// with a manual recovery command and resolution continues. Recursion is
/// bounded by [`MAX_PACK_DEP_DEPTH`] with cycle detection by pack id.
fn resolve_dependency_closure(
    project_root: &Path,
    parent: &LoadedPackManifest,
    origin: PackInstallOrigin,
    activate: bool,
    dry_run: bool,
) -> Vec<PackDependencyResult> {
    let inventory = load_pack_inventory(project_root).unwrap_or_default();
    let scratch = tempfile::tempdir().ok();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    visited.insert(parent.manifest.id.trim().to_ascii_lowercase());
    // Versions resolved (installed or confirmed) this run, by lowercase pack
    // id. Used to re-validate duplicate edges that reach an already-handled
    // pack id with a different version requirement.
    let mut resolved_versions: BTreeMap<String, String> = BTreeMap::new();
    resolved_versions.insert(parent.manifest.id.trim().to_ascii_lowercase(), parent.manifest.version.clone());

    let mut queue: Vec<(PackDependency, String, usize)> = parent
        .manifest
        .dependencies
        .iter()
        .cloned()
        .map(|dependency| (dependency, parent.manifest.id.clone(), 1usize))
        .collect();
    let mut rows = Vec::new();
    // Optional suggestions already emitted, by lowercase pack id (kept apart
    // from `visited` so optional edges never block a later required edge).
    let mut suggested: BTreeSet<String> = BTreeSet::new();

    while !queue.is_empty() {
        let (dependency, requested_by, depth) = queue.remove(0);
        let normalized = dependency.id.trim().to_ascii_lowercase();

        // Optional edges are suggestions only: they must NOT mark the pack id
        // as visited, otherwise a later required edge to the same pack would
        // be silently skipped instead of installed.
        if dependency.optional {
            if visited.contains(&normalized) || !suggested.insert(normalized) {
                // Already resolved as a required dependency, or already suggested.
                continue;
            }
            rows.push(PackDependencyResult {
                id: dependency.id.clone(),
                version: dependency.version.clone(),
                optional: true,
                requested_by,
                depth,
                status: "optional_suggestion".to_string(),
                installed_version: None,
                detail: Some(format!(
                    "optional dependency; install manually with: {}",
                    manual_dependency_install_command(&dependency.id)
                )),
            });
            continue;
        }

        if visited.contains(&normalized) {
            // Cycle / duplicate: the pack id was already handled this run.
            // Still validate this edge's version requirement so a stricter
            // constraint reached through another branch is not silently lost.
            {
                if let Some(raw_req) = dependency.version.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                    // Only the version resolved this run is the one that ends
                    // up active — another installed version satisfying the
                    // requirement does not make this edge usable. Fall back to
                    // the installed inventory only when the earlier edge
                    // resolved nothing (e.g. it failed and was already reported).
                    let satisfied = match resolved_versions.get(&normalized) {
                        Some(resolved) => {
                            match (semver::VersionReq::parse(raw_req), semver::Version::parse(resolved)) {
                                (Ok(requirement), Ok(version)) => requirement.matches(&version),
                                // Unparseable requirements were rejected by
                                // manifest validation; never fail on them here.
                                _ => true,
                            }
                        }
                        None => satisfying_installed_version(&inventory, &dependency.id, Some(raw_req)).is_some(),
                    };
                    if !satisfied {
                        rows.push(PackDependencyResult {
                            id: dependency.id.clone(),
                            version: dependency.version.clone(),
                            optional: false,
                            requested_by,
                            depth,
                            status: "failed".to_string(),
                            installed_version: None,
                            detail: Some(format!(
                                "conflicting requirement: '{}' was already resolved this run at a version that does not satisfy '{raw_req}'; install manually: {}",
                                dependency.id,
                                manual_dependency_install_command(&dependency.id)
                            )),
                        });
                    }
                }
            }
            continue;
        }
        visited.insert(normalized.clone());

        if depth > MAX_PACK_DEP_DEPTH {
            rows.push(PackDependencyResult {
                id: dependency.id.clone(),
                version: dependency.version.clone(),
                optional: false,
                requested_by,
                depth,
                status: "skipped_depth_cap".to_string(),
                installed_version: None,
                detail: Some(format!(
                    "dependency depth exceeds the cap of {MAX_PACK_DEP_DEPTH}; install manually with: {}",
                    manual_dependency_install_command(&dependency.id)
                )),
            });
            continue;
        }

        if let Some(version) = satisfying_installed_version(&inventory, &dependency.id, dependency.version.as_deref()) {
            if let Some(manifest) = find_inventory_manifest(&inventory, &dependency.id, &version) {
                for transitive in &manifest.manifest.dependencies {
                    queue.push((transitive.clone(), dependency.id.clone(), depth + 1));
                }
            }
            resolved_versions.insert(normalized, version.clone());
            // Mirror the fresh-install path: when the parent install activates,
            // a previously disabled or re-pinned dependency must become usable
            // again instead of staying parked on a stale project selection.
            let activation_error = if activate && !dry_run {
                activate_dependency_selection(project_root, &inventory, &dependency.id, &version).err()
            } else {
                None
            };
            match activation_error {
                None => rows.push(PackDependencyResult {
                    id: dependency.id.clone(),
                    version: dependency.version.clone(),
                    optional: false,
                    requested_by,
                    depth,
                    status: "already_installed".to_string(),
                    installed_version: Some(version),
                    detail: None,
                }),
                Some(err) => rows.push(PackDependencyResult {
                    id: dependency.id.clone(),
                    version: dependency.version.clone(),
                    optional: false,
                    requested_by,
                    depth,
                    status: "failed".to_string(),
                    installed_version: Some(version),
                    detail: Some(format!("pack is installed but could not be activated: {err:#}")),
                }),
            }
            continue;
        }

        if dry_run {
            // Dry-run stays offline: only the offline source override is
            // probed for transitive dependencies; remote sources are not
            // cloned, so their transitive deps are reported as unknown.
            let mut detail = None;
            let mut status = "would_install";
            if let Ok(source_dir) = std::env::var("ANIMUS_INIT_PACK_SOURCE_DIR") {
                match load_pack_manifest(&PathBuf::from(source_dir).join(&dependency.id)) {
                    // Mirror the real install: a missing or invalid offline
                    // source, a source declaring a different pack id, or a
                    // version that misses the requirement would fail rather
                    // than install.
                    Err(err) => {
                        status = "failed";
                        detail = Some(format!(
                            "{err:#}; install manually: {}",
                            manual_dependency_install_command(&dependency.id)
                        ));
                    }
                    Ok(loaded) => {
                        if !loaded.manifest.id.eq_ignore_ascii_case(&dependency.id) {
                            rows.push(PackDependencyResult {
                                id: dependency.id.clone(),
                                version: dependency.version.clone(),
                                optional: false,
                                requested_by,
                                depth,
                                status: "failed".to_string(),
                                installed_version: None,
                                detail: Some(format!(
                                    "dependency source declares id '{}' but '{}' was requested; install manually: {}",
                                    loaded.manifest.id,
                                    dependency.id,
                                    manual_dependency_install_command(&dependency.id)
                                )),
                            });
                            continue;
                        }
                        let version_mismatch =
                            dependency.version.as_deref().map(str::trim).filter(|v| !v.is_empty()).and_then(
                                |raw_req| match (
                                    semver::VersionReq::parse(raw_req),
                                    semver::Version::parse(&loaded.manifest.version),
                                ) {
                                    (Ok(requirement), Ok(version)) if !requirement.matches(&version) => {
                                        Some((raw_req.to_string(), loaded.manifest.version.clone()))
                                    }
                                    _ => None,
                                },
                            );
                        if let Some((raw_req, source_version)) = version_mismatch {
                            status = "failed";
                            detail = Some(format!(
                            "dependency source provides version '{source_version}' which does not satisfy '{raw_req}'; install manually: {}",
                            manual_dependency_install_command(&dependency.id)
                        ));
                        } else {
                            for transitive in &loaded.manifest.dependencies {
                                queue.push((transitive.clone(), dependency.id.clone(), depth + 1));
                            }
                            resolved_versions.insert(normalized, loaded.manifest.version.clone());
                        }
                    }
                }
            } else {
                detail = Some("transitive dependencies unknown until the pack source is fetched".to_string());
            }
            rows.push(PackDependencyResult {
                id: dependency.id.clone(),
                version: dependency.version.clone(),
                optional: false,
                requested_by,
                depth,
                status: status.to_string(),
                installed_version: None,
                detail,
            });
            continue;
        }

        let install = scratch
            .as_ref()
            .ok_or_else(|| anyhow!("failed to create scratch directory for dependency downloads"))
            .and_then(|scratch| resolve_dependency_source(&dependency.id, &origin, scratch.path()))
            .and_then(|source_root| {
                let loaded = load_pack_manifest(&source_root)?;
                if !loaded.manifest.id.eq_ignore_ascii_case(&dependency.id) {
                    return Err(anyhow!(
                        "dependency source declares id '{}' but '{}' was requested",
                        loaded.manifest.id,
                        dependency.id
                    ));
                }
                if let Some(raw_req) = dependency.version.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                    if let (Ok(requirement), Ok(version)) =
                        (semver::VersionReq::parse(raw_req), semver::Version::parse(&loaded.manifest.version))
                    {
                        if !requirement.matches(&version) {
                            return Err(anyhow!(
                                "dependency source provides version '{}' which does not satisfy '{raw_req}'",
                                loaded.manifest.version
                            ));
                        }
                    }
                }
                let output = install_pack_from_source_root(project_root, &source_root, activate, false)?;
                Ok((output, loaded))
            });
        match install {
            Ok((output, loaded)) => {
                for transitive in &loaded.manifest.dependencies {
                    queue.push((transitive.clone(), dependency.id.clone(), depth + 1));
                }
                resolved_versions.insert(normalized, output.version.clone());
                rows.push(PackDependencyResult {
                    id: dependency.id.clone(),
                    version: dependency.version.clone(),
                    optional: false,
                    requested_by,
                    depth,
                    status: "installed".to_string(),
                    installed_version: Some(output.version),
                    detail: None,
                });
            }
            Err(err) => rows.push(PackDependencyResult {
                id: dependency.id.clone(),
                version: dependency.version.clone(),
                optional: false,
                requested_by,
                depth,
                status: "failed".to_string(),
                installed_version: None,
                detail: Some(format!(
                    "{err:#}; install manually: {}",
                    manual_dependency_install_command(&dependency.id)
                )),
            }),
        }
    }

    rows
}

/// Re-enable the project selection for an already-installed dependency
/// version, preserving the inventory source it was discovered under.
fn activate_dependency_selection(
    project_root: &Path,
    inventory: &PackInventory,
    pack_id: &str,
    version: &str,
) -> Result<()> {
    let source = inventory
        .entries
        .iter()
        .find(|entry| entry.pack_id.eq_ignore_ascii_case(pack_id) && entry.version == version)
        .map(|entry| entry.source);
    let mut state = load_pack_selection_state(project_root)?;
    state.upsert(PackSelectionEntry {
        pack_id: pack_id.to_string(),
        version: Some(format!("={version}")),
        source: selection_source_for(source).or(Some(PackSelectionSource::Installed)),
        enabled: true,
    })?;
    save_pack_selection_state(project_root, &state)
}

fn plugin_install_command(requirement: &PackPluginRequirement) -> String {
    match requirement.tag.as_deref().map(str::trim).filter(|tag| !tag.is_empty()) {
        Some(tag) => format!("animus plugin install {}@{}", requirement.repo, tag),
        None => format!("animus plugin install {}", requirement.repo),
    }
}

/// True when the installed-plugin registry contains the required repo —
/// matched by origin slug (`owner/repo@tag`) or by plugin name equal to the
/// repo basename.
fn plugin_requirement_installed(installed: &BTreeMap<String, super::ops_plugin::InstalledPlugin>, repo: &str) -> bool {
    let repo = repo.trim();
    let basename = repo.rsplit('/').next().unwrap_or(repo);
    installed.values().any(|entry| {
        let origin_slug = entry
            .origin
            .as_deref()
            .and_then(|origin| origin.split('@').next())
            .map(str::trim)
            .filter(|slug| !slug.is_empty());
        match origin_slug {
            // An entry with a recorded origin must match the exact repo —
            // a same-named plugin from another owner does not satisfy it.
            Some(slug) => {
                slug.eq_ignore_ascii_case(repo) || (!slug.contains('/') && entry.name.eq_ignore_ascii_case(basename))
            }
            // Local-path installs carry no origin; fall back to the name.
            None => entry.name.eq_ignore_ascii_case(basename),
        }
    })
}

fn prompt_yes_no_default_yes(message: &str) -> bool {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return false;
    }
    print!("{message} [Y/n] ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    let trimmed = input.trim().to_ascii_lowercase();
    trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
}

/// Check (and optionally install) the `[[requires_plugins]]` declarations of
/// the requested pack plus every dependency pack touched this run. Missing
/// required plugins are NEVER silent: they either install (`--install-plugins`
/// or an interactive yes) or surface the exact install command.
async fn ensure_required_plugins(
    requirements: Vec<PackPluginRequirement>,
    install_plugins: bool,
    interactive: bool,
    dry_run: bool,
    project_root: &Path,
) -> Vec<RequiredPluginResult> {
    if requirements.is_empty() {
        return Vec::new();
    }

    let installed = match super::ops_plugin::read_installed_index() {
        Ok(installed) => installed,
        Err(err) => {
            eprintln!("warning: failed to read the installed-plugin registry ({err:#}); treating all required plugins as missing");
            BTreeMap::new()
        }
    };

    let mut rows = Vec::new();
    let mut missing_required: Vec<PackPluginRequirement> = Vec::new();
    for requirement in requirements {
        if plugin_requirement_installed(&installed, &requirement.repo) {
            rows.push(RequiredPluginResult {
                repo: requirement.repo,
                tag: requirement.tag,
                role: requirement.role,
                optional: requirement.optional,
                status: "installed".to_string(),
                install_command: None,
                detail: None,
            });
        } else if requirement.optional {
            rows.push(RequiredPluginResult {
                install_command: Some(plugin_install_command(&requirement)),
                repo: requirement.repo,
                tag: requirement.tag,
                role: requirement.role,
                optional: true,
                status: "optional_suggestion".to_string(),
                detail: requirement.reason,
            });
        } else {
            missing_required.push(requirement);
        }
    }

    if missing_required.is_empty() {
        return rows;
    }

    let do_install = !dry_run
        && (install_plugins || {
            let listing = missing_required.iter().map(|r| r.repo.as_str()).collect::<Vec<_>>().join(", ");
            interactive
                && prompt_yes_no_default_yes(&format!(
                    "Install {} missing required plugin(s) ({listing})?",
                    missing_required.len()
                ))
        });

    for requirement in missing_required {
        if !do_install {
            rows.push(RequiredPluginResult {
                install_command: Some(plugin_install_command(&requirement)),
                repo: requirement.repo,
                tag: requirement.tag,
                role: requirement.role,
                optional: false,
                status: "missing".to_string(),
                detail: requirement.reason,
            });
            continue;
        }

        // Offline/test seam: `ANIMUS_PACK_PLUGIN_SOURCE_DIR/<repo-basename>`
        // installs the plugin binary from a local directory instead of the
        // GitHub release path. Mirrors `ANIMUS_INIT_PACK_SOURCE_DIR`.
        let request = match std::env::var("ANIMUS_PACK_PLUGIN_SOURCE_DIR") {
            Ok(source_dir) if !source_dir.trim().is_empty() => {
                let basename = requirement.repo.rsplit('/').next().unwrap_or(&requirement.repo);
                super::ops_plugin::PluginInstallRequest {
                    path: Some(PathBuf::from(source_dir).join(basename).to_string_lossy().to_string()),
                    skip_signature: true,
                    yes: true,
                    project_root: Some(project_root.to_string_lossy().to_string()),
                    ..Default::default()
                }
            }
            _ => super::ops_plugin::PluginInstallRequest {
                source: Some(requirement.repo.clone()),
                tag: requirement.tag.clone(),
                yes: true,
                project_root: Some(project_root.to_string_lossy().to_string()),
                ..Default::default()
            },
        };
        match super::ops_plugin::run_plugin_install(request).await {
            Ok(_) => rows.push(RequiredPluginResult {
                repo: requirement.repo,
                tag: requirement.tag,
                role: requirement.role,
                optional: false,
                status: "installed_now".to_string(),
                install_command: None,
                detail: None,
            }),
            Err(err) => rows.push(RequiredPluginResult {
                install_command: Some(plugin_install_command(&requirement)),
                repo: requirement.repo,
                tag: requirement.tag,
                role: requirement.role,
                optional: false,
                status: "failed".to_string(),
                detail: Some(format!("{err:#}")),
            }),
        }
    }

    rows
}

/// Aggregate the plugin requirements of the parent pack plus every dependency
/// manifest that was installed or confirmed this run, deduped by repo.
fn collect_plugin_requirements(
    project_root: &Path,
    parent: &LoadedPackManifest,
    dependency_rows: &[PackDependencyResult],
) -> Vec<PackPluginRequirement> {
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut requirements: Vec<PackPluginRequirement> = Vec::new();
    let mut push_all = |list: &[PackPluginRequirement]| {
        for requirement in list {
            let key = requirement.repo.trim().to_ascii_lowercase();
            match index.get(&key) {
                Some(&existing) => {
                    // A required declaration overrides an earlier optional
                    // one for the same plugin repo.
                    if requirements[existing].optional && !requirement.optional {
                        requirements[existing] = requirement.clone();
                    }
                }
                None => {
                    index.insert(key, requirements.len());
                    requirements.push(requirement.clone());
                }
            }
        }
    };
    push_all(&parent.manifest.requires_plugins);

    let inventory = load_pack_inventory(project_root).unwrap_or_default();
    for row in dependency_rows {
        match row.status.as_str() {
            "installed" | "already_installed" => {
                if let Some(version) = row.installed_version.as_deref() {
                    if let Some(manifest) = find_inventory_manifest(&inventory, &row.id, version) {
                        push_all(&manifest.manifest.requires_plugins);
                    }
                }
            }
            // Dry-run rows: pick up plugin requirements from the offline
            // source override when available so the dry-run report covers
            // the resolvable closure (remote sources stay unfetched).
            "would_install" => {
                if let Ok(source_dir) = std::env::var("ANIMUS_INIT_PACK_SOURCE_DIR") {
                    if let Ok(loaded) = load_pack_manifest(&PathBuf::from(source_dir).join(&row.id)) {
                        push_all(&loaded.manifest.requires_plugins);
                    }
                }
            }
            _ => {}
        }
    }
    requirements
}

fn print_dependency_rows(rows: &[PackDependencyResult], dry_run: bool) {
    for row in rows {
        let requirement = row.version.as_deref().map(|req| format!(" {req}")).unwrap_or_default();
        match row.status.as_str() {
            "installed" => println!(
                "dependency {}{requirement} installed (version {}, via {})",
                row.id,
                row.installed_version.as_deref().unwrap_or("?"),
                row.requested_by
            ),
            "already_installed" => println!(
                "dependency {}{requirement} already installed (version {})",
                row.id,
                row.installed_version.as_deref().unwrap_or("?")
            ),
            "would_install" => {
                let verb = if dry_run { "would install" } else { "missing" };
                println!("dependency {}{requirement} {verb} (via {})", row.id, row.requested_by);
            }
            "optional_suggestion" => {
                println!("optional dependency {}{requirement}: {}", row.id, row.detail.as_deref().unwrap_or(""))
            }
            _ => eprintln!(
                "warning: dependency {}{requirement} {}: {}",
                row.id,
                row.status,
                row.detail.as_deref().unwrap_or("unknown error")
            ),
        }
    }
}

fn print_required_plugin_rows(rows: &[RequiredPluginResult]) {
    for row in rows {
        let role = row.role.as_deref().map(|role| format!(" [{role}]")).unwrap_or_default();
        match row.status.as_str() {
            "installed" => println!("required plugin {}{role} already installed", row.repo),
            "installed_now" => println!("required plugin {}{role} installed", row.repo),
            "optional_suggestion" => println!(
                "optional plugin {}{role} not installed; install with: {}",
                row.repo,
                row.install_command.as_deref().unwrap_or("")
            ),
            "missing" => eprintln!(
                "warning: required plugin {}{role} is not installed; install with: {}",
                row.repo,
                row.install_command.as_deref().unwrap_or("")
            ),
            _ => eprintln!(
                "warning: required plugin {}{role} failed to install ({}); install manually: {}",
                row.repo,
                row.detail.as_deref().unwrap_or("unknown error"),
                row.install_command.as_deref().unwrap_or("")
            ),
        }
    }
}

fn parse_source(raw: Option<&str>) -> Result<Option<PackRegistrySource>> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let parsed = match raw.to_ascii_lowercase().as_str() {
        "installed" => PackRegistrySource::Installed,
        "project_override" | "project-override" | "project" => PackRegistrySource::ProjectOverride,
        _ => {
            return Err(invalid_input_error(format!(
                "unsupported pack source '{}'; expected installed or project_override",
                raw
            )))
        }
    };
    Ok(Some(parsed))
}

fn selection_source_for(source: Option<PackRegistrySource>) -> Option<PackSelectionSource> {
    match source {
        Some(PackRegistrySource::Bundled) => Some(PackSelectionSource::Bundled),
        Some(PackRegistrySource::Installed) => Some(PackSelectionSource::Installed),
        Some(PackRegistrySource::ProjectOverride) => Some(PackSelectionSource::ProjectOverride),
        None => None,
    }
}

fn selection_summary(entry: &PackInventoryEntry) -> Option<PackSelectionSummary> {
    entry.selection.as_ref().map(|selection| PackSelectionSummary {
        enabled: selection.enabled,
        version: selection.version.clone(),
        source: selection.source.map(|source| source.as_registry_source().as_str().to_string()),
    })
}

fn inventory_row(entry: &PackInventoryEntry) -> PackListRow {
    let manifest = entry.loaded_manifest().map(|pack| &pack.manifest);
    PackListRow {
        pack_id: entry.pack_id.clone(),
        version: entry.version.clone(),
        source: entry.source.as_str().to_string(),
        active: entry.active,
        title: manifest.map(|manifest| manifest.title.clone()),
        description: manifest
            .map(|manifest| manifest.description.clone())
            .filter(|description| !description.trim().is_empty()),
        pack_root: entry.pack_root.as_ref().map(|path| path.display().to_string()),
        selection: selection_summary(entry),
    }
}

fn inspect_inventory_entry(entry: &PackInventoryEntry, inventory: &PackInventory) -> Result<PackInspectOutput> {
    let pack = entry
        .loaded_manifest()
        .ok_or_else(|| anyhow!("pack '{}' does not expose an inspectable manifest", entry.pack_id))?;
    let installed_plugins = super::ops_plugin::read_installed_index().unwrap_or_default();
    Ok(PackInspectOutput {
        pack_id: entry.pack_id.clone(),
        version: entry.version.clone(),
        source: entry.source.as_str().to_string(),
        active: Some(entry.active),
        pack_root: entry.pack_root.as_ref().map(|path| path.display().to_string()),
        manifest_path: entry.manifest_path.as_ref().map(|path| path.display().to_string()),
        selection: selection_summary(entry),
        runtime_report: check_pack_runtime_requirements(pack)?,
        dependencies: dependency_statuses(&pack.manifest, inventory),
        required_plugins: required_plugin_statuses(&pack.manifest, &installed_plugins),
        manifest: pack.manifest.clone(),
    })
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)
                .with_context(|| format!("failed to copy {} to {}", src_path.display(), dst_path.display()))?;
        }
    }
    Ok(())
}

fn resolve_local_pack_root(raw_path: &str) -> Result<PathBuf> {
    let root = PathBuf::from(raw_path.trim());
    if root.as_os_str().is_empty() {
        return Err(invalid_input_error("pack path must not be empty"));
    }
    root.canonicalize().with_context(|| format!("failed to resolve pack path {}", root.display()))
}

fn inspect_pack(project_root: &Path, args: PackInspectArgs) -> Result<PackInspectOutput> {
    if let Some(path) = args.path.as_deref() {
        let root = resolve_local_pack_root(path)?;
        let pack = load_pack_manifest(&root)?;
        let inventory = load_pack_inventory(project_root).unwrap_or_default();
        let installed_plugins = super::ops_plugin::read_installed_index().unwrap_or_default();
        return Ok(PackInspectOutput {
            pack_id: pack.manifest.id.clone(),
            version: pack.manifest.version.clone(),
            source: "local".to_string(),
            active: None,
            pack_root: Some(pack.pack_root.display().to_string()),
            manifest_path: Some(pack.manifest_path.display().to_string()),
            selection: None,
            runtime_report: check_pack_runtime_requirements(&pack)?,
            dependencies: dependency_statuses(&pack.manifest, &inventory),
            required_plugins: required_plugin_statuses(&pack.manifest, &installed_plugins),
            manifest: pack.manifest.clone(),
        });
    }

    let pack_id = args
        .pack_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_input_error("either --path or --pack-id is required"))?;
    let source = parse_source(args.source.as_deref())?;
    let inventory = load_pack_inventory(project_root)?;
    let entry = inventory
        .resolve(pack_id, args.version.as_deref(), source)
        .or_else(|| inventory.resolve(pack_id, None, source))
        .ok_or_else(|| anyhow!("pack '{}' not found", pack_id))?;
    inspect_inventory_entry(entry, &inventory)
}

pub(crate) async fn handle_pack(command: PackCommand, project_root: &str, json: bool) -> Result<()> {
    let project_root = Path::new(project_root);
    match command {
        PackCommand::List(args) => {
            let source = parse_source(args.source.as_deref())?;
            let inventory = load_pack_inventory(project_root)?;
            let rows = inventory
                .entries
                .iter()
                .filter(|entry| source.map(|candidate| entry.source == candidate).unwrap_or(true))
                .filter(|entry| !args.active_only || entry.active)
                .map(inventory_row)
                .collect::<Vec<_>>();
            if !json {
                print_pack_list_table(&rows);
                return Ok(());
            }
            print_value(rows, json)
        }
        PackCommand::Info(args) => print_value(inspect_pack(project_root, args)?, json),
        PackCommand::Search(args) => {
            let results = search_marketplace_packs(args.query(), args.category.as_deref(), args.registry.as_deref())?;
            if results.is_empty() && !json {
                print_ok("no packs found matching the query", false);
                return Ok(());
            }
            print_value(results, json)
        }
        PackCommand::Registry { command } => handle_registry(command, json),
        PackCommand::Install(args) => {
            let (source_root, origin) = if let Some(name) = args.name.as_deref() {
                let registry_id = args.registry.as_deref().unwrap_or_else(|| {
                    eprintln!("no --registry specified, searching all registries for '{}'", name);
                    ""
                });
                let registry_id = if registry_id.is_empty() {
                    let results = search_marketplace_packs(Some(name), None, None)?;
                    let hit = results
                        .iter()
                        .find(|r| r.name.eq_ignore_ascii_case(name))
                        .ok_or_else(|| anyhow!("pack '{}' not found in any registry", name))?;
                    hit.registry_id.clone()
                } else {
                    registry_id.to_string()
                };
                (
                    clone_marketplace_pack(&registry_id, name)?,
                    PackInstallOrigin::Marketplace {
                        explicit: args.registry.as_deref().map(str::trim).is_some_and(|value| !value.is_empty()),
                        registry_id,
                    },
                )
            } else if let Some(path) = args.path.as_deref() {
                (resolve_local_pack_root(path)?, PackInstallOrigin::LocalPath)
            } else {
                return Err(invalid_input_error("either --path or --name is required for pack install"));
            };

            let loaded = load_pack_manifest(&source_root)?;
            let install = if args.dry_run {
                None
            } else {
                Some(install_pack_from_source_root(project_root, &source_root, args.activate, args.force)?)
            };

            let dependencies = if args.no_deps {
                Vec::new()
            } else {
                resolve_dependency_closure(project_root, &loaded, origin, args.activate, args.dry_run)
            };

            let requirements = collect_plugin_requirements(project_root, &loaded, &dependencies);
            let interactive = !json
                && !args.install_plugins
                && std::io::IsTerminal::is_terminal(&std::io::stdin())
                && std::io::IsTerminal::is_terminal(&std::io::stdout());
            let required_plugins =
                ensure_required_plugins(requirements, args.install_plugins, interactive, args.dry_run, project_root)
                    .await;

            let report = PackInstallReport {
                pack_id: loaded.manifest.id.clone(),
                version: loaded.manifest.version.clone(),
                dry_run: args.dry_run,
                installed_root: install.as_ref().map(|output| output.installed_root.clone()),
                activated: install.as_ref().map(|output| output.activated).unwrap_or(false),
                dependencies,
                required_plugins,
            };
            if json {
                return print_value(report, true);
            }
            print_dependency_rows(&report.dependencies, report.dry_run);
            print_required_plugin_rows(&report.required_plugins);
            if report.dry_run {
                print_ok(&format!("dry-run: pack {} {} not installed", report.pack_id, report.version), false);
            } else {
                print_ok(&format!("installed pack {} {}", report.pack_id, report.version), false);
            }
            Ok(())
        }
        PackCommand::Pin(args) => handle_pin(project_root, args, json),
        PackCommand::Uninstall(args) => handle_uninstall(project_root, args, json),
        PackCommand::Publish(args) => handle_publish(args, json),
    }
}

#[derive(Debug, Serialize)]
struct PackUninstallOutput {
    pack_id: String,
    dry_run: bool,
    removed_versions: Vec<String>,
    removed_paths: Vec<String>,
    selection_removed: bool,
    warnings: Vec<String>,
}

fn child_directories_sorted(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = fs::read_dir(root)
        .with_context(|| format!("failed to read directory {}", root.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    Ok(dirs)
}

fn project_workflow_yaml_files(project_root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let single_file = project_root.join(".animus").join("workflows.yaml");
    if single_file.is_file() {
        files.push(single_file);
    }
    let workflows_dir = project_root.join(".animus").join("workflows");
    if let Ok(entries) = fs::read_dir(&workflows_dir) {
        let mut dir_files = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && path.extension().map(|ext| ext == "yaml" || ext == "yml").unwrap_or(false))
            .collect::<Vec<_>>();
        dir_files.sort();
        files.extend(dir_files);
    }
    files
}

fn pack_export_needles(version_roots: &[PathBuf]) -> Vec<String> {
    let mut needles = Vec::new();
    for root in version_roots {
        let Ok(pack) = load_pack_manifest(root) else {
            continue;
        };
        if let Some(workflows) = pack.manifest.workflows.as_ref() {
            for export in &workflows.exports {
                let export = export.trim().to_ascii_lowercase();
                if !export.is_empty() && !needles.contains(&export) {
                    needles.push(export);
                }
            }
        }
    }
    needles
}

fn pack_reference_needles(pack_id: &str, removed_roots: &[PathBuf], remaining_roots: &[PathBuf]) -> Vec<String> {
    if remaining_roots.is_empty() {
        let mut needles = vec![format!("{}/", pack_id.to_ascii_lowercase())];
        for export in pack_export_needles(removed_roots) {
            if !needles.contains(&export) {
                needles.push(export);
            }
        }
        return needles;
    }

    // Remaining versions keep serving the pack-id-prefixed refs, so only
    // exports that disappear with the removed versions can break workflows.
    let remaining_exports = pack_export_needles(remaining_roots);
    pack_export_needles(removed_roots).into_iter().filter(|export| !remaining_exports.contains(export)).collect()
}

fn find_pack_references_in_project_workflows(project_root: &Path, needles: &[String]) -> Vec<String> {
    if needles.is_empty() {
        return Vec::new();
    }
    let mut referencing_files = Vec::new();
    for file in project_workflow_yaml_files(project_root) {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        let content = content.to_ascii_lowercase();
        if needles.iter().any(|needle| content.contains(needle.as_str())) {
            referencing_files.push(file.display().to_string());
        }
    }
    referencing_files
}

fn selection_version_matches_any(version_req: Option<&str>, remaining_versions: &[String]) -> bool {
    if remaining_versions.is_empty() {
        return false;
    }
    let Some(requirement) = version_req.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    // An unparseable requirement is left for `pack pin` to fix rather than silently dropped.
    let Ok(requirement) = semver::VersionReq::parse(requirement) else {
        return true;
    };
    remaining_versions
        .iter()
        .any(|version| semver::Version::parse(version).map(|version| requirement.matches(&version)).unwrap_or(false))
}

fn project_override_pack_version(project_root: &Path, pack_id: &str) -> Option<String> {
    let overrides_dir = project_pack_overrides_dir(project_root);
    let dirs = child_directories_sorted(&overrides_dir).ok()?;
    dirs.iter().find_map(|dir| {
        load_pack_manifest(dir)
            .ok()
            .filter(|pack| pack.manifest.id.eq_ignore_ascii_case(pack_id))
            .map(|pack| pack.manifest.version)
    })
}

fn handle_uninstall(project_root: &Path, args: PackUninstallArgs, json: bool) -> Result<()> {
    let pack_id = args.pack_id.trim();
    if pack_id.is_empty() {
        return Err(invalid_input_error("pack id must not be empty"));
    }
    if pack_id.contains(['/', '\\']) || pack_id == "." || pack_id == ".." {
        return Err(invalid_input_error(format!("invalid pack id '{}'", pack_id)));
    }

    let pack_dir = machine_installed_packs_dir().join(pack_id);
    if !pack_dir.is_dir() {
        return Err(not_found_error(format!(
            "pack '{}' is not installed (no directory at {})",
            pack_id,
            pack_dir.display()
        )));
    }

    let all_version_dirs = child_directories_sorted(&pack_dir)?;
    let version = args.version.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let target_version_dirs = match version {
        Some(version) => {
            let dir = pack_dir.join(version);
            if version.contains(['/', '\\']) || version == "." || version == ".." || !dir.is_dir() {
                return Err(not_found_error(format!("pack '{}' version '{}' is not installed", pack_id, version)));
            }
            vec![dir]
        }
        None => all_version_dirs.clone(),
    };
    let removing_all = target_version_dirs.len() == all_version_dirs.len();
    let remaining_version_dirs =
        all_version_dirs.iter().filter(|dir| !target_version_dirs.contains(dir)).cloned().collect::<Vec<_>>();

    let mut warnings = Vec::new();
    let needles = pack_reference_needles(pack_id, &target_version_dirs, &remaining_version_dirs);
    let references = find_pack_references_in_project_workflows(project_root, &needles);
    if !references.is_empty() {
        if args.force {
            warnings.push(format!(
                "project workflow YAML still references this pack: {} (removed anyway via --force)",
                references.join(", ")
            ));
        } else {
            return Err(conflict_error(format!(
                "pack '{}' is still referenced by project workflow YAML: {}; re-run with --force to uninstall anyway",
                pack_id,
                references.join(", ")
            )));
        }
    }

    let removed_versions = target_version_dirs
        .iter()
        .filter_map(|dir| dir.file_name().and_then(|name| name.to_str()).map(str::to_string))
        .collect::<Vec<_>>();
    let mut removed_paths = Vec::new();
    if removing_all {
        removed_paths.push(pack_dir.display().to_string());
        if !args.dry_run {
            fs::remove_dir_all(&pack_dir).with_context(|| format!("failed to remove {}", pack_dir.display()))?;
        }
    } else {
        for dir in &target_version_dirs {
            removed_paths.push(dir.display().to_string());
            if !args.dry_run {
                fs::remove_dir_all(dir).with_context(|| format!("failed to remove {}", dir.display()))?;
            }
        }
    }

    let remaining_versions = remaining_version_dirs
        .iter()
        .filter_map(|dir| dir.file_name().and_then(|name| name.to_str()).map(str::to_string))
        .collect::<Vec<_>>();

    let mut selection_removed = false;
    let mut state = load_pack_selection_state(project_root)?;
    let selection_still_resolvable = state.selection_for(pack_id).map(|selection| match selection.source {
        Some(PackSelectionSource::ProjectOverride) => true,
        Some(PackSelectionSource::Bundled) => false,
        Some(PackSelectionSource::Installed) => {
            selection_version_matches_any(selection.version.as_deref(), &remaining_versions)
        }
        None => {
            project_override_pack_version(project_root, pack_id)
                .map(|version| selection_version_matches_any(selection.version.as_deref(), &[version]))
                .unwrap_or(false)
                || selection_version_matches_any(selection.version.as_deref(), &remaining_versions)
        }
    });
    match selection_still_resolvable {
        Some(true) => {
            if removing_all {
                warnings.push(format!(
                    "kept the project selection entry for '{}' because it can still resolve (project override or remaining installed versions)",
                    pack_id
                ));
            }
        }
        Some(false) => {
            selection_removed = true;
            if !args.dry_run {
                state.selections.retain(|selection| !selection.matches_pack_id(pack_id));
                save_pack_selection_state(project_root, &state)?;
            }
        }
        None => {}
    }

    let output = PackUninstallOutput {
        pack_id: pack_id.to_string(),
        dry_run: args.dry_run,
        removed_versions,
        removed_paths,
        selection_removed,
        warnings,
    };
    if json {
        return print_value(output, true);
    }

    for warning in &output.warnings {
        eprintln!("warning: {warning}");
    }
    let verb = if output.dry_run { "would remove" } else { "removed" };
    for path in &output.removed_paths {
        println!("{verb} {path}");
    }
    if output.selection_removed {
        println!("{verb} project selection entry for '{}'", output.pack_id);
    }
    let versions = if output.removed_versions.is_empty() {
        "no versioned directories".to_string()
    } else {
        format!("versions: {}", output.removed_versions.join(", "))
    };
    if output.dry_run {
        print_ok(&format!("dry-run: pack {} would be uninstalled ({})", output.pack_id, versions), false);
    } else {
        print_ok(&format!("uninstalled pack {} ({})", output.pack_id, versions), false);
    }
    Ok(())
}

fn handle_pin(project_root: &Path, args: PackPinArgs, json: bool) -> Result<()> {
    let pack_id = args.pack_id.trim();
    if pack_id.is_empty() {
        return Err(invalid_input_error("pack id must not be empty"));
    }

    let source = parse_source(args.source.as_deref())?;
    let inventory = load_pack_inventory(project_root)?;
    if !inventory.entries.iter().any(|entry| entry.pack_id.eq_ignore_ascii_case(pack_id)) {
        return Err(anyhow!("pack '{}' not found", pack_id));
    }

    let mut state = load_pack_selection_state(project_root)?;
    state.upsert(PackSelectionEntry {
        pack_id: pack_id.to_string(),
        version: args.version.clone(),
        source: selection_source_for(source),
        enabled: !args.disable,
    })?;
    save_pack_selection_state(project_root, &state)?;

    let selection = state.selection_for(pack_id).cloned().ok_or_else(|| anyhow!("selection missing after save"))?;
    if json {
        return print_value(
            serde_json::json!({
                "pack_id": selection.pack_id,
                "enabled": selection.enabled,
                "version": selection.version,
                "source": selection.source.map(|value| value.as_registry_source().as_str().to_string()),
            }),
            true,
        );
    }

    print_ok(if selection.enabled { "pack pin updated" } else { "pack disabled for project" }, false);
    Ok(())
}

fn handle_publish(args: PackPublishArgs, json: bool) -> Result<()> {
    let pack_dir = args.path.as_deref().map(Path::new).unwrap_or(Path::new("."));
    let manifest = load_pack_manifest(pack_dir)
        .map_err(|err| invalid_input_error(format!("failed to load pack.toml from '{}': {err}", pack_dir.display())))?;
    let name = manifest.manifest.id.trim().to_string();
    if name.is_empty() {
        return Err(invalid_input_error("pack manifest `id` must not be empty"));
    }
    let url = args.url.trim();
    if url.is_empty() {
        return Err(invalid_input_error("--url must not be empty"));
    }
    let description = manifest.manifest.description.trim().to_string();
    let description = if description.is_empty() { None } else { Some(description.as_str()) };
    let result = register_pack_in_registry(&args.registry, &name, url, description, args.category.as_deref())?;
    let clone_path_str = result.registry_clone_path.display().to_string();
    let action = if result.updated_existing { "updated" } else { "added" };
    if json {
        print_value(
            serde_json::json!({
                "pack": name,
                "url": result.url,
                "registry": result.registry_id,
                "action": action,
                "registry_clone_path": clone_path_str,
                "next_steps": [
                    format!("cd {clone_path_str}"),
                    format!("git add .claude-plugin/marketplace.json"),
                    format!("git commit -m 'publish {name}'"),
                    "git push".to_string(),
                ],
            }),
            true,
        )
    } else {
        eprintln!("{action} pack '{name}' in registry '{}' catalog", result.registry_id);
        eprintln!("registry clone: {clone_path_str}");
        eprintln!();
        eprintln!("To publish, commit and push the catalog update:");
        eprintln!("  cd {clone_path_str}");
        eprintln!("  git add .claude-plugin/marketplace.json");
        eprintln!("  git commit -m 'publish {name}'");
        eprintln!("  git push");
        Ok(())
    }
}

fn handle_registry(command: PackRegistryCommand, json: bool) -> Result<()> {
    match command {
        PackRegistryCommand::Add(args) => {
            add_marketplace_registry(&args.id, &args.url)?;
            if json {
                print_value(serde_json::json!({"id": args.id, "url": args.url, "status": "added"}), true)
            } else {
                print_ok(&format!("registry '{}' added and synced", args.id), false);
                Ok(())
            }
        }
        PackRegistryCommand::Remove(args) => {
            remove_marketplace_registry(&args.id)?;
            if json {
                print_value(serde_json::json!({"id": args.id, "status": "removed"}), true)
            } else {
                print_ok(&format!("registry '{}' removed", args.id), false);
                Ok(())
            }
        }
        PackRegistryCommand::List => {
            let state = load_marketplace_state()?;
            print_value(state.registries, json)
        }
        PackRegistryCommand::Sync(args) => {
            if let Some(id) = args.id {
                let state = load_marketplace_state()?;
                let entry = state
                    .registries
                    .iter()
                    .find(|r| r.id == id)
                    .ok_or_else(|| anyhow!("registry '{}' not found", id))?;
                sync_registry(&entry.id, &entry.url)?;
                if json {
                    print_value(serde_json::json!({"id": id, "status": "synced"}), true)
                } else {
                    print_ok(&format!("registry '{}' synced", id), false);
                    Ok(())
                }
            } else {
                let synced = sync_all_registries()?;
                if json {
                    print_value(serde_json::json!({"synced": synced}), true)
                } else {
                    print_ok(&format!("synced {} registries", synced.len()), false);
                    Ok(())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_minimal_pack(root: &Path, pack_id: &str, version: &str, export: &str) {
        fs::create_dir_all(root.join("workflows")).expect("create workflows dir");
        fs::write(
            root.join("pack.toml"),
            format!(
                r#"
schema = "animus.pack.v1"
id = "{pack_id}"
version = "{version}"
kind = "domain-pack"
title = "Pack {pack_id}"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
animus_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["animus.task"]
default_kind = "animus.task"

[workflows]
root = "workflows"
exports = ["{export}"]
"#
            ),
        )
        .expect("write manifest");
    }

    #[test]
    fn pack_reference_needles_include_pack_prefix_and_exports() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_minimal_pack(temp.path(), "animus.review", "0.1.0", "animus.review/standard");

        let needles = pack_reference_needles("animus.review", &[temp.path().to_path_buf()], &[]);
        assert!(needles.contains(&"animus.review/".to_string()));
        assert!(needles.contains(&"animus.review/standard".to_string()));
    }

    #[test]
    fn pack_reference_needles_only_flag_exports_lost_with_removed_versions() {
        let removed = tempfile::tempdir().expect("removed tempdir");
        let remaining = tempfile::tempdir().expect("remaining tempdir");
        write_minimal_pack(removed.path(), "animus.review", "0.1.0", "animus.review/old-only");
        write_minimal_pack(remaining.path(), "animus.review", "0.2.0", "animus.review/old-only");

        let needles =
            pack_reference_needles("animus.review", &[removed.path().to_path_buf()], &[remaining.path().to_path_buf()]);
        assert!(needles.is_empty(), "exports still served by remaining versions should not be flagged");

        let other_remaining = tempfile::tempdir().expect("other remaining tempdir");
        write_minimal_pack(other_remaining.path(), "animus.review", "0.2.0", "animus.review/new-only");
        let needles = pack_reference_needles(
            "animus.review",
            &[removed.path().to_path_buf()],
            &[other_remaining.path().to_path_buf()],
        );
        assert_eq!(needles, vec!["animus.review/old-only".to_string()]);
        assert!(
            !needles.contains(&"animus.review/".to_string()),
            "pack-id prefix should not be flagged while versions remain"
        );
    }

    #[test]
    fn find_pack_references_scans_project_workflow_yaml() {
        let project = tempfile::tempdir().expect("project tempdir");
        let pack = tempfile::tempdir().expect("pack tempdir");
        write_minimal_pack(pack.path(), "animus.review", "0.1.0", "animus.review/standard");

        fs::create_dir_all(project.path().join(".animus").join("workflows")).expect("create workflows dir");
        fs::write(
            project.path().join(".animus").join("workflows.yaml"),
            "workflows:\n  - id: pipeline\n    name: Pipeline\n    phases:\n      - workflow_ref: animus.review/standard\n",
        )
        .expect("write workflows.yaml");
        fs::write(
            project.path().join(".animus").join("workflows").join("other.yaml"),
            "workflows:\n  - id: other\n    name: Other\n    phases:\n      - requirements\n",
        )
        .expect("write other.yaml");

        let needles = pack_reference_needles("animus.review", &[pack.path().to_path_buf()], &[]);
        let references = find_pack_references_in_project_workflows(project.path(), &needles);
        assert_eq!(references.len(), 1, "only the referencing file should be reported");
        assert!(references[0].ends_with("workflows.yaml"));

        let unrelated_needles = pack_reference_needles("animus.unrelated", &[], &[]);
        let none = find_pack_references_in_project_workflows(project.path(), &unrelated_needles);
        assert!(none.is_empty(), "unrelated pack should have no references");
    }

    #[test]
    fn selection_version_matching_covers_pins_and_remaining_versions() {
        let none_remaining: Vec<String> = Vec::new();
        assert!(!selection_version_matches_any(None, &none_remaining), "no versions left can never resolve");

        let remaining = vec!["0.2.0".to_string()];
        assert!(selection_version_matches_any(None, &remaining), "unpinned selection resolves to any version");
        assert!(selection_version_matches_any(Some("=0.2.0"), &remaining));
        assert!(!selection_version_matches_any(Some("=0.1.0"), &remaining), "removed pinned version cannot resolve");
        assert!(selection_version_matches_any(Some("not-a-req"), &remaining), "unparseable pins are left in place");
    }

    #[test]
    fn uninstall_unknown_pack_is_an_error() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let project = tempfile::tempdir().expect("project tempdir");
        let args = PackUninstallArgs {
            pack_id: "animus.definitely-not-installed-xyz".to_string(),
            version: None,
            force: false,
            dry_run: false,
        };
        let error = handle_uninstall(project.path(), args, true).expect_err("uninstall should fail");
        assert!(error.to_string().contains("is not installed"));
    }

    #[test]
    fn uninstall_rejects_path_like_pack_ids() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let project = tempfile::tempdir().expect("project tempdir");
        let args = PackUninstallArgs { pack_id: "../escape".to_string(), version: None, force: false, dry_run: false };
        let error = handle_uninstall(project.path(), args, true).expect_err("uninstall should fail");
        assert!(error.to_string().contains("invalid pack id"));
    }

    /// Write a pack fixture whose manifest carries extra TOML sections
    /// (dependencies, requires_plugins, ...).
    fn write_pack_with(root: &Path, pack_id: &str, version: &str, extra_toml: &str) {
        fs::create_dir_all(root.join("workflows")).expect("create workflows dir");
        fs::write(
            root.join("pack.toml"),
            format!(
                r#"
schema = "animus.pack.v1"
id = "{pack_id}"
version = "{version}"
kind = "domain-pack"
title = "Pack {pack_id}"
description = "Fixture"

[ownership]
mode = "installed"

[workflows]
root = "workflows"
exports = ["{pack_id}/standard"]

{extra_toml}
"#
            ),
        )
        .expect("write manifest");
    }

    struct DepFixture {
        _home: tempfile::TempDir,
        home_guard: protocol::test_utils::EnvVarGuard,
        source_guard: protocol::test_utils::EnvVarGuard,
        source_dir: tempfile::TempDir,
        project: tempfile::TempDir,
    }

    impl DepFixture {
        fn new() -> Self {
            let home = tempfile::tempdir().expect("home tempdir");
            let home_guard =
                protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
            let source_dir = tempfile::tempdir().expect("source tempdir");
            let source_guard = protocol::test_utils::EnvVarGuard::set(
                "ANIMUS_INIT_PACK_SOURCE_DIR",
                Some(source_dir.path().to_string_lossy().as_ref()),
            );
            let project = tempfile::tempdir().expect("project tempdir");
            Self { _home: home, home_guard, source_guard, source_dir, project }
        }

        fn write_source_pack(&self, pack_id: &str, version: &str, extra_toml: &str) {
            write_pack_with(&self.source_dir.path().join(pack_id), pack_id, version, extra_toml);
        }

        fn load_source_pack(&self, pack_id: &str) -> LoadedPackManifest {
            load_pack_manifest(&self.source_dir.path().join(pack_id)).expect("load source pack")
        }

        fn installed_pack_dir(&self, pack_id: &str, version: &str) -> PathBuf {
            machine_installed_packs_dir().join(pack_id).join(version)
        }
    }

    impl Drop for DepFixture {
        fn drop(&mut self) {
            // Guards drop in field order anyway; this impl exists to silence
            // dead_code on the named guard fields.
            let _ = (&self.home_guard, &self.source_guard);
        }
    }

    fn row<'a>(rows: &'a [PackDependencyResult], id: &str) -> &'a PackDependencyResult {
        rows.iter().find(|row| row.id == id).unwrap_or_else(|| panic!("row for {id} missing"))
    }

    #[test]
    fn dependency_closure_installs_transitive_deps_and_skips_installed() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack(
            "animus.parent",
            "0.1.0",
            "[[dependencies]]\nid = \"animus.dep-a\"\nversion = \">=0.1.0\"\n",
        );
        fixture.write_source_pack("animus.dep-a", "0.1.0", "[[dependencies]]\nid = \"animus.dep-b\"\n");
        fixture.write_source_pack("animus.dep-b", "0.2.0", "");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, true, false);

        assert_eq!(row(&rows, "animus.dep-a").status, "installed");
        assert_eq!(row(&rows, "animus.dep-b").status, "installed");
        assert_eq!(row(&rows, "animus.dep-b").depth, 2);
        assert!(fixture.installed_pack_dir("animus.dep-a", "0.1.0").is_dir());
        assert!(fixture.installed_pack_dir("animus.dep-b", "0.2.0").is_dir());

        // Second resolution: everything is already installed and skipped.
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, true, false);
        assert_eq!(row(&rows, "animus.dep-a").status, "already_installed");
        assert_eq!(row(&rows, "animus.dep-b").status, "already_installed");
    }

    #[test]
    fn dependency_closure_rejects_source_version_that_misses_requirement() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack(
            "animus.parent",
            "0.1.0",
            "[[dependencies]]\nid = \"animus.dep-a\"\nversion = \">=2.0.0\"\n",
        );
        fixture.write_source_pack("animus.dep-a", "1.0.0", "");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, false);

        let dep = row(&rows, "animus.dep-a");
        assert_eq!(dep.status, "failed");
        assert!(dep.detail.as_deref().unwrap_or("").contains("does not satisfy"));
        assert!(!fixture.installed_pack_dir("animus.dep-a", "1.0.0").exists());
    }

    #[test]
    fn dependency_closure_terminates_on_cycles() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack("animus.cycle-a", "0.1.0", "[[dependencies]]\nid = \"animus.cycle-b\"\n");
        fixture.write_source_pack("animus.cycle-b", "0.1.0", "[[dependencies]]\nid = \"animus.cycle-a\"\n");

        let parent = fixture.load_source_pack("animus.cycle-a");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, false);

        assert_eq!(rows.len(), 1, "the back-edge to the parent must be skipped, got {rows:?}");
        assert_eq!(row(&rows, "animus.cycle-b").status, "installed");
    }

    #[test]
    fn dependency_closure_enforces_depth_cap() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack("animus.chain-0", "0.1.0", "[[dependencies]]\nid = \"animus.chain-1\"\n");
        for index in 1..=5 {
            fixture.write_source_pack(
                &format!("animus.chain-{index}"),
                "0.1.0",
                &format!("[[dependencies]]\nid = \"animus.chain-{}\"\n", index + 1),
            );
        }

        let parent = fixture.load_source_pack("animus.chain-0");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, false);

        // chain-1..chain-5 install at depths 1..5; chain-6 exceeds the cap.
        for index in 1..=5 {
            assert_eq!(row(&rows, &format!("animus.chain-{index}")).status, "installed");
        }
        let capped = row(&rows, "animus.chain-6");
        assert_eq!(capped.status, "skipped_depth_cap");
        assert_eq!(capped.depth, 6);
        assert!(capped.detail.as_deref().unwrap_or("").contains("animus pack install"));
    }

    #[test]
    fn dependency_closure_suggests_optional_deps_without_installing() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack(
            "animus.parent",
            "0.1.0",
            "[[dependencies]]\nid = \"animus.extra\"\noptional = true\n",
        );
        fixture.write_source_pack("animus.extra", "0.1.0", "");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, false);

        let dep = row(&rows, "animus.extra");
        assert_eq!(dep.status, "optional_suggestion");
        assert!(dep.detail.as_deref().unwrap_or("").contains("animus pack install"));
        assert!(!fixture.installed_pack_dir("animus.extra", "0.1.0").exists());
    }

    #[test]
    fn dependency_closure_continues_past_failures() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack(
            "animus.parent",
            "0.1.0",
            "[[dependencies]]\nid = \"animus.gone\"\n\n[[dependencies]]\nid = \"animus.dep-ok\"\n",
        );
        fixture.write_source_pack("animus.dep-ok", "0.1.0", "");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, false);

        let failed = row(&rows, "animus.gone");
        assert_eq!(failed.status, "failed");
        assert!(failed.detail.as_deref().unwrap_or("").contains("install manually"));
        assert_eq!(row(&rows, "animus.dep-ok").status, "installed");
        assert!(fixture.installed_pack_dir("animus.dep-ok", "0.1.0").is_dir());
    }

    #[test]
    fn dependency_closure_dry_run_lists_closure_without_installing() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack("animus.parent", "0.1.0", "[[dependencies]]\nid = \"animus.dep-a\"\n");
        fixture.write_source_pack("animus.dep-a", "0.1.0", "[[dependencies]]\nid = \"animus.dep-b\"\n");
        fixture.write_source_pack("animus.dep-b", "0.1.0", "");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, true);

        assert_eq!(row(&rows, "animus.dep-a").status, "would_install");
        assert_eq!(row(&rows, "animus.dep-b").status, "would_install");
        assert!(!fixture.installed_pack_dir("animus.dep-a", "0.1.0").exists());
        assert!(!fixture.installed_pack_dir("animus.dep-b", "0.1.0").exists());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the install await
    async fn pack_install_no_deps_skips_dependency_closure() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack("animus.parent", "0.1.0", "[[dependencies]]\nid = \"animus.dep-a\"\n");
        fixture.write_source_pack("animus.dep-a", "0.1.0", "");

        let args = crate::PackInstallArgs {
            path: Some(fixture.source_dir.path().join("animus.parent").to_string_lossy().to_string()),
            name: None,
            registry: None,
            force: false,
            activate: false,
            no_deps: true,
            install_plugins: false,
            dry_run: false,
        };
        handle_pack(crate::PackCommand::Install(args), fixture.project.path().to_string_lossy().as_ref(), true)
            .await
            .expect("install with --no-deps should succeed");

        assert!(fixture.installed_pack_dir("animus.parent", "0.1.0").is_dir());
        assert!(!fixture.installed_pack_dir("animus.dep-a", "0.1.0").exists(), "--no-deps must skip dependencies");
    }

    #[test]
    fn dependency_closure_optional_edge_does_not_block_required_edge() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack(
            "animus.parent",
            "0.1.0",
            concat!(
                "[[dependencies]]\nid = \"animus.shared\"\noptional = true\n\n",
                "[[dependencies]]\nid = \"animus.branch-b\"\n",
            ),
        );
        fixture.write_source_pack("animus.branch-b", "0.1.0", "[[dependencies]]\nid = \"animus.shared\"\n");
        fixture.write_source_pack("animus.shared", "0.1.0", "");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, false);

        let shared_rows: Vec<_> = rows.iter().filter(|row| row.id == "animus.shared").collect();
        assert!(shared_rows.iter().any(|row| row.status == "optional_suggestion"));
        assert!(
            shared_rows.iter().any(|row| row.status == "installed"),
            "the required edge must still install the pack, got {rows:?}"
        );
        assert!(fixture.installed_pack_dir("animus.shared", "0.1.0").is_dir());
    }

    #[test]
    fn dependency_closure_dry_run_flags_offline_version_mismatch() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack(
            "animus.parent",
            "0.1.0",
            "[[dependencies]]\nid = \"animus.dep-a\"\nversion = \">=2.0.0\"\n",
        );
        fixture.write_source_pack("animus.dep-a", "1.0.0", "");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, true);

        let dep = row(&rows, "animus.dep-a");
        assert_eq!(dep.status, "failed");
        assert!(dep.detail.as_deref().unwrap_or("").contains("does not satisfy"));
        assert!(!fixture.installed_pack_dir("animus.dep-a", "1.0.0").exists());
    }

    #[test]
    fn dependency_closure_reactivates_already_installed_deps_when_activating() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack("animus.parent", "0.1.0", "[[dependencies]]\nid = \"animus.dep-a\"\n");
        fixture.write_source_pack("animus.dep-a", "0.1.0", "");
        install_pack_from_source_root(
            fixture.project.path(),
            &fixture.source_dir.path().join("animus.dep-a"),
            false,
            false,
        )
        .expect("install dep");
        // Park the dependency on a disabled project selection.
        let mut state = load_pack_selection_state(fixture.project.path()).expect("load selection state");
        state
            .upsert(PackSelectionEntry {
                pack_id: "animus.dep-a".to_string(),
                version: Some("=0.1.0".to_string()),
                source: Some(PackSelectionSource::Installed),
                enabled: false,
            })
            .expect("disable selection");
        save_pack_selection_state(fixture.project.path(), &state).expect("save selection state");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, true, false);
        assert_eq!(row(&rows, "animus.dep-a").status, "already_installed");

        let state = load_pack_selection_state(fixture.project.path()).expect("reload selection state");
        let selection = state.selection_for("animus.dep-a").expect("selection entry");
        assert!(selection.enabled, "--activate must re-enable an already-installed dependency");
    }

    #[test]
    fn dependency_closure_dry_run_flags_offline_id_mismatch() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack("animus.parent", "0.1.0", "[[dependencies]]\nid = \"animus.dep-a\"\n");
        // The offline directory for animus.dep-a actually contains another pack.
        write_pack_with(&fixture.source_dir.path().join("animus.dep-a"), "animus.other", "0.1.0", "");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, true);

        let dep = row(&rows, "animus.dep-a");
        assert_eq!(dep.status, "failed");
        assert!(dep.detail.as_deref().unwrap_or("").contains("declares id"));
    }

    #[test]
    fn dependency_closure_flags_conflicting_requirements_across_branches() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack(
            "animus.parent",
            "0.1.0",
            "[[dependencies]]\nid = \"animus.branch-a\"\n\n[[dependencies]]\nid = \"animus.branch-b\"\n",
        );
        fixture.write_source_pack("animus.branch-a", "0.1.0", "[[dependencies]]\nid = \"animus.shared\"\n");
        fixture.write_source_pack(
            "animus.branch-b",
            "0.1.0",
            "[[dependencies]]\nid = \"animus.shared\"\nversion = \">=2.0.0\"\n",
        );
        fixture.write_source_pack("animus.shared", "1.0.0", "");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, false);

        let shared_rows: Vec<_> = rows.iter().filter(|row| row.id == "animus.shared").collect();
        assert_eq!(shared_rows.len(), 2, "both edges to the shared dep must be reported, got {rows:?}");
        assert!(shared_rows.iter().any(|row| row.status == "installed"));
        let conflict = shared_rows.iter().find(|row| row.status == "failed").expect("conflicting edge flagged");
        assert!(conflict.detail.as_deref().unwrap_or("").contains("conflicting requirement"));
    }

    #[test]
    fn dependency_closure_dry_run_flags_missing_offline_source() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack("animus.parent", "0.1.0", "[[dependencies]]\nid = \"animus.gone\"\n");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, true);

        let dep = row(&rows, "animus.gone");
        assert_eq!(dep.status, "failed", "a missing offline source must not report would_install, got {rows:?}");
        assert!(dep.detail.as_deref().unwrap_or("").contains("install manually"));
    }

    #[test]
    fn dependency_closure_conflict_check_uses_resolved_version_not_inactive_installs() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack(
            "animus.parent",
            "0.1.0",
            "[[dependencies]]\nid = \"animus.branch-a\"\n\n[[dependencies]]\nid = \"animus.branch-b\"\n",
        );
        fixture.write_source_pack(
            "animus.branch-a",
            "0.1.0",
            "[[dependencies]]\nid = \"animus.shared\"\nversion = \"=1.0.0\"\n",
        );
        fixture.write_source_pack(
            "animus.branch-b",
            "0.1.0",
            "[[dependencies]]\nid = \"animus.shared\"\nversion = \">=2.0.0\"\n",
        );
        fixture.write_source_pack("animus.shared", "1.0.0", "");
        // A 2.0.0 copy is installed but it is NOT the version this run resolves
        // and activates — it must not silence the conflicting edge.
        let two = tempfile::tempdir().expect("shared 2.0.0 tempdir");
        write_pack_with(two.path(), "animus.shared", "2.0.0", "");
        install_pack_from_source_root(fixture.project.path(), two.path(), false, false).expect("install 2.0.0");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, true, false);

        let shared_rows: Vec<_> = rows.iter().filter(|row| row.id == "animus.shared").collect();
        assert!(shared_rows.iter().any(|row| row.status == "installed"), "rows: {rows:?}");
        assert!(
            shared_rows
                .iter()
                .any(|row| row.status == "failed"
                    && row.detail.as_deref().unwrap_or("").contains("conflicting requirement")),
            "an inactive installed version must not satisfy the conflicting edge, got {rows:?}"
        );
    }

    #[test]
    fn collect_plugin_requirements_lets_required_override_optional() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack(
            "animus.parent",
            "0.1.0",
            concat!(
                "[[dependencies]]\nid = \"animus.dep-a\"\n\n",
                "[[requires_plugins]]\nrepo = \"launchapp-dev/animus-subject-linear\"\noptional = true\n",
            ),
        );
        fixture.write_source_pack(
            "animus.dep-a",
            "0.1.0",
            "[[requires_plugins]]\nrepo = \"launchapp-dev/animus-subject-linear\"\n",
        );
        install_pack_from_source_root(
            fixture.project.path(),
            &fixture.source_dir.path().join("animus.dep-a"),
            false,
            false,
        )
        .expect("install dep");

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, false);
        let requirements = collect_plugin_requirements(fixture.project.path(), &parent, &rows);

        assert_eq!(requirements.len(), 1);
        assert!(
            !requirements[0].optional,
            "the dependency's required declaration must override the parent's optional one"
        );
    }

    #[test]
    fn collect_plugin_requirements_covers_dry_run_dependencies_from_offline_source() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack("animus.parent", "0.1.0", "[[dependencies]]\nid = \"animus.dep-a\"\n");
        fixture.write_source_pack(
            "animus.dep-a",
            "0.1.0",
            "[[requires_plugins]]\nrepo = \"launchapp-dev/animus-subject-linear\"\n",
        );

        let parent = fixture.load_source_pack("animus.parent");
        let rows =
            resolve_dependency_closure(fixture.project.path(), &parent, PackInstallOrigin::LocalPath, false, true);
        assert_eq!(row(&rows, "animus.dep-a").status, "would_install");

        let requirements = collect_plugin_requirements(fixture.project.path(), &parent, &rows);
        assert_eq!(requirements.len(), 1);
        assert_eq!(requirements[0].repo, "launchapp-dev/animus-subject-linear");
    }

    fn linear_requirement(optional: bool) -> orchestrator_config::PackPluginRequirement {
        orchestrator_config::PackPluginRequirement {
            repo: "launchapp-dev/animus-subject-linear".to_string(),
            tag: Some("v0.2.0".to_string()),
            role: Some("subject_backend:linear".to_string()),
            optional,
            reason: None,
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the await
    async fn required_plugins_missing_surface_install_commands() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let project = tempfile::tempdir().expect("project tempdir");

        let rows = ensure_required_plugins(vec![linear_requirement(false)], false, false, false, project.path()).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "missing");
        assert_eq!(
            rows[0].install_command.as_deref(),
            Some("animus plugin install launchapp-dev/animus-subject-linear@v0.2.0")
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the await
    async fn required_plugins_optional_are_suggested_not_installed() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let project = tempfile::tempdir().expect("project tempdir");

        let rows = ensure_required_plugins(vec![linear_requirement(true)], true, false, false, project.path()).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "optional_suggestion");
        assert!(rows[0].install_command.is_some());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the await
    async fn required_plugins_present_in_registry_are_silent() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let animus_home = home.path().join(".animus");
        fs::create_dir_all(&animus_home).expect("create animus home");
        fs::write(
            animus_home.join("plugins.yaml"),
            "plugins:\n  animus-subject-linear:\n    origin: launchapp-dev/animus-subject-linear@v0.2.0\n    release_tag: v0.2.0\n",
        )
        .expect("write plugins.yaml");
        let project = tempfile::tempdir().expect("project tempdir");

        let rows = ensure_required_plugins(vec![linear_requirement(false)], false, false, false, project.path()).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "installed");
        assert!(rows[0].install_command.is_none());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the await
    async fn required_plugins_reject_same_name_plugin_from_other_repo() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let animus_home = home.path().join(".animus");
        fs::create_dir_all(&animus_home).expect("create animus home");
        // Same basename, different owner: must NOT satisfy the requirement.
        fs::write(
            animus_home.join("plugins.yaml"),
            "plugins:\n  animus-subject-linear:\n    origin: other-org/animus-subject-linear@v9.9.9\n",
        )
        .expect("write plugins.yaml");
        let project = tempfile::tempdir().expect("project tempdir");

        let rows = ensure_required_plugins(vec![linear_requirement(false)], false, false, false, project.path()).await;
        assert_eq!(rows[0].status, "missing", "a same-named plugin from another repo must not satisfy the requirement");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)] // intentional: guards process-global env mutation across the install await
    async fn required_plugins_install_flag_installs_from_offline_source() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let plugin_source = tempfile::tempdir().expect("plugin source tempdir");
        let _source = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_PACK_PLUGIN_SOURCE_DIR",
            Some(plugin_source.path().to_string_lossy().as_ref()),
        );
        let project = tempfile::tempdir().expect("project tempdir");

        // Fake plugin binary: answers `--manifest` with valid manifest JSON.
        let manifest = serde_json::json!({
            "name": "animus-subject-fake",
            "version": "0.1.0",
            "plugin_kind": "subject_backend",
            "description": "fake plugin for pack requires_plugins tests",
            "protocol_version": "1.0.0",
            "capabilities": [],
        });
        let binary = plugin_source.path().join("animus-subject-fake");
        fs::write(
            &binary,
            format!("#!/bin/sh\nif [ \"$1\" = \"--manifest\" ]; then\n  printf '%s\\n' '{manifest}'\nfi\n"),
        )
        .expect("write fake plugin binary");
        let mut perms = fs::metadata(&binary).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&binary, perms).expect("chmod");

        let requirement = orchestrator_config::PackPluginRequirement {
            repo: "launchapp-dev/animus-subject-fake".to_string(),
            tag: None,
            role: None,
            optional: false,
            reason: None,
        };
        let rows = ensure_required_plugins(vec![requirement.clone()], true, false, false, project.path()).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "installed_now", "rows: {rows:?}");

        // A second check sees the freshly registered plugin as installed.
        let rows = ensure_required_plugins(vec![requirement], false, false, false, project.path()).await;
        assert_eq!(rows[0].status, "installed");
    }

    #[test]
    fn pack_info_reports_dependency_and_plugin_status() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let fixture = DepFixture::new();
        fixture.write_source_pack(
            "animus.parent",
            "0.1.0",
            concat!(
                "[[dependencies]]\nid = \"animus.dep-a\"\nversion = \">=0.1.0\"\n\n",
                "[[requires_plugins]]\nrepo = \"launchapp-dev/animus-subject-linear\"\ntag = \"v0.2.0\"\n",
            ),
        );
        fixture.write_source_pack("animus.dep-a", "0.1.0", "");
        install_pack_from_source_root(
            fixture.project.path(),
            &fixture.source_dir.path().join("animus.dep-a"),
            false,
            false,
        )
        .expect("install dep");

        let args = PackInspectArgs {
            pack_id: None,
            version: None,
            source: None,
            path: Some(fixture.source_dir.path().join("animus.parent").to_string_lossy().to_string()),
        };
        let output = inspect_pack(fixture.project.path(), args).expect("inspect pack");

        assert_eq!(output.dependencies.len(), 1);
        assert!(output.dependencies[0].installed);
        assert_eq!(output.dependencies[0].installed_version.as_deref(), Some("0.1.0"));
        assert_eq!(output.required_plugins.len(), 1);
        assert!(!output.required_plugins[0].installed);
        assert_eq!(
            output.required_plugins[0].install_command.as_deref(),
            Some("animus plugin install launchapp-dev/animus-subject-linear@v0.2.0")
        );
    }

    #[test]
    fn parse_source_accepts_project_aliases() {
        assert_eq!(
            parse_source(Some("project")).expect("source should parse"),
            Some(PackRegistrySource::ProjectOverride)
        );
        assert_eq!(
            parse_source(Some("project-override")).expect("source should parse"),
            Some(PackRegistrySource::ProjectOverride)
        );
    }

    #[test]
    fn register_pack_in_registry_errors_on_unknown_registry() {
        let err = orchestrator_config::register_pack_in_registry(
            "no-such-registry",
            "my.pack",
            "https://github.com/example/my-pack.git",
            None,
            None,
        )
        .expect_err("unknown registry should error");
        assert!(err.to_string().contains("no-such-registry"), "error mentions registry id: {err}");
    }
}
