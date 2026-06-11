use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use orchestrator_config::{
    add_marketplace_registry, check_pack_runtime_requirements, clone_marketplace_pack, load_marketplace_state,
    load_pack_inventory, load_pack_manifest, load_pack_selection_state, machine_installed_packs_dir,
    project_pack_overrides_dir, remove_marketplace_registry, save_pack_selection_state, search_marketplace_packs,
    sync_all_registries, sync_registry, PackInventoryEntry, PackRegistrySource, PackSelectionEntry,
    PackSelectionSource,
};
use serde::Serialize;

use crate::{
    conflict_error, invalid_input_error, not_found_error, print_ok, print_value, PackCommand, PackInspectArgs,
    PackPinArgs, PackRegistryCommand, PackUninstallArgs,
};

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
    manifest: orchestrator_config::PackManifest,
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

fn inspect_inventory_entry(entry: &PackInventoryEntry) -> Result<PackInspectOutput> {
    let pack = entry
        .loaded_manifest()
        .ok_or_else(|| anyhow!("pack '{}' does not expose an inspectable manifest", entry.pack_id))?;
    Ok(PackInspectOutput {
        pack_id: entry.pack_id.clone(),
        version: entry.version.clone(),
        source: entry.source.as_str().to_string(),
        active: Some(entry.active),
        pack_root: entry.pack_root.as_ref().map(|path| path.display().to_string()),
        manifest_path: entry.manifest_path.as_ref().map(|path| path.display().to_string()),
        selection: selection_summary(entry),
        runtime_report: check_pack_runtime_requirements(pack)?,
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
        return Ok(PackInspectOutput {
            pack_id: pack.manifest.id.clone(),
            version: pack.manifest.version.clone(),
            source: "local".to_string(),
            active: None,
            pack_root: Some(pack.pack_root.display().to_string()),
            manifest_path: Some(pack.manifest_path.display().to_string()),
            selection: None,
            runtime_report: check_pack_runtime_requirements(&pack)?,
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
    inspect_inventory_entry(entry)
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
            print_value(rows, json)
        }
        PackCommand::Inspect(args) => print_value(inspect_pack(project_root, args)?, json),
        PackCommand::Search(args) => {
            let results =
                search_marketplace_packs(args.query.as_deref(), args.category.as_deref(), args.registry.as_deref())?;
            if results.is_empty() && !json {
                print_ok("no packs found matching the query", false);
                return Ok(());
            }
            print_value(results, json)
        }
        PackCommand::Registry { command } => handle_registry(command, json),
        PackCommand::Install(args) => {
            let source_root = if let Some(name) = args.name.as_deref() {
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
                clone_marketplace_pack(&registry_id, name)?
            } else if let Some(path) = args.path.as_deref() {
                resolve_local_pack_root(path)?
            } else {
                return Err(invalid_input_error("either --path or --name is required for pack install"));
            };
            let output = install_pack_from_source_root(project_root, &source_root, args.activate, args.force)?;
            if json {
                return print_value(output, true);
            }
            print_ok(&format!("installed pack {} {}", output.pack_id, output.version), false);
            Ok(())
        }
        PackCommand::Pin(args) => handle_pin(project_root, args, json),
        PackCommand::Uninstall(args) => handle_uninstall(project_root, args, json),
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
}
