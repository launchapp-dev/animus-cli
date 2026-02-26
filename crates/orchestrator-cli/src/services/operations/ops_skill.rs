use crate::cli_types::{
    SkillCommand, SkillInstallArgs, SkillPublishArgs, SkillSearchArgs, SkillUpdateArgs,
};
use crate::print_value;
use anyhow::{anyhow, Result};
use semver::Version;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};

mod model;
mod resolver;
mod store;

use self::model::{
    ResolvedSkillEntry, SkillLockEntry, SkillLockStateV1, SkillProjectConstraint,
    SkillRegistrySourceConfig, SkillRegistryStateV1, SkillVersionRecord,
};
use self::resolver::{resolve_skill_version, ResolveSkillRequest};
use self::store::{
    load_skill_lock_state, load_skill_registry_state, save_skill_lock_state_if_changed,
    save_skill_registry_state_if_changed,
};

#[derive(Debug, Clone, Serialize)]
struct SkillListItem {
    name: String,
    version: String,
    source: String,
    registry: String,
    integrity: String,
    artifact: String,
    lock_status: String,
}

fn compare_semver_desc(left: &str, right: &str) -> std::cmp::Ordering {
    match (Version::parse(left), Version::parse(right)) {
        (Ok(left), Ok(right)) => right.cmp(&left),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => right.cmp(left),
    }
}

fn sanitize_required(value: &str, field_name: &str) -> Result<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        anyhow::bail!("invalid {field_name}");
    }
    Ok(normalized.to_string())
}

fn ensure_registry_available(state: &SkillRegistryStateV1, registry: Option<&str>) -> Result<()> {
    let Some(registry) = registry else {
        return Ok(());
    };
    let registry = registry.trim();
    if registry.is_empty() {
        anyhow::bail!("invalid registry");
    }
    if let Some(config) = state.registries.iter().find(|entry| entry.id == registry) {
        if !config.available {
            anyhow::bail!("registry backend unavailable: {}", registry);
        }
    }
    Ok(())
}

fn ensure_registry_registered(state: &mut SkillRegistryStateV1, registry: &str) {
    if state.registries.iter().any(|entry| entry.id == registry) {
        return;
    }
    let next_priority = state
        .registries
        .iter()
        .map(|entry| entry.priority)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    state.registries.push(SkillRegistrySourceConfig {
        id: registry.to_string(),
        priority: next_priority,
        available: true,
    });
}

fn find_lock_pin<'a>(
    lock_state: &'a SkillLockStateV1,
    name: &str,
    preferred_source: Option<&str>,
) -> Option<&'a SkillLockEntry> {
    let mut candidates: Vec<&SkillLockEntry> = lock_state
        .entries
        .iter()
        .filter(|entry| entry.name == name)
        .collect();
    if let Some(source) = preferred_source {
        candidates.retain(|entry| entry.source == source);
    }
    candidates.sort_by(|left, right| left.source.cmp(&right.source));
    candidates.into_iter().next()
}

fn find_project_default<'a>(
    state: &'a SkillRegistryStateV1,
    name: &str,
) -> Option<&'a SkillProjectConstraint> {
    state.defaults.iter().find(|item| item.name == name)
}

fn upsert_project_default(
    state: &mut SkillRegistryStateV1,
    name: &str,
    version: Option<String>,
    source: Option<String>,
    registry: Option<String>,
    allow_prerelease: bool,
) {
    let mut next = state
        .defaults
        .iter()
        .find(|item| item.name == name)
        .cloned()
        .unwrap_or(SkillProjectConstraint {
            name: name.to_string(),
            version: None,
            source: None,
            registry: None,
            allow_prerelease: false,
        });

    if let Some(version) = version {
        next.version = Some(version);
    }
    if let Some(source) = source {
        next.source = Some(source);
    }
    if let Some(registry) = registry {
        next.registry = Some(registry);
    }
    if allow_prerelease {
        next.allow_prerelease = true;
    }

    state.defaults.retain(|item| item.name != name);
    state.defaults.push(next);
}

fn upsert_installed(state: &mut SkillRegistryStateV1, selected: &SkillVersionRecord) {
    let entry = ResolvedSkillEntry {
        name: selected.name.clone(),
        version: selected.version.clone(),
        source: selected.source.clone(),
        registry: selected.registry.clone(),
        integrity: selected.integrity.clone(),
        artifact: selected.artifact.clone(),
    };
    state
        .installed
        .retain(|item| !(item.name == entry.name && item.source == entry.source));
    state.installed.push(entry);
}

fn upsert_lock_entry(lock_state: &mut SkillLockStateV1, selected: &SkillVersionRecord) {
    let entry = SkillLockEntry {
        name: selected.name.clone(),
        version: selected.version.clone(),
        source: selected.source.clone(),
        integrity: selected.integrity.clone(),
        artifact: selected.artifact.clone(),
        registry: Some(selected.registry.clone()),
    };
    lock_state
        .entries
        .retain(|item| !(item.name == entry.name && item.source == entry.source));
    lock_state.entries.push(entry);
}

fn lock_status_for(entry: &ResolvedSkillEntry, lock_state: &SkillLockStateV1) -> &'static str {
    let Some(lock_entry) = lock_state
        .entries
        .iter()
        .find(|item| item.name == entry.name && item.source == entry.source)
    else {
        return "missing";
    };
    if lock_entry.version == entry.version
        && lock_entry.integrity == entry.integrity
        && lock_entry.artifact == entry.artifact
    {
        "locked"
    } else {
        "out_of_sync"
    }
}

fn build_integrity(name: &str, version: &str, source: &str, artifact: &str) -> String {
    let payload = format!("{name}:{version}:{source}:{artifact}");
    let digest = Sha256::digest(payload.as_bytes());
    format!("sha256:{:x}", digest)
}

fn handle_search(args: SkillSearchArgs, project_root: &str, json: bool) -> Result<()> {
    let state = load_skill_registry_state(project_root)?;
    ensure_registry_available(&state, args.registry.as_deref())?;
    let registry_rank: HashMap<&str, u32> = state
        .registries
        .iter()
        .map(|item| (item.id.as_str(), item.priority))
        .collect();

    let query = args.query.map(|value| value.to_ascii_lowercase());
    let mut results: Vec<SkillVersionRecord> = state
        .catalog
        .into_iter()
        .filter(|record| {
            if let Some(query) = query.as_deref() {
                record.name.to_ascii_lowercase().contains(query)
            } else {
                true
            }
        })
        .filter(|record| {
            args.source
                .as_deref()
                .map(|source| record.source == source.trim())
                .unwrap_or(true)
        })
        .filter(|record| {
            args.registry
                .as_deref()
                .map(|registry| record.registry == registry.trim())
                .unwrap_or(true)
        })
        .collect();
    results.sort_by(|left, right| {
        registry_rank
            .get(left.registry.as_str())
            .unwrap_or(&u32::MAX)
            .cmp(
                registry_rank
                    .get(right.registry.as_str())
                    .unwrap_or(&u32::MAX),
            )
            .then_with(|| left.registry.cmp(&right.registry))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| compare_semver_desc(&left.version, &right.version))
            .then_with(|| right.version.cmp(&left.version))
    });

    print_value(results, json)
}

fn handle_install(args: SkillInstallArgs, project_root: &str, json: bool) -> Result<()> {
    let name = sanitize_required(&args.name, "skill name")?;
    let mut registry_state = load_skill_registry_state(project_root)?;
    ensure_registry_available(&registry_state, args.registry.as_deref())?;
    let mut lock_state = load_skill_lock_state(project_root)?;

    let lock_pin = find_lock_pin(&lock_state, &name, args.source.as_deref());
    let project_default = find_project_default(&registry_state, &name);
    let resolution = resolve_skill_version(
        &ResolveSkillRequest {
            name: &name,
            cli_version: args.version.as_deref(),
            cli_source: args.source.as_deref(),
            cli_registry: args.registry.as_deref(),
            allow_prerelease: args.allow_prerelease,
        },
        &registry_state.catalog,
        lock_pin,
        project_default,
    )?;

    ensure_registry_registered(&mut registry_state, &resolution.selected.registry);
    upsert_installed(&mut registry_state, &resolution.selected);
    upsert_project_default(
        &mut registry_state,
        &name,
        args.version,
        args.source.or(Some(resolution.selected.source.clone())),
        args.registry.or(Some(resolution.selected.registry.clone())),
        args.allow_prerelease,
    );
    upsert_lock_entry(&mut lock_state, &resolution.selected);

    let registry_changed = save_skill_registry_state_if_changed(project_root, &registry_state)?;
    let lock_changed = save_skill_lock_state_if_changed(project_root, &lock_state)?;

    print_value(
        serde_json::json!({
            "installed": resolution.selected,
            "used_lock_pin": resolution.used_lock_pin,
            "used_project_default": resolution.used_project_default,
            "registry_changed": registry_changed,
            "lock_changed": lock_changed,
        }),
        json,
    )
}

fn handle_list(project_root: &str, json: bool) -> Result<()> {
    let state = load_skill_registry_state(project_root)?;
    let lock_state = load_skill_lock_state(project_root)?;
    let mut items: Vec<SkillListItem> = state
        .installed
        .iter()
        .map(|entry| SkillListItem {
            name: entry.name.clone(),
            version: entry.version.clone(),
            source: entry.source.clone(),
            registry: entry.registry.clone(),
            integrity: entry.integrity.clone(),
            artifact: entry.artifact.clone(),
            lock_status: lock_status_for(entry, &lock_state).to_string(),
        })
        .collect();
    items.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.source.cmp(&right.source))
    });
    print_value(items, json)
}

fn resolve_update_targets(
    state: &SkillRegistryStateV1,
    name: Option<&str>,
    source: Option<&str>,
) -> Vec<(String, String)> {
    let mut targets = BTreeSet::new();
    for entry in &state.installed {
        if let Some(name) = name {
            if entry.name != name {
                continue;
            }
        }
        if let Some(source) = source {
            if entry.source != source {
                continue;
            }
        }
        targets.insert((entry.name.clone(), entry.source.clone()));
    }
    targets.into_iter().collect()
}

fn handle_update(args: SkillUpdateArgs, project_root: &str, json: bool) -> Result<()> {
    let mut registry_state = load_skill_registry_state(project_root)?;
    ensure_registry_available(&registry_state, args.registry.as_deref())?;
    let mut lock_state = load_skill_lock_state(project_root)?;

    let target_name = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let target_source = args
        .source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let targets = resolve_update_targets(&registry_state, target_name, target_source);

    if target_name.is_some() && targets.is_empty() {
        anyhow::bail!("skill not found: {}", target_name.unwrap_or_default());
    }

    let mut updated_entries = Vec::new();
    for (name, installed_source) in targets {
        let lock_pin = find_lock_pin(&lock_state, &name, Some(installed_source.as_str()));
        let project_default = find_project_default(&registry_state, &name);
        let resolution = resolve_skill_version(
            &ResolveSkillRequest {
                name: &name,
                cli_version: args.version.as_deref(),
                cli_source: args.source.as_deref(),
                cli_registry: args.registry.as_deref(),
                allow_prerelease: args.allow_prerelease,
            },
            &registry_state.catalog,
            lock_pin,
            project_default,
        )?;

        registry_state
            .installed
            .retain(|entry| !(entry.name == name && entry.source == installed_source));
        lock_state
            .entries
            .retain(|entry| !(entry.name == name && entry.source == installed_source));
        ensure_registry_registered(&mut registry_state, &resolution.selected.registry);
        upsert_installed(&mut registry_state, &resolution.selected);
        upsert_lock_entry(&mut lock_state, &resolution.selected);

        upsert_project_default(
            &mut registry_state,
            &name,
            args.version.clone(),
            args.source
                .clone()
                .or(Some(resolution.selected.source.clone())),
            args.registry
                .clone()
                .or(Some(resolution.selected.registry.clone())),
            args.allow_prerelease,
        );
        updated_entries.push(serde_json::json!({
            "name": resolution.selected.name,
            "version": resolution.selected.version,
            "source": resolution.selected.source,
            "registry": resolution.selected.registry,
            "used_lock_pin": resolution.used_lock_pin,
            "used_project_default": resolution.used_project_default,
        }));
    }

    let registry_changed = save_skill_registry_state_if_changed(project_root, &registry_state)?;
    let lock_changed = save_skill_lock_state_if_changed(project_root, &lock_state)?;

    print_value(
        serde_json::json!({
            "updated": updated_entries,
            "registry_changed": registry_changed,
            "lock_changed": lock_changed,
        }),
        json,
    )
}

fn handle_publish(args: SkillPublishArgs, project_root: &str, json: bool) -> Result<()> {
    let name = sanitize_required(&args.name, "skill name")?;
    let version = sanitize_required(&args.version, "skill version")?;
    let source = sanitize_required(&args.source, "skill source")?;
    let registry = sanitize_required(&args.registry, "registry")?;
    let artifact = args
        .artifact
        .as_deref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{name}-{version}.tgz"));
    Version::parse(&version)
        .map_err(|error| anyhow!("invalid version '{}': {}", version, error))?;

    let mut state = load_skill_registry_state(project_root)?;
    ensure_registry_available(&state, Some(&registry))?;
    if state
        .catalog
        .iter()
        .any(|entry| entry.name == name && entry.version == version && entry.source == source)
    {
        anyhow::bail!(
            "skill version already exists for source '{}': {}@{}",
            source,
            name,
            version
        );
    }

    ensure_registry_registered(&mut state, &registry);
    let integrity = args
        .integrity
        .as_deref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| build_integrity(&name, &version, &source, &artifact));

    let record = SkillVersionRecord {
        name,
        version,
        source,
        registry,
        integrity,
        artifact,
    };
    state.catalog.push(record.clone());
    let registry_changed = save_skill_registry_state_if_changed(project_root, &state)?;

    print_value(
        serde_json::json!({
            "published": record,
            "registry_changed": registry_changed,
        }),
        json,
    )
}

pub(crate) async fn handle_skill(
    command: SkillCommand,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        SkillCommand::Search(args) => handle_search(args, project_root, json),
        SkillCommand::Install(args) => handle_install(args, project_root, json),
        SkillCommand::List => handle_list(project_root, json),
        SkillCommand::Update(args) => handle_update(args, project_root, json),
        SkillCommand::Publish(args) => handle_publish(args, project_root, json),
    }
}
