use crate::cli_types::{
    SkillCommand, SkillCreateArgs, SkillInstallArgs, SkillListArgs, SkillMigrateFromAoArgs, SkillPublishArgs,
    SkillRegistryAddArgs, SkillRegistryCommand, SkillRegistryRemoveArgs, SkillSearchArgs, SkillShowArgs,
    SkillUninstallArgs, SkillUpdateArgs,
};
use crate::{conflict_error, invalid_input_error, not_found_error, print_value, render_table, unavailable_error};
use anyhow::{Context, Result};
use orchestrator_config::skill_definition::{
    skill_definition_warnings, SkillActivation, SkillDefinition, SkillModelPreference, SkillPrompt,
};
use orchestrator_config::skill_resolution::{list_available_skills, resolve_skill};
use orchestrator_config::skill_scoping::{
    legacy_project_markdown_skills_dir, legacy_project_yaml_skills_dir, legacy_user_markdown_skills_dir,
    legacy_user_yaml_skills_dir, load_markdown_skill_file, load_skill_sources, markdown_skill_file_for_path,
    migrate_legacy_skills_from_ao, parse_skill_category_label, project_markdown_skills_dir, project_skills_dir,
    set_suppress_markdown_skill_parse_warnings, user_markdown_skills_dir, user_skills_dir, validate_skill_slug,
    write_skill_yaml, MigrateFromAoOutcome, SkillWriteOutcome, SkillWriteScope,
};
use semver::Version;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

mod model;
mod resolver;
mod store;

use self::model::{
    ResolvedSkillEntry, SkillLockEntry, SkillLockStateV1, SkillProjectConstraint, SkillRegistrySourceConfig,
    SkillRegistryStateV1, SkillVersionRecord,
};
use self::resolver::{resolve_skill_version, ResolveSkillRequest};
use self::store::{
    load_skill_lock_state, load_skill_registry_state, save_skill_lock_state_if_changed,
    save_skill_registry_state_if_changed,
};

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
        return Err(invalid_input_error(format!("invalid {field_name}")));
    }
    Ok(normalized.to_string())
}

fn ensure_registry_available(state: &SkillRegistryStateV1, registry: Option<&str>) -> Result<()> {
    let Some(registry) = registry else {
        return Ok(());
    };
    let registry = registry.trim();
    if registry.is_empty() {
        return Err(invalid_input_error("invalid registry"));
    }
    if let Some(config) = state.registries.iter().find(|entry| entry.id == registry) {
        if !config.available {
            return Err(unavailable_error(format!("registry backend unavailable: {}", registry)));
        }
    }
    Ok(())
}

fn ensure_registry_registered(state: &mut SkillRegistryStateV1, registry: &str) {
    if state.registries.iter().any(|entry| entry.id == registry) {
        return;
    }
    let next_priority = state.registries.iter().map(|entry| entry.priority).max().unwrap_or(0).saturating_add(1);
    state.registries.push(SkillRegistrySourceConfig {
        id: registry.to_string(),
        priority: next_priority,
        available: true,
        url: None,
    });
}

fn find_lock_pin<'a>(
    lock_state: &'a SkillLockStateV1,
    name: &str,
    preferred_source: Option<&str>,
) -> Option<&'a SkillLockEntry> {
    let mut candidates: Vec<&SkillLockEntry> = lock_state.entries.iter().filter(|entry| entry.name == name).collect();
    if let Some(source) = preferred_source {
        candidates.retain(|entry| entry.source == source);
    }
    candidates.sort_by(|left, right| left.source.cmp(&right.source));
    candidates.into_iter().next()
}

fn find_project_default<'a>(state: &'a SkillRegistryStateV1, name: &str) -> Option<&'a SkillProjectConstraint> {
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
    let mut next = state.defaults.iter().find(|item| item.name == name).cloned().unwrap_or(SkillProjectConstraint {
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
        definition: selected.definition.clone(),
    };
    state.installed.retain(|item| !(item.name == entry.name && item.source == entry.source));
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
    lock_state.entries.retain(|item| !(item.name == entry.name && item.source == entry.source));
    lock_state.entries.push(entry);
}

fn local_skill_definition_snapshot(project_root: &str, name: &str) -> Option<orchestrator_config::SkillDefinition> {
    let sources = load_skill_sources(Path::new(project_root), None).ok()?;
    resolve_skill(name, &sources).ok().map(|resolved| resolved.definition)
}

fn lock_status_for(entry: &ResolvedSkillEntry, lock_state: &SkillLockStateV1) -> &'static str {
    let Some(lock_entry) =
        lock_state.entries.iter().find(|item| item.name == entry.name && item.source == entry.source)
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

fn skill_install_root(project_root: &str) -> PathBuf {
    project_markdown_skills_dir(Path::new(project_root))
}

/// Render non-fatal definition warnings (inert `activation.tools` /
/// `adapters` entries) as display strings for CLI/JSON output. Empty when the
/// definition is clean.
fn definition_warning_strings(definition: &SkillDefinition) -> Vec<String> {
    skill_definition_warnings(definition).iter().map(ToString::to_string).collect()
}

fn render_skill_definition_as_markdown(definition: &SkillDefinition) -> Result<String> {
    let mut frontmatter = String::new();
    frontmatter.push_str("name: ");
    frontmatter.push_str(&serde_json::to_string(&definition.name)?);
    frontmatter.push('\n');
    if !definition.description.trim().is_empty() {
        frontmatter.push_str("description: ");
        frontmatter.push_str(&serde_json::to_string(&definition.description)?);
        frontmatter.push('\n');
    }
    if let Some(version) = definition.version.as_deref().filter(|value| !value.trim().is_empty()) {
        frontmatter.push_str("version: ");
        frontmatter.push_str(&serde_json::to_string(version)?);
        frontmatter.push('\n');
    }

    let body =
        definition.prompt.system.as_deref().filter(|value| !value.trim().is_empty()).map(str::trim).unwrap_or("");

    Ok(format!("---\n{}---\n\n{}\n", frontmatter, body))
}

fn write_bytes_if_changed(path: &Path, bytes: &[u8]) -> Result<bool> {
    if path.exists() && fs::read(path).with_context(|| format!("failed to read {}", path.display()))? == bytes {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

fn write_skill_definition_file(project_root: &str, definition: &SkillDefinition) -> Result<bool> {
    let path = skill_install_root(project_root).join(&definition.name).join("SKILL.md");
    let rendered = render_skill_definition_as_markdown(definition)?;
    write_bytes_if_changed(&path, rendered.as_bytes())
}

fn discover_local_markdown_skill_files(path: &Path) -> Result<Vec<PathBuf>> {
    let direct = markdown_skill_file_for_path(path);
    if direct.is_file() {
        return Ok(vec![direct]);
    }
    if !path.is_dir() {
        return Err(not_found_error(format!("skill path not found: {}", path.display())));
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let candidate = markdown_skill_file_for_path(&entry.path());
        if candidate.is_file() {
            files.push(candidate);
        }
    }
    files.sort();

    if files.is_empty() {
        return Err(not_found_error(format!("no Markdown skills found in {}", path.display())));
    }

    Ok(files)
}

fn copy_dir_recursive_if_changed(source: &Path, destination: &Path) -> Result<bool> {
    if source.canonicalize().ok().as_ref() == destination.canonicalize().ok().as_ref() {
        return Ok(false);
    }

    let mut changed = false;
    fs::create_dir_all(destination).with_context(|| format!("failed to create {}", destination.display()))?;
    for entry in fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            changed |= copy_dir_recursive_if_changed(&source_path, &destination_path)?;
        } else if source_path.is_file() {
            let bytes = fs::read(&source_path).with_context(|| format!("failed to read {}", source_path.display()))?;
            changed |= write_bytes_if_changed(&destination_path, &bytes)?;
        }
    }
    Ok(changed)
}

fn install_local_markdown_skills(
    path: &Path,
    name_filter: Option<&str>,
    project_root: &str,
) -> Result<Vec<serde_json::Value>> {
    let name_filter = name_filter.map(str::trim).filter(|value| !value.is_empty());
    let files = discover_local_markdown_skill_files(path)?;
    let mut installed = Vec::new();

    for file in files {
        let definition = load_markdown_skill_file(&file)?;
        if name_filter.is_some_and(|name| name != definition.name) {
            continue;
        }

        let destination = skill_install_root(project_root).join(&definition.name);
        let changed = if file
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
        {
            let source_dir = file.parent().context("skill file should have parent directory")?;
            copy_dir_recursive_if_changed(source_dir, &destination)?
        } else {
            let bytes = fs::read(&file).with_context(|| format!("failed to read {}", file.display()))?;
            write_bytes_if_changed(&destination.join("SKILL.md"), &bytes)?
        };

        installed.push(serde_json::json!({
            "name": definition.name,
            "description": definition.description,
            "path": destination.join("SKILL.md"),
            "changed": changed,
            "type": "markdown",
        }));
    }

    if installed.is_empty() {
        return Err(not_found_error(format!("skill not found in path: {}", name_filter.unwrap_or_default())));
    }

    Ok(installed)
}

const PROJECT_SHADOWS_USER_NOTE: &str =
    "project-scoped skills shadow user-scoped skills with the same name during resolution";

fn skill_write_outcome_label(outcome: SkillWriteOutcome) -> &'static str {
    match outcome {
        SkillWriteOutcome::Created => "created",
        SkillWriteOutcome::Updated => "updated",
    }
}

fn handle_create(args: SkillCreateArgs, project_root: &str, json: bool) -> Result<()> {
    let name = validate_skill_slug(&args.name).map_err(|err| invalid_input_error(err.to_string()))?;

    let description = args.description.trim().to_string();
    if description.is_empty() {
        return Err(invalid_input_error("description must not be empty"));
    }

    let prompt = match (args.prompt, args.prompt_file) {
        (Some(prompt), None) => prompt,
        (None, Some(path)) => {
            fs::read_to_string(&path).with_context(|| format!("failed to read --prompt-file {}", path.display()))?
        }
        (None, None) => return Err(invalid_input_error("pass --prompt or --prompt-file")),
        (Some(_), Some(_)) => unreachable!("clap conflicts_with prevents --prompt with --prompt-file"),
    };
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(invalid_input_error("prompt must not be empty"));
    }

    let category = match args.category.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) => Some(parse_skill_category_label(raw).map_err(|err| invalid_input_error(err.to_string()))?),
        None => None,
    };

    // `--project` and `--user` are mutually exclusive via clap; project is the default.
    let scope = match (args.project, args.user) {
        (_, true) => SkillWriteScope::User,
        (_, false) => SkillWriteScope::Project,
    };

    let definition = SkillDefinition {
        name: name.clone(),
        version: None,
        description,
        category,
        activation: SkillActivation::default(),
        prompt: SkillPrompt { system: Some(prompt), ..SkillPrompt::default() },
        tool_policy: None,
        model: SkillModelPreference::default(),
        mcp_servers: Vec::new(),
        timeout_secs: None,
        capabilities: BTreeMap::new(),
        extra_args: Vec::new(),
        env: BTreeMap::new(),
        codex_config_overrides: Vec::new(),
        adapters: BTreeMap::new(),
        tags: args.tags.into_iter().map(|tag| tag.trim().to_string()).filter(|tag| !tag.is_empty()).collect(),
    };

    let (path, outcome) = write_skill_yaml(Path::new(project_root), scope, &definition, args.force).map_err(|err| {
        let message = err.to_string().replace("pass overwrite=true", "pass --force");
        if message.contains("already") {
            conflict_error(message)
        } else {
            invalid_input_error(message)
        }
    })?;

    print_value(
        serde_json::json!({
            "name": name,
            "scope": scope.to_string(),
            "path": path,
            "outcome": skill_write_outcome_label(outcome),
            "note": PROJECT_SHADOWS_USER_NOTE,
        }),
        json,
    )
}

fn handle_search(args: SkillSearchArgs, project_root: &str, json: bool) -> Result<()> {
    let query = args.query.map(|value| value.to_ascii_lowercase());
    let source_filter = args.source.as_deref().map(|s| s.trim().to_ascii_lowercase());
    let registry_filter = args.registry.as_deref();

    let mut combined: Vec<serde_json::Value> = Vec::new();

    let skip_definitions = source_filter.as_deref() == Some("installed") || registry_filter.is_some();
    if !skip_definitions {
        let sources = load_skill_sources(Path::new(project_root), None).unwrap_or_default();
        let available = list_available_skills(&sources);
        for resolved in available {
            let origin = resolved.source.to_string();
            if let Some(ref sf) = source_filter {
                if &origin != sf {
                    continue;
                }
            }
            if let Some(ref q) = query {
                if !resolved.definition.name.to_ascii_lowercase().contains(q.as_str()) {
                    continue;
                }
            }
            combined.push(serde_json::json!({
                "name": resolved.definition.name,
                "description": resolved.definition.description,
                "source": origin,
                "category": resolved.definition.category.as_ref().map(|c| format!("{:?}", c)),
                "type": "definition",
            }));
        }
    }

    let skip_registry = matches!(source_filter.as_deref(), Some("built-in" | "user" | "project"));
    if !skip_registry {
        let state = load_skill_registry_state(project_root)?;
        ensure_registry_available(&state, registry_filter)?;
        let registry_rank: HashMap<&str, u32> =
            state.registries.iter().map(|item| (item.id.as_str(), item.priority)).collect();

        let mut catalog_results: Vec<SkillVersionRecord> =
            state
                .catalog
                .into_iter()
                .filter(|record| {
                    if let Some(ref q) = query {
                        record.name.to_ascii_lowercase().contains(q.as_str())
                    } else {
                        true
                    }
                })
                .filter(|record| registry_filter.map(|registry| record.registry == registry.trim()).unwrap_or(true))
                .collect();
        catalog_results.sort_by(|left, right| {
            registry_rank
                .get(left.registry.as_str())
                .unwrap_or(&u32::MAX)
                .cmp(registry_rank.get(right.registry.as_str()).unwrap_or(&u32::MAX))
                .then_with(|| left.registry.cmp(&right.registry))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| compare_semver_desc(&left.version, &right.version))
                .then_with(|| right.version.cmp(&left.version))
        });
        for record in catalog_results {
            combined.push(serde_json::json!({
                "name": record.name,
                "version": record.version,
                "source": record.source,
                "registry": record.registry,
                "integrity": record.integrity,
                "artifact": record.artifact,
                "type": "registry",
            }));
        }
    }

    print_value(combined, json)
}

fn handle_install(args: SkillInstallArgs, project_root: &str, json: bool) -> Result<()> {
    if let Some(path) = args.path.as_deref() {
        let installed = install_local_markdown_skills(path, args.name.as_deref(), project_root)?;
        return print_value(
            serde_json::json!({
                "installed": installed,
                "install_root": skill_install_root(project_root),
            }),
            json,
        );
    }

    let name = sanitize_required(args.name.as_deref().unwrap_or_default(), "skill name")?;
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
    let skill_file_changed = resolution
        .selected
        .definition
        .as_ref()
        .map(|definition| write_skill_definition_file(project_root, definition))
        .transpose()?
        .unwrap_or(false);

    print_value(
        serde_json::json!({
            "installed": resolution.selected,
            "used_lock_pin": resolution.used_lock_pin,
            "used_project_default": resolution.used_project_default,
            "registry_changed": registry_changed,
            "lock_changed": lock_changed,
            "skill_file_changed": skill_file_changed,
        }),
        json,
    )
}

fn handle_uninstall(args: SkillUninstallArgs, project_root: &str, json: bool) -> Result<()> {
    let name = sanitize_required(&args.name, "skill name")?;
    if name.contains(['/', '\\']) || name == "." || name == ".." {
        return Err(invalid_input_error(format!("invalid skill name '{}'", name)));
    }
    let source = args.source.as_deref().map(str::trim).filter(|value| !value.is_empty());

    let mut registry_state = load_skill_registry_state(project_root)?;
    let mut lock_state = load_skill_lock_state(project_root)?;

    let matches_target =
        |entry_name: &str, entry_source: &str| entry_name == name && source.is_none_or(|s| entry_source == s);
    let removed_installed = registry_state
        .installed
        .iter()
        .filter(|entry| matches_target(&entry.name, &entry.source))
        .map(|entry| {
            serde_json::json!({
                "name": entry.name,
                "version": entry.version,
                "source": entry.source,
                "registry": entry.registry,
            })
        })
        .collect::<Vec<_>>();
    let removed_lock_entries = lock_state
        .entries
        .iter()
        .filter(|entry| matches_target(&entry.name, &entry.source))
        .map(|entry| {
            serde_json::json!({
                "name": entry.name,
                "version": entry.version,
                "source": entry.source,
                "registry": entry.registry,
            })
        })
        .collect::<Vec<_>>();

    registry_state.installed.retain(|entry| !matches_target(&entry.name, &entry.source));
    lock_state.entries.retain(|entry| !matches_target(&entry.name, &entry.source));

    // A --source filter that matched nothing must not cascade into deleting
    // the shared materialized files or the per-name project default.
    let source_satisfied = source.is_none() || !removed_installed.is_empty() || !removed_lock_entries.is_empty();
    let remaining_installed = registry_state.installed.iter().any(|entry| entry.name == name);
    let removed_project_default =
        source_satisfied && !remaining_installed && registry_state.defaults.iter().any(|d| d.name == name);
    if removed_project_default {
        registry_state.defaults.retain(|d| d.name != name);
    }

    let skill_dir = skill_install_root(project_root).join(&name);

    // The materialized SKILL.md is shared per-name and may hold the removed
    // source's definition; rewrite it from a remaining source's snapshot, or
    // drop it when no snapshot is left so the removed definition does not
    // stay active (`skill update` re-materializes it).
    let remaining_definition = (remaining_installed && !removed_installed.is_empty() && skill_dir.is_dir())
        .then(|| {
            registry_state
                .installed
                .iter()
                .find(|entry| entry.name == name && entry.definition.is_some())
                .and_then(|entry| entry.definition.clone())
        })
        .flatten();
    let stale_without_snapshot =
        remaining_installed && !removed_installed.is_empty() && skill_dir.is_dir() && remaining_definition.is_none();
    let removed_skill_dir = (source_satisfied && !remaining_installed && skill_dir.is_dir()) || stale_without_snapshot;

    if removed_installed.is_empty() && removed_lock_entries.is_empty() && !removed_project_default && !removed_skill_dir
    {
        return Err(not_found_error(match source {
            Some(source) => format!("skill not installed from source '{}': {}", source, name),
            None => format!("skill not installed: {}", name),
        }));
    }

    let registry_modified = !removed_installed.is_empty() || removed_project_default;
    let lock_modified = !removed_lock_entries.is_empty();
    let mut rewrote_skill_file = false;
    let (registry_changed, lock_changed) = if args.dry_run {
        rewrote_skill_file = remaining_definition.is_some();
        (false, false)
    } else {
        let registry_changed =
            registry_modified && save_skill_registry_state_if_changed(project_root, &registry_state)?;
        let lock_changed = lock_modified && save_skill_lock_state_if_changed(project_root, &lock_state)?;
        if removed_skill_dir {
            fs::remove_dir_all(&skill_dir).with_context(|| format!("failed to remove {}", skill_dir.display()))?;
        } else if let Some(definition) = remaining_definition.as_ref() {
            rewrote_skill_file = write_skill_definition_file(project_root, definition)?;
        }
        (registry_changed, lock_changed)
    };

    print_value(
        serde_json::json!({
            "name": name,
            "dry_run": args.dry_run,
            "removed_installed": removed_installed,
            "removed_lock_entries": removed_lock_entries,
            "removed_project_default": removed_project_default,
            "removed_skill_dir": removed_skill_dir.then(|| skill_dir.display().to_string()),
            "rewrote_skill_file": rewrote_skill_file,
            "registry_changed": registry_changed,
            "lock_changed": lock_changed,
        }),
        json,
    )
}

fn handle_list(args: SkillListArgs, project_root: &str, json: bool) -> Result<()> {
    let source_filter = args.source.as_deref().map(|s| s.trim().to_ascii_lowercase());
    let mut items: Vec<serde_json::Value> = Vec::new();

    // Suppress per-file "could not parse markdown skill" warnings from
    // foreign tool directories (~/.claude, ~/.codex, ~/.cursor) unless the
    // operator explicitly asks for them. Skill list sweeps those dirs to
    // surface adapter-shaped skills, so unrelated parse failures otherwise
    // spam the default output.
    set_suppress_markdown_skill_parse_warnings(!args.verbose);
    let _restore_warnings = RestoreSkillParseWarnings;

    let skip_definitions = source_filter.as_deref() == Some("installed");
    if !skip_definitions {
        let sources = load_skill_sources(Path::new(project_root), None).unwrap_or_default();
        let available = list_available_skills(&sources);
        for resolved in available {
            let origin = resolved.source.to_string();
            if let Some(ref sf) = source_filter {
                if &origin != sf {
                    continue;
                }
            }
            let mut item = serde_json::json!({
                "name": resolved.definition.name,
                "description": resolved.definition.description,
                "source": origin,
                "category": resolved.definition.category.as_ref().map(|c| format!("{:?}", c)),
                "type": "definition",
            });
            let warnings = definition_warning_strings(&resolved.definition);
            if !warnings.is_empty() {
                item.as_object_mut().unwrap().insert("warnings".to_string(), serde_json::json!(warnings));
            }
            items.push(item);
        }
    }

    let skip_registry = matches!(source_filter.as_deref(), Some("built-in" | "user" | "project"));
    if !skip_registry {
        let state = load_skill_registry_state(project_root)?;
        let lock_state = load_skill_lock_state(project_root)?;
        for entry in &state.installed {
            items.push(serde_json::json!({
                "name": entry.name,
                "version": entry.version,
                "source": entry.source,
                "registry": entry.registry,
                "integrity": entry.integrity,
                "artifact": entry.artifact,
                "definition_snapshot": entry.definition.is_some(),
                "lock_status": lock_status_for(entry, &lock_state),
                "type": "installed",
            }));
        }
    }

    items.sort_by(|a, b| {
        let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
        a_name.cmp(b_name)
    });

    if !json {
        if items.is_empty() {
            println!("No skills found. Author one with: animus skill create --name <name> --description <text>");
            return Ok(());
        }
        let rows: Vec<Vec<String>> = items
            .iter()
            .map(|item| {
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("--").to_string();
                let version = item.get("version").and_then(|v| v.as_str()).unwrap_or("--").to_string();
                let source = item.get("source").and_then(|v| v.as_str()).unwrap_or("--").to_string();
                let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("--").to_string();
                vec![name, version, source, kind]
            })
            .collect();
        render_table(&["NAME", "VERSION", "SOURCE", "TYPE"], &rows);
        return Ok(());
    }

    print_value(items, json)
}

/// RAII guard that re-enables markdown-skill parse warnings when the skill
/// list scope exits, so the process-global suppression never leaks into
/// subsequent operations (e.g. workflow validate) in long-lived hosts.
struct RestoreSkillParseWarnings;

impl Drop for RestoreSkillParseWarnings {
    fn drop(&mut self) {
        set_suppress_markdown_skill_parse_warnings(false);
    }
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

    let target_name = args.name.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let target_source = args.source.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let targets = resolve_update_targets(&registry_state, target_name, target_source);

    if target_name.is_some() && targets.is_empty() {
        return Err(not_found_error(format!("skill not found: {}", target_name.unwrap_or_default())));
    }

    let mut updated_entries = Vec::new();
    let mut skill_file_changed = false;
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

        registry_state.installed.retain(|entry| !(entry.name == name && entry.source == installed_source));
        lock_state.entries.retain(|entry| !(entry.name == name && entry.source == installed_source));
        ensure_registry_registered(&mut registry_state, &resolution.selected.registry);
        upsert_installed(&mut registry_state, &resolution.selected);
        upsert_lock_entry(&mut lock_state, &resolution.selected);

        upsert_project_default(
            &mut registry_state,
            &name,
            args.version.clone(),
            args.source.clone().or(Some(resolution.selected.source.clone())),
            args.registry.clone().or(Some(resolution.selected.registry.clone())),
            args.allow_prerelease,
        );
        if let Some(definition) = resolution.selected.definition.as_ref() {
            skill_file_changed |= write_skill_definition_file(project_root, definition)?;
        }
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
            "skill_file_changed": skill_file_changed,
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
        .map_err(|error| invalid_input_error(format!("invalid version '{}': {}", version, error)))?;

    let mut state = load_skill_registry_state(project_root)?;
    ensure_registry_available(&state, Some(&registry))?;
    if state.catalog.iter().any(|entry| entry.name == name && entry.version == version && entry.source == source) {
        return Err(conflict_error(format!(
            "skill version already exists for source '{}': {}@{}",
            source, name, version
        )));
    }

    ensure_registry_registered(&mut state, &registry);
    let integrity = args
        .integrity
        .as_deref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| build_integrity(&name, &version, &source, &artifact));

    let definition = local_skill_definition_snapshot(project_root, &name);
    let record = SkillVersionRecord { name, version, source, registry, integrity, artifact, definition };
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

fn handle_registry_add(args: SkillRegistryAddArgs, project_root: &str, json: bool) -> Result<()> {
    let id = sanitize_required(&args.id, "id")?;
    let url = sanitize_required(&args.url, "url")?;
    let mut state = load_skill_registry_state(project_root)?;
    let existing = state.registries.iter().find(|entry| entry.id == id).cloned();
    let default_priority = state.registries.iter().map(|entry| entry.priority).max().unwrap_or(0).saturating_add(1);
    let priority = args.priority.unwrap_or_else(|| existing.as_ref().map(|e| e.priority).unwrap_or(default_priority));
    state.registries.retain(|entry| entry.id != id);
    let registry = SkillRegistrySourceConfig { id: id.clone(), priority, available: true, url: Some(url) };
    state.registries.push(SkillRegistrySourceConfig {
        id: registry.id.clone(),
        priority: registry.priority,
        available: registry.available,
        url: registry.url.clone(),
    });
    let changed = save_skill_registry_state_if_changed(project_root, &state)?;
    print_value(
        serde_json::json!({
            "registry": registry,
            "registry_changed": changed,
        }),
        json,
    )
}

fn handle_registry_remove(args: SkillRegistryRemoveArgs, project_root: &str, json: bool) -> Result<()> {
    let id = sanitize_required(&args.id, "id")?;
    let mut state = load_skill_registry_state(project_root)?;
    if !state.registries.iter().any(|entry| entry.id == id) {
        return Err(not_found_error(format!("registry not found: {}", id)));
    }
    state.registries.retain(|entry| entry.id != id);
    let changed = save_skill_registry_state_if_changed(project_root, &state)?;
    print_value(
        serde_json::json!({
            "removed_id": id,
            "registry_changed": changed,
        }),
        json,
    )
}

fn handle_registry_list(project_root: &str, json: bool) -> Result<()> {
    let mut state = load_skill_registry_state(project_root)?;
    state.normalize();
    print_value(&state.registries, json)
}

fn handle_show(args: SkillShowArgs, project_root: &str, json: bool) -> Result<()> {
    let sources = load_skill_sources(Path::new(project_root), None)?;
    match resolve_skill(&args.name, &sources) {
        Ok(resolved) => {
            let def = &resolved.definition;
            let mut payload = serde_json::json!({
                "name": def.name,
                "description": def.description,
                "source": resolved.source.to_string(),
                "category": def.category.as_ref().map(|c| format!("{:?}", c)),
                "version": def.version,
                "tags": def.tags,
                "prompt": {
                    "system": def.prompt.system,
                    "prefix": def.prompt.prefix,
                    "suffix": def.prompt.suffix,
                    "directives": def.prompt.directives,
                },
                "mcp_servers": def.mcp_servers,
                "timeout_secs": def.timeout_secs,
                "capabilities": def.capabilities,
                "adapters": def.adapters.keys().collect::<Vec<_>>(),
            });
            let warnings = definition_warning_strings(def);
            if !warnings.is_empty() {
                payload.as_object_mut().unwrap().insert("warnings".to_string(), serde_json::json!(warnings));
            }
            print_value(payload, json)
        }
        Err(_) => {
            let state = load_skill_registry_state(project_root)?;
            let installed = state.installed.iter().find(|e| e.name == args.name);
            match installed {
                Some(entry) => print_value(
                    serde_json::json!({
                        "name": entry.name,
                        "version": entry.version,
                        "source": entry.source,
                        "registry": entry.registry,
                        "integrity": entry.integrity,
                        "artifact": entry.artifact,
                        "definition_snapshot": entry.definition.is_some(),
                        "definition": entry.definition.clone(),
                        "type": "installed",
                    }),
                    json,
                ),
                None => Err(not_found_error(format!("skill not found: {}", args.name))),
            }
        }
    }
}

fn outcome_to_json(outcome: &MigrateFromAoOutcome) -> serde_json::Value {
    serde_json::json!({
        "scope": outcome.scope,
        "legacy_path": outcome.legacy_path,
        "animus_path": outcome.animus_path,
        "moved": outcome.moved,
        "already_migrated": outcome.already_migrated,
        "entries_moved": outcome.entries_moved,
        "notes": outcome.notes,
    })
}

fn migrate_one(legacy: &Path, new: &Path, scope: &'static str, dry_run: bool) -> Result<MigrateFromAoOutcome> {
    if dry_run {
        let mut outcome = MigrateFromAoOutcome {
            scope,
            legacy_path: legacy.to_path_buf(),
            animus_path: new.to_path_buf(),
            ..Default::default()
        };
        if !legacy.exists() {
            outcome.notes.push("dry-run: no legacy directory found".to_string());
        } else {
            outcome.notes.push(format!("dry-run: would move entries from {} to {}", legacy.display(), new.display()));
        }
        return Ok(outcome);
    }
    migrate_legacy_skills_from_ao(legacy, new, scope)
        .with_context(|| format!("{}-scope migration ({} -> {})", scope, legacy.display(), new.display()))
}

fn handle_migrate_from_ao(args: SkillMigrateFromAoArgs, project_root: &str, json: bool) -> Result<()> {
    let project_root_path = Path::new(project_root);
    let mut outcomes: Vec<MigrateFromAoOutcome> = Vec::new();

    // Project scope: markdown skills dir + yaml skill_definitions dir.
    outcomes.push(migrate_one(
        &legacy_project_markdown_skills_dir(project_root_path),
        &project_markdown_skills_dir(project_root_path),
        "project-skills",
        args.dry_run,
    )?);
    outcomes.push(migrate_one(
        &legacy_project_yaml_skills_dir(project_root_path),
        &project_skills_dir(project_root_path),
        "project-skill-definitions",
        args.dry_run,
    )?);

    if !args.project_only {
        outcomes.push(migrate_one(
            &legacy_user_markdown_skills_dir(),
            &user_markdown_skills_dir(),
            "user-skills",
            args.dry_run,
        )?);
        outcomes.push(migrate_one(
            &legacy_user_yaml_skills_dir(),
            &user_skills_dir(),
            "user-skill-definitions",
            args.dry_run,
        )?);
    }

    let payload = serde_json::json!({
        "dry_run": args.dry_run,
        "project_only": args.project_only,
        "scopes": outcomes.iter().map(outcome_to_json).collect::<Vec<_>>(),
    });
    print_value(payload, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::test_utils::EnvVarGuard;

    fn seed_installed_skill(project_root: &str, name: &str) {
        let mut registry = SkillRegistryStateV1::default();
        registry.installed.push(ResolvedSkillEntry {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            source: "local".to_string(),
            registry: "project".to_string(),
            integrity: "sha256:abc".to_string(),
            artifact: format!("{name}-1.0.0.tgz"),
            definition: None,
        });
        registry.defaults.push(SkillProjectConstraint {
            name: name.to_string(),
            version: None,
            source: Some("local".to_string()),
            registry: None,
            allow_prerelease: false,
        });
        save_skill_registry_state_if_changed(project_root, &registry).expect("save registry state");

        let mut lock = SkillLockStateV1::default();
        lock.entries.push(SkillLockEntry {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            source: "local".to_string(),
            integrity: "sha256:abc".to_string(),
            artifact: format!("{name}-1.0.0.tgz"),
            registry: Some("project".to_string()),
        });
        save_skill_lock_state_if_changed(project_root, &lock).expect("save lock state");

        let skill_dir = skill_install_root(project_root).join(name);
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(skill_dir.join("SKILL.md"), "---\nname: \"alpha\"\n---\n\nbody\n").expect("write skill file");
    }

    fn create_args(name: &str) -> SkillCreateArgs {
        SkillCreateArgs {
            name: name.to_string(),
            description: "A test skill".to_string(),
            prompt: Some("Do the thing.".to_string()),
            prompt_file: None,
            category: None,
            tags: Vec::new(),
            project: false,
            user: false,
            force: false,
        }
    }

    #[test]
    fn create_defaults_to_project_scope_and_loader_resolves_it() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");

        let mut args = create_args("authored-cli");
        args.tags = vec!["one".to_string(), " two ".to_string()];
        args.category = Some("review".to_string());
        handle_create(args, root, true).expect("create should succeed");

        let path = project_skills_dir(temp.path()).join("authored-cli.yaml");
        assert!(path.exists(), "project-scope create should write {}", path.display());
        assert!(!user_skills_dir().join("authored-cli.yaml").exists(), "default scope must not touch the user tier");

        let sources = load_skill_sources(temp.path(), None).expect("load sources");
        let resolved = resolve_skill("authored-cli", &sources).expect("resolve authored skill");
        assert_eq!(resolved.source.to_string(), "project");
        assert_eq!(resolved.definition.description, "A test skill");
        assert_eq!(resolved.definition.prompt.system.as_deref(), Some("Do the thing."));
        assert_eq!(resolved.definition.tags, vec!["one", "two"]);
    }

    #[test]
    fn create_user_scope_writes_to_user_skill_definitions_dir() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");

        let mut args = create_args("user-cli");
        args.user = true;
        handle_create(args, root, true).expect("user-scope create should succeed");

        let expected = home.path().join(".animus").join("config").join("skill_definitions").join("user-cli.yaml");
        assert!(expected.exists(), "user-scope create should write {}", expected.display());
        assert!(
            !project_skills_dir(temp.path()).join("user-cli.yaml").exists(),
            "user scope must not touch the project tier"
        );

        let sources = load_skill_sources(temp.path(), None).expect("load sources");
        let resolved = resolve_skill("user-cli", &sources).expect("resolve user-scoped skill");
        assert_eq!(resolved.source.to_string(), "user");
    }

    #[test]
    fn create_refuses_overwrite_without_force() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");

        handle_create(create_args("dup-cli"), root, true).expect("first create");
        let error = handle_create(create_args("dup-cli"), root, true).expect_err("second create should fail");
        let message = error.to_string();
        assert!(message.contains("already exists"), "got: {message}");
        assert!(message.contains("--force"), "refusal should suggest --force, got: {message}");
        assert!(!message.contains("overwrite=true"), "CLI error must not leak the MCP remedy: {message}");

        let mut forced = create_args("dup-cli");
        forced.description = "Replaced".to_string();
        forced.force = true;
        handle_create(forced, root, true).expect("forced create should succeed");

        let sources = load_skill_sources(temp.path(), None).expect("load sources");
        let resolved = resolve_skill("dup-cli", &sources).expect("resolve");
        assert_eq!(resolved.definition.description, "Replaced");
    }

    #[test]
    fn create_validates_slug_prompt_and_category() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");

        for bad in ["../escape", "Has Space", "UPPER", "a/b"] {
            let mut args = create_args(bad);
            args.name = bad.to_string();
            assert!(handle_create(args, root, true).is_err(), "slug {bad:?} should be rejected");
        }

        let mut no_prompt = create_args("no-prompt");
        no_prompt.prompt = None;
        let error = handle_create(no_prompt, root, true).expect_err("missing prompt should fail");
        assert!(error.to_string().contains("--prompt"));

        let mut blank_prompt = create_args("blank-prompt");
        blank_prompt.prompt = Some("   ".to_string());
        assert!(handle_create(blank_prompt, root, true).is_err(), "blank prompt should be rejected");

        let mut bad_category = create_args("bad-category");
        bad_category.category = Some("bogus".to_string());
        let error = handle_create(bad_category, root, true).expect_err("unknown category should fail");
        assert!(error.to_string().contains("unknown category 'bogus'"));
    }

    #[test]
    fn create_reads_prompt_from_file() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");

        let prompt_path = temp.path().join("prompt.md");
        fs::write(&prompt_path, "Prompt from file.\n").expect("write prompt file");
        let mut args = create_args("from-file");
        args.prompt = None;
        args.prompt_file = Some(prompt_path);
        handle_create(args, root, true).expect("create from --prompt-file");

        let sources = load_skill_sources(temp.path(), None).expect("load sources");
        let resolved = resolve_skill("from-file", &sources).expect("resolve");
        assert_eq!(resolved.definition.prompt.system.as_deref(), Some("Prompt from file."));
    }

    #[test]
    fn uninstall_removes_state_entries_and_materialized_files() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");
        seed_installed_skill(root, "alpha");

        let args = SkillUninstallArgs { name: "alpha".to_string(), source: None, dry_run: false };
        handle_uninstall(args, root, true).expect("uninstall should succeed");

        let registry = load_skill_registry_state(root).expect("load registry state");
        assert!(registry.installed.is_empty(), "installed entry should be removed");
        assert!(registry.defaults.is_empty(), "project default should be removed");
        let lock = load_skill_lock_state(root).expect("load lock state");
        assert!(lock.entries.is_empty(), "lock entry should be removed");
        assert!(!skill_install_root(root).join("alpha").exists(), "skill dir should be removed");
    }

    #[test]
    fn uninstall_dry_run_leaves_state_and_files_in_place() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");
        seed_installed_skill(root, "alpha");

        let args = SkillUninstallArgs { name: "alpha".to_string(), source: None, dry_run: true };
        handle_uninstall(args, root, true).expect("dry-run uninstall should succeed");

        let registry = load_skill_registry_state(root).expect("load registry state");
        assert_eq!(registry.installed.len(), 1, "installed entry should remain");
        assert_eq!(registry.defaults.len(), 1, "project default should remain");
        let lock = load_skill_lock_state(root).expect("load lock state");
        assert_eq!(lock.entries.len(), 1, "lock entry should remain");
        assert!(skill_install_root(root).join("alpha").exists(), "skill dir should remain");
    }

    #[test]
    fn uninstall_unknown_skill_is_an_error() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");

        let args = SkillUninstallArgs { name: "missing".to_string(), source: None, dry_run: false };
        let error = handle_uninstall(args, root, true).expect_err("uninstall should fail");
        assert!(error.to_string().contains("skill not installed"));
    }

    #[test]
    fn uninstall_with_source_filter_keeps_other_sources_and_drops_stale_files() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");
        seed_installed_skill(root, "alpha");

        let mut registry = load_skill_registry_state(root).expect("load registry state");
        registry.installed.push(ResolvedSkillEntry {
            name: "alpha".to_string(),
            version: "1.1.0".to_string(),
            source: "github".to_string(),
            registry: "project".to_string(),
            integrity: "sha256:def".to_string(),
            artifact: "alpha-1.1.0.tgz".to_string(),
            definition: None,
        });
        save_skill_registry_state_if_changed(root, &registry).expect("save registry state");

        let args = SkillUninstallArgs { name: "alpha".to_string(), source: Some("local".to_string()), dry_run: false };
        handle_uninstall(args, root, true).expect("uninstall should succeed");

        let registry = load_skill_registry_state(root).expect("load registry state");
        assert_eq!(registry.installed.len(), 1, "github entry should remain");
        assert_eq!(registry.installed[0].source, "github");
        assert_eq!(registry.defaults.len(), 1, "project default should remain while another source is installed");
        assert!(
            !skill_install_root(root).join("alpha").exists(),
            "shared skill dir holds the removed definition and the remaining source has no snapshot, so it must go"
        );
    }

    #[test]
    fn uninstall_with_source_filter_rewrites_skill_file_from_remaining_snapshot() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");
        seed_installed_skill(root, "alpha");

        let definition: SkillDefinition =
            serde_json::from_value(serde_json::json!({"name": "alpha", "description": "from github"}))
                .expect("definition should deserialize");
        let mut registry = load_skill_registry_state(root).expect("load registry state");
        registry.installed.push(ResolvedSkillEntry {
            name: "alpha".to_string(),
            version: "1.1.0".to_string(),
            source: "github".to_string(),
            registry: "project".to_string(),
            integrity: "sha256:def".to_string(),
            artifact: "alpha-1.1.0.tgz".to_string(),
            definition: Some(definition),
        });
        save_skill_registry_state_if_changed(root, &registry).expect("save registry state");

        let args = SkillUninstallArgs { name: "alpha".to_string(), source: Some("local".to_string()), dry_run: false };
        handle_uninstall(args, root, true).expect("uninstall should succeed");

        let skill_file = skill_install_root(root).join("alpha").join("SKILL.md");
        let content = fs::read_to_string(&skill_file).expect("skill file should remain");
        assert!(content.contains("from github"), "skill file should be rewritten from the remaining snapshot");
    }

    #[test]
    fn uninstall_with_unmatched_source_filter_keeps_files_and_default() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");
        seed_installed_skill(root, "alpha");

        let args = SkillUninstallArgs { name: "alpha".to_string(), source: Some("github".to_string()), dry_run: false };
        let error = handle_uninstall(args, root, true).expect_err("uninstall should fail");
        assert!(error.to_string().contains("skill not installed from source 'github'"));

        let registry = load_skill_registry_state(root).expect("load registry state");
        assert_eq!(registry.installed.len(), 1, "installed entry should remain");
        assert_eq!(registry.defaults.len(), 1, "project default should remain");
        assert!(skill_install_root(root).join("alpha").exists(), "skill dir should remain");
    }

    #[test]
    fn uninstall_rejects_path_like_skill_names() {
        let _guard = crate::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_str().expect("utf8 path");

        let args = SkillUninstallArgs { name: "../escape".to_string(), source: None, dry_run: false };
        let error = handle_uninstall(args, root, true).expect_err("uninstall should fail");
        assert!(error.to_string().contains("invalid skill name"));
    }
}

pub(crate) async fn handle_skill(command: SkillCommand, project_root: &str, json: bool) -> Result<()> {
    match command {
        SkillCommand::Create(args) => handle_create(args, project_root, json),
        SkillCommand::Search(args) => handle_search(args, project_root, json),
        SkillCommand::Install(args) => handle_install(args, project_root, json),
        SkillCommand::List(args) => handle_list(args, project_root, json),
        SkillCommand::Info(args) => handle_show(args, project_root, json),
        SkillCommand::Update(args) => handle_update(args, project_root, json),
        SkillCommand::Uninstall(args) => handle_uninstall(args, project_root, json),
        SkillCommand::Publish(args) => handle_publish(args, project_root, json),
        SkillCommand::Registry { command } => match command {
            SkillRegistryCommand::Add(args) => handle_registry_add(args, project_root, json),
            SkillRegistryCommand::Remove(args) => handle_registry_remove(args, project_root, json),
            SkillRegistryCommand::List => handle_registry_list(project_root, json),
        },
        SkillCommand::MigrateFromAo(args) => handle_migrate_from_ao(args, project_root, json),
    }
}
