use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use orchestrator_config::{
    add_marketplace_registry, check_pack_runtime_requirements, clone_marketplace_pack, load_marketplace_state,
    load_pack_inventory, load_pack_manifest, load_pack_selection_state, machine_installed_packs_dir,
    remove_marketplace_registry, save_pack_selection_state, search_marketplace_packs, sync_all_registries, sync_registry,
    PackInventoryEntry, PackRegistrySource, PackSelectionEntry, PackSelectionSource,
};
use serde::Serialize;

use crate::{
    invalid_input_error, print_ok, print_value, PackCommand, PackInspectArgs, PackPinArgs, PackRegistryCommand,
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
struct PackInstallOutput {
    pack_id: String,
    version: String,
    installed_root: String,
    activated: bool,
}

fn parse_source(raw: Option<&str>) -> Result<Option<PackRegistrySource>> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let parsed = match raw.to_ascii_lowercase().as_str() {
        "bundled" => PackRegistrySource::Bundled,
        "installed" => PackRegistrySource::Installed,
        "project_override" | "project-override" | "project" => PackRegistrySource::ProjectOverride,
        _ => {
            return Err(invalid_input_error(format!(
                "unsupported pack source '{}'; expected bundled, installed, or project_override",
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
            let results = search_marketplace_packs(
                args.query.as_deref(),
                args.category.as_deref(),
                args.registry.as_deref(),
            )?;
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
            let loaded = load_pack_manifest(&source_root)?;
            let target_root = machine_installed_packs_dir().join(&loaded.manifest.id).join(&loaded.manifest.version);

            if target_root.exists() {
                if !args.force {
                    return Err(anyhow!(
                        "pack '{}' version '{}' already exists at {} (use --force to overwrite)",
                        loaded.manifest.id,
                        loaded.manifest.version,
                        target_root.display()
                    ));
                }
                fs::remove_dir_all(&target_root)
                    .with_context(|| format!("failed to remove {}", target_root.display()))?;
            }

            copy_dir_recursive(&source_root, &target_root)?;

            if args.activate {
                let mut state = load_pack_selection_state(project_root)?;
                state.upsert(PackSelectionEntry {
                    pack_id: loaded.manifest.id.clone(),
                    version: Some(format!("={}", loaded.manifest.version)),
                    source: Some(PackSelectionSource::Installed),
                    enabled: true,
                })?;
                save_pack_selection_state(project_root, &state)?;
            }

            let output = PackInstallOutput {
                pack_id: loaded.manifest.id,
                version: loaded.manifest.version,
                installed_root: target_root.display().to_string(),
                activated: args.activate,
            };
            if json {
                return print_value(output, true);
            }
            print_ok(&format!("installed pack {} {}", output.pack_id, output.version), false);
            Ok(())
        }
        PackCommand::Pin(args) => handle_pin(project_root, args, json),
    }
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
