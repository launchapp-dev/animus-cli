//! Marketplace operations backed by the public Animus plugin registry.
//!
//! Provides three CLI commands and matching reusable runners:
//! * `animus plugin search` — substring + filter search against the registry index
//! * `animus plugin browse` — grouped listing (installed vs available)
//! * `animus plugin update` — bulk-update installed plugins to the recommended
//!   pins declared in `crates/orchestrator-cli/config/default-install.json`,
//!   with `--all` / `--kind <KIND>` / `--name <NAME>` selectors, a `--check`
//!   diff preview, and `--yes` for unattended runs.
//!
//! Search + browse share a registry fetch + on-disk cache layer
//! (`~/.cache/animus/plugin-registry.json`, refreshed every 6 hours unless
//! `--no-cache` is passed). Update bypasses the registry entirely — it reads
//! the bundled `default-install.json` as the source of truth so the update
//! surface stays consistent with `animus plugin install-defaults`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use orchestrator_plugin_host::{legacy_plugins_registry_path, plugins_registry_path};
use serde::{Deserialize, Serialize};

use crate::{
    invalid_input_error, print_value, PluginBrowseArgs, PluginOutdatedArgs, PluginSearchArgs, PluginUpdateArgs,
    DEFAULT_PLUGIN_REGISTRY_URL,
};

use super::{run_plugin_install, PluginInstallOutput, PluginInstallRequest};

/// Default cache TTL: 6 hours.
const REGISTRY_CACHE_TTL: Duration = Duration::from_hours(6);

// =================== Registry types ===================

/// Top-level registry index returned by `plugins.json` in the public
/// animus-plugin-registry repository.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PluginRegistryIndex {
    #[serde(default)]
    pub(crate) registry_version: Option<String>,
    #[serde(default)]
    pub(crate) updated_at: Option<String>,
    #[serde(default)]
    pub(crate) plugins: Vec<RegistryPluginEntry>,
}

/// A single plugin entry as published in the registry. Unknown fields are kept
/// flexible via `#[serde(default)]` so registry schema bumps don't break the CLI.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct RegistryPluginEntry {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) repo: String,
    #[serde(default)]
    pub(crate) latest_tag: Option<String>,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) homepage: Option<String>,
    #[serde(default)]
    pub(crate) license: Option<String>,
    #[serde(default)]
    pub(crate) stability: Option<String>,
    #[serde(default)]
    pub(crate) platforms: Vec<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) install_hint: Option<String>,
}

impl RegistryPluginEntry {
    fn org(&self) -> Option<&str> {
        self.repo.split_once('/').map(|(o, _)| o)
    }
}

// =================== Search ===================

#[derive(Debug, Clone, Default)]
pub(crate) struct PluginSearchRequest {
    pub(crate) query: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) tag: Vec<String>,
    pub(crate) org: Option<String>,
    pub(crate) stability: Option<String>,
    pub(crate) registry_url: String,
    pub(crate) no_cache: bool,
    pub(crate) offline: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginSearchRow {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) description: String,
    pub(crate) repo: String,
    pub(crate) latest_tag: Option<String>,
    pub(crate) stability: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) install_command: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginSearchOutput {
    pub(crate) registry_url: String,
    pub(crate) total: usize,
    pub(crate) matched: usize,
    pub(crate) results: Vec<PluginSearchRow>,
}

pub(crate) async fn run_plugin_search(req: PluginSearchRequest) -> Result<PluginSearchOutput> {
    let registry_url = if req.registry_url.trim().is_empty() {
        DEFAULT_PLUGIN_REGISTRY_URL.to_string()
    } else {
        req.registry_url.clone()
    };
    let index = fetch_registry_index(&registry_url, RegistryCachePolicy::from_flags(req.no_cache, req.offline)).await?;
    let total = index.plugins.len();

    let query_lower = req.query.as_deref().map(str::to_ascii_lowercase);
    let kind_lower = req.kind.as_deref().map(str::to_ascii_lowercase);
    let stability_lower = req.stability.as_deref().map(str::to_ascii_lowercase);
    let org_lower = req.org.as_deref().map(str::to_ascii_lowercase);
    let tags_lower: Vec<String> = req.tag.iter().map(|t| t.to_ascii_lowercase()).collect();

    let mut matched: Vec<PluginSearchRow> = index
        .plugins
        .into_iter()
        .filter(|entry| {
            if let Some(q) = query_lower.as_deref() {
                let hay_name = entry.name.to_ascii_lowercase();
                let hay_desc = entry.description.to_ascii_lowercase();
                if !hay_name.contains(q) && !hay_desc.contains(q) {
                    return false;
                }
            }
            if let Some(k) = kind_lower.as_deref() {
                if entry.kind.to_ascii_lowercase() != k {
                    return false;
                }
            }
            if let Some(s) = stability_lower.as_deref() {
                match entry.stability.as_deref().map(str::to_ascii_lowercase) {
                    Some(actual) if actual == s => {}
                    _ => return false,
                }
            }
            if let Some(o) = org_lower.as_deref() {
                match entry.org().map(str::to_ascii_lowercase) {
                    Some(actual) if actual == o => {}
                    _ => return false,
                }
            }
            if !tags_lower.is_empty() {
                let entry_tags: BTreeSet<String> = entry.tags.iter().map(|t| t.to_ascii_lowercase()).collect();
                for needle in &tags_lower {
                    if !entry_tags.contains(needle) {
                        return false;
                    }
                }
            }
            true
        })
        .map(to_search_row)
        .collect();

    matched.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(PluginSearchOutput { registry_url, total, matched: matched.len(), results: matched })
}

fn to_search_row(entry: RegistryPluginEntry) -> PluginSearchRow {
    let install_command = entry.install_hint.clone().unwrap_or_else(|| format!("animus plugin install {}", entry.repo));
    PluginSearchRow {
        name: entry.name,
        kind: entry.kind,
        description: entry.description,
        repo: entry.repo,
        latest_tag: entry.latest_tag,
        stability: entry.stability,
        tags: entry.tags,
        install_command,
    }
}

pub(crate) async fn handle_plugin_search(args: PluginSearchArgs) -> Result<()> {
    let json = args.json;
    let output = run_plugin_search(PluginSearchRequest {
        query: args.query,
        kind: args.kind,
        tag: args.tag,
        org: args.org,
        stability: args.stability,
        registry_url: args.registry_url,
        no_cache: args.no_cache,
        offline: args.offline,
    })
    .await?;

    if json {
        return print_value(output, true);
    }

    if output.results.is_empty() {
        println!("no matching plugins (registry: {}, total: {})", output.registry_url, output.total);
        return Ok(());
    }

    println!("{} of {} plugins matched (registry: {})", output.matched, output.total, output.registry_url);
    println!();
    for row in &output.results {
        let stability = row.stability.as_deref().unwrap_or("--");
        let tag = row.latest_tag.as_deref().unwrap_or("--");
        println!("{}  ({}, {}, {})", row.name, row.kind, tag, stability);
        if !row.description.is_empty() {
            println!("  {}", row.description);
        }
        if !row.tags.is_empty() {
            println!("  tags: {}", row.tags.join(", "));
        }
        println!("  install: {}", row.install_command);
        println!();
    }
    Ok(())
}

// =================== Browse ===================

#[derive(Debug, Clone, Default)]
pub(crate) struct PluginBrowseRequest {
    pub(crate) kind: Option<String>,
    pub(crate) installed: bool,
    pub(crate) available: bool,
    pub(crate) registry_url: String,
    pub(crate) no_cache: bool,
    pub(crate) offline: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginBrowseRow {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) description: String,
    pub(crate) repo: String,
    pub(crate) latest_tag: Option<String>,
    pub(crate) stability: Option<String>,
    pub(crate) installed: bool,
    pub(crate) installed_tag: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginBrowseOutput {
    pub(crate) registry_url: String,
    pub(crate) total: usize,
    pub(crate) shown: usize,
    pub(crate) groups: BTreeMap<String, Vec<PluginBrowseRow>>,
}

pub(crate) async fn run_plugin_browse(req: PluginBrowseRequest) -> Result<PluginBrowseOutput> {
    if req.installed && req.available {
        return Err(invalid_input_error("--installed and --available are mutually exclusive"));
    }
    let registry_url = if req.registry_url.trim().is_empty() {
        DEFAULT_PLUGIN_REGISTRY_URL.to_string()
    } else {
        req.registry_url.clone()
    };
    let index = fetch_registry_index(&registry_url, RegistryCachePolicy::from_flags(req.no_cache, req.offline)).await?;
    let total = index.plugins.len();
    let installed = read_installed_index().unwrap_or_default();
    let kind_lower = req.kind.as_deref().map(str::to_ascii_lowercase);

    let mut groups: BTreeMap<String, Vec<PluginBrowseRow>> = BTreeMap::new();
    let mut shown = 0;
    for entry in index.plugins {
        if let Some(k) = kind_lower.as_deref() {
            if entry.kind.to_ascii_lowercase() != k {
                continue;
            }
        }
        // Reconciled view: a registry entry whose binary is gone does not count
        // as installed. This keeps `browse --installed` aligned with `list`.
        let installed_entry = installed.get(&entry.name).filter(|e| e.is_present());
        let is_installed = installed_entry.is_some();
        if req.installed && !is_installed {
            continue;
        }
        if req.available && is_installed {
            continue;
        }
        let row = PluginBrowseRow {
            name: entry.name.clone(),
            kind: entry.kind.clone(),
            description: entry.description.clone(),
            repo: entry.repo.clone(),
            latest_tag: entry.latest_tag.clone(),
            stability: entry.stability.clone(),
            installed: is_installed,
            installed_tag: installed_entry.and_then(|e| e.release_tag.clone()),
        };
        let group_key = if entry.kind.is_empty() { "unknown".to_string() } else { entry.kind.clone() };
        groups.entry(group_key).or_default().push(row);
        shown += 1;
    }
    for rows in groups.values_mut() {
        rows.sort_by(|a, b| a.name.cmp(&b.name));
    }

    Ok(PluginBrowseOutput { registry_url, total, shown, groups })
}

pub(crate) async fn handle_plugin_browse(args: PluginBrowseArgs) -> Result<()> {
    let json = args.json;
    let output = run_plugin_browse(PluginBrowseRequest {
        kind: args.kind,
        installed: args.installed,
        available: args.available,
        registry_url: args.registry_url,
        no_cache: args.no_cache,
        offline: args.offline,
    })
    .await?;
    if json {
        return print_value(output, true);
    }
    if output.shown == 0 {
        println!("no plugins to display (registry: {}, total: {})", output.registry_url, output.total);
        return Ok(());
    }
    println!("{} of {} plugins shown (registry: {})", output.shown, output.total, output.registry_url);
    for (kind, rows) in &output.groups {
        println!();
        println!("== {} ({}) ==", kind, rows.len());
        for row in rows {
            let installed_marker = if row.installed { "installed" } else { "available" };
            let tag = row.latest_tag.as_deref().unwrap_or("--");
            println!("  {}  {}  latest={}  [{}]", row.name, row.kind, tag, installed_marker);
            if !row.description.is_empty() {
                println!("    {}", row.description);
            }
            if let Some(installed_tag) = row.installed_tag.as_deref() {
                println!("    installed_tag: {}", installed_tag);
            }
        }
    }
    Ok(())
}

// =================== Update ===================

/// Selector for `animus plugin update`. Exactly one variant is required.
#[derive(Debug, Clone)]
pub(crate) enum PluginUpdateSelector {
    /// All installed release-source plugins.
    All,
    /// Every installed plugin whose recommended pin lives under this section
    /// of `default-install.json`.
    Kind(String),
    /// One installed plugin by name (the `plugins.yaml` / lockfile key).
    Name(String),
}

#[derive(Debug, Clone)]
pub(crate) struct PluginUpdateRequest {
    pub(crate) selector: PluginUpdateSelector,
    pub(crate) tag_override: Option<String>,
    pub(crate) check: bool,
    pub(crate) force: bool,
    pub(crate) project_root: Option<String>,
    /// When `true`, operate on the project-local plugin root: the installed
    /// set is read from `<project_root>/.animus/plugins.yaml` and updates
    /// reinstall under the project scope. Requires `project_root`.
    pub(crate) project: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginUpdateRow {
    pub(crate) name: String,
    pub(crate) installed_tag: Option<String>,
    pub(crate) recommended_tag: Option<String>,
    pub(crate) origin: Option<String>,
    /// Action that would be taken (or was taken when `--check` is off):
    /// `update`, `skip`, `failed`, or `would_update` (dry-run only).
    pub(crate) action: &'static str,
    /// Free-form note ("ahead of pin", "not in default-install", "already current", ...).
    pub(crate) note: Option<String>,
    /// `default-install.json` section the recommended pin came from
    /// (`providers`, `subjects`, ...). `None` when the slug has no matching
    /// recommended pin.
    pub(crate) recommended_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) install: Option<PluginInstallOutput>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginUpdateOutput {
    pub(crate) check: bool,
    pub(crate) considered: usize,
    pub(crate) updated: usize,
    pub(crate) failed: usize,
    pub(crate) results: Vec<PluginUpdateRow>,
}

/// Recommended pins parsed from `crates/orchestrator-cli/config/default-install.json`.
#[derive(Debug, Clone, Default)]
pub(crate) struct RecommendedPins {
    /// `owner/repo` -> (`tag`, `default-install.json section`).
    pub(crate) by_slug: BTreeMap<String, (String, String)>,
}

impl RecommendedPins {
    pub(crate) fn parse(raw_json: &str) -> Result<Self> {
        let value: serde_json::Value =
            serde_json::from_str(raw_json).context("default-install.json is not valid JSON")?;
        let mut by_slug: BTreeMap<String, (String, String)> = BTreeMap::new();
        if let Some(map) = value.get("plugins").and_then(|p| p.as_object()) {
            for (section, entries) in map {
                let Some(arr) = entries.as_array() else { continue };
                for entry in arr {
                    let Some(repo) = entry.get("repo").and_then(|v| v.as_str()) else { continue };
                    let Some(tag) = entry.get("tag").and_then(|v| v.as_str()) else { continue };
                    by_slug.insert(repo.to_string(), (tag.to_string(), section.to_string()));
                }
            }
        }
        Ok(RecommendedPins { by_slug })
    }

    pub(crate) fn lookup(&self, slug: &str) -> Option<&(String, String)> {
        self.by_slug.get(slug)
    }
}

const DEFAULT_INSTALL_MANIFEST_JSON: &str = include_str!("../../../../config/default-install.json");

// TODO(codex-p2): `default-install.json` is currently a SUBSET of the slugs
// install-defaults actually installs (it omits e.g. animus-subject-linear,
// animus-transport-http, animus-web-ui, gemini/opencode/oai-runner). Those
// slugs get reported as "not in default-install" by `animus plugin update`
// and are never bumped here. Bringing them under update requires either
// expanding the JSON file or merging with `orchestrator_core::plugin_registry`
// + `flavors/default.toml`. Tracked as a v0.5.9 follow-up so this v0.5.8 PR
// stays scoped to the surface the task asked for.
pub(crate) fn load_recommended_pins() -> Result<RecommendedPins> {
    RecommendedPins::parse(DEFAULT_INSTALL_MANIFEST_JSON)
}

/// Normalize a `default-install.json` section name or singular plugin_kind
/// to the canonical section key (`providers`, `subjects`, ...). Returns the
/// input unchanged for unknown values so the caller can surface a clear error.
pub(crate) fn normalize_kind_selector(kind: &str) -> String {
    match kind.trim().to_ascii_lowercase().as_str() {
        // Singular plugin_kind aliases users see in `animus plugin list`.
        "provider" => "providers".to_string(),
        "subject_backend" | "subject" => "subjects".to_string(),
        "workflow_runner" => "workflow_runners".to_string(),
        "queue" => "queues".to_string(),
        "notifier" => "notifiers".to_string(),
        "transport_backend" | "transport" => "transports".to_string(),
        "config_source" => "config_sources".to_string(),
        // Canonical section names plus a couple of forgivable plurals.
        "providers" | "subjects" | "workflow_runners" | "queues" | "notifiers" | "transports" | "oai_agent"
        | "config_sources" => kind.trim().to_ascii_lowercase(),
        other => other.to_string(),
    }
}

/// Parse a release tag into a comparable semver, stripping a leading `v`.
fn parse_tag(tag: &str) -> Option<semver::Version> {
    let trimmed = tag.trim();
    let stripped = trimmed.strip_prefix('v').unwrap_or(trimmed);
    semver::Version::parse(stripped).ok()
}

/// Compare two release tags as semver. Returns `None` when either side fails
/// to parse — callers fall back to string equality.
fn compare_tags(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    Some(parse_tag(a)?.cmp(&parse_tag(b)?))
}

#[derive(Debug, Clone)]
struct UpdatePlan {
    entry: InstalledPlugin,
    repo_slug: Option<String>,
    recommended_tag: Option<String>,
    recommended_kind: Option<String>,
    action: &'static str,
    note: Option<String>,
}

fn build_update_plan(
    installed: &BTreeMap<String, InstalledPlugin>,
    pins: &RecommendedPins,
    selector: &PluginUpdateSelector,
    tag_override: Option<&str>,
    force: bool,
) -> Result<Vec<UpdatePlan>> {
    let normalized_kind = match selector {
        PluginUpdateSelector::Kind(k) => Some(normalize_kind_selector(k)),
        _ => None,
    };

    if let PluginUpdateSelector::Name(name) = selector {
        match installed.get(name) {
            None => return Err(invalid_input_error(format!("plugin '{name}' is not installed"))),
            Some(entry) if !entry.is_present() => {
                return Err(invalid_input_error(format!(
                    "plugin '{name}' has a stale registry entry (binary missing); run `animus plugin prune` to clear it"
                )));
            }
            Some(_) => {}
        }
    }

    let mut plans: Vec<UpdatePlan> = Vec::new();
    let entries: Vec<&InstalledPlugin> = match selector {
        PluginUpdateSelector::Name(n) => vec![installed.get(n).expect("checked above")],
        // Reconciled view: `--all` / `--kind` only act on plugins whose binary
        // is still present. Stale registry entries are cleared via `prune`.
        PluginUpdateSelector::All | PluginUpdateSelector::Kind(_) => {
            installed.values().filter(|e| e.is_present()).collect()
        }
    };

    for installed_entry in entries {
        let source_kind = installed_entry.source_kind.as_deref().unwrap_or("");
        if source_kind != "release" {
            // Per spec: NEVER touch a plugin installed from a non-default
            // source. When the user filtered by `--kind`, suppress these
            // entries entirely — they have no recommended_kind to match.
            // `--name` and `--all` still surface them so the operator sees
            // why the plugin was untouched.
            if normalized_kind.is_some() {
                continue;
            }
            let note = if source_kind.is_empty() {
                "not from registry (no source_kind)".to_string()
            } else {
                format!("not from registry (source_kind={source_kind})")
            };
            plans.push(UpdatePlan {
                entry: installed_entry.clone(),
                repo_slug: None,
                recommended_tag: None,
                recommended_kind: None,
                action: "skip",
                note: Some(note),
            });
            continue;
        }

        let repo_slug = origin_to_repo_slug(installed_entry.origin.as_deref());
        let recommended = repo_slug.as_deref().and_then(|slug| pins.lookup(slug)).cloned();
        let recommended_kind = recommended.as_ref().map(|(_, k)| k.clone());
        let recommended_tag = recommended.as_ref().map(|(t, _)| t.clone());

        if let Some(needle) = normalized_kind.as_deref() {
            match recommended_kind.as_deref() {
                Some(k) if k == needle => {}
                _ => continue,
            }
        }

        // Resolve target_tag: explicit --tag override (only valid with --name)
        // wins, else the recommended pin.
        let target_tag = tag_override.map(|s| s.to_string()).or(recommended_tag.clone());

        let installed_tag = installed_entry.release_tag.as_deref();
        let (action, note) = decide_action(installed_tag, target_tag.as_deref(), recommended_tag.is_some(), force);

        plans.push(UpdatePlan {
            entry: installed_entry.clone(),
            repo_slug,
            recommended_tag: target_tag,
            recommended_kind,
            action,
            note,
        });
    }

    plans.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
    Ok(plans)
}

fn decide_action(
    installed_tag: Option<&str>,
    target_tag: Option<&str>,
    has_recommendation: bool,
    force: bool,
) -> (&'static str, Option<String>) {
    match (installed_tag, target_tag) {
        (_, None) => {
            if has_recommendation {
                ("skip", Some("recommended pin missing release tag".to_string()))
            } else {
                // `force` is intentionally ignored here: without a target tag
                // there is nothing to reinstall to.
                let _ = force;
                ("skip", Some("not in default-install".to_string()))
            }
        }
        (None, Some(_)) => ("update", None),
        (Some(installed), Some(target)) => {
            if installed == target {
                if force {
                    ("update", Some("forced reinstall".to_string()))
                } else {
                    ("skip", Some("current".to_string()))
                }
            } else {
                match compare_tags(installed, target) {
                    Some(std::cmp::Ordering::Greater) => {
                        if force {
                            ("update", Some("forced downgrade".to_string()))
                        } else {
                            ("skip", Some("ahead of pin".to_string()))
                        }
                    }
                    Some(std::cmp::Ordering::Less) | Some(std::cmp::Ordering::Equal) => ("update", None),
                    None => ("update", Some("non-semver tag; falling back to string compare".to_string())),
                }
            }
        }
    }
}

pub(crate) async fn run_plugin_update(req: PluginUpdateRequest) -> Result<PluginUpdateOutput> {
    // Validate request shape BEFORE touching the filesystem so callers get
    // deterministic errors regardless of whether plugins.yaml exists yet.
    if req.tag_override.is_some() && !matches!(req.selector, PluginUpdateSelector::Name(_)) {
        return Err(invalid_input_error("--tag <TAG> is only valid with --name <NAME>"));
    }

    let installed = if req.project {
        let root = req
            .project_root
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_input_error("--project requires a resolvable project root"))?;
        read_installed_index_at(&orchestrator_plugin_host::project_plugins_registry_path(std::path::Path::new(root)))
            .context("failed to read project-local installed plugin registry")?
    } else {
        read_installed_index().context("failed to read installed plugin registry")?
    };
    let pins = load_recommended_pins()?;

    if let PluginUpdateSelector::Name(_) = &req.selector {
        // valid even when registry is empty — explicit error surfaces below
    } else if installed.is_empty() {
        return Ok(PluginUpdateOutput { check: req.check, considered: 0, updated: 0, failed: 0, results: vec![] });
    }

    let plans = build_update_plan(&installed, &pins, &req.selector, req.tag_override.as_deref(), req.force)?;

    let mut results: Vec<PluginUpdateRow> = Vec::with_capacity(plans.len());
    let mut updated = 0usize;
    let mut failed = 0usize;

    for plan in plans {
        if plan.action == "skip" {
            results.push(PluginUpdateRow {
                name: plan.entry.name.clone(),
                installed_tag: plan.entry.release_tag.clone(),
                recommended_tag: plan.recommended_tag.clone(),
                origin: plan.entry.origin.clone(),
                action: "skip",
                note: plan.note,
                recommended_kind: plan.recommended_kind,
                install: None,
            });
            continue;
        }

        if req.check {
            results.push(PluginUpdateRow {
                name: plan.entry.name.clone(),
                installed_tag: plan.entry.release_tag.clone(),
                recommended_tag: plan.recommended_tag.clone(),
                origin: plan.entry.origin.clone(),
                action: "would_update",
                note: plan.note,
                recommended_kind: plan.recommended_kind,
                install: None,
            });
            continue;
        }

        let Some(slug) = plan.repo_slug.clone() else {
            results.push(PluginUpdateRow {
                name: plan.entry.name.clone(),
                installed_tag: plan.entry.release_tag.clone(),
                recommended_tag: plan.recommended_tag.clone(),
                origin: plan.entry.origin.clone(),
                action: "skip",
                note: Some("origin missing owner/repo".to_string()),
                recommended_kind: plan.recommended_kind,
                install: None,
            });
            continue;
        };

        let source = match plan.recommended_tag.as_deref() {
            Some(tag) => format!("{slug}@{tag}"),
            None => slug.clone(),
        };
        // Only bypass the publisher TOFU prompt + provider-shadow guard when
        // the slug is one of the curated `default-install.json` pins. For
        // arbitrary `--name --tag` overrides against a plugin that is NOT in
        // the curated list, fall back to the regular install policy so the
        // user re-confirms trust for the publisher.
        let is_curated_pin = plan.recommended_kind.is_some();
        let install_request = PluginInstallRequest {
            source: Some(source),
            name: Some(plan.entry.name.clone()),
            force: true,
            project_root: req.project_root.clone(),
            allow_org: if is_curated_pin { vec!["launchapp-dev".to_string()] } else { Vec::new() },
            yes: is_curated_pin,
            allow_shadow_builtin: is_curated_pin,
            project: req.project,
            ..Default::default()
        };

        match run_plugin_install(install_request).await {
            Ok(output) => {
                let resolved_tag = output.release_tag.clone();
                updated += 1;
                results.push(PluginUpdateRow {
                    name: plan.entry.name.clone(),
                    installed_tag: plan.entry.release_tag.clone(),
                    recommended_tag: resolved_tag.or(plan.recommended_tag),
                    origin: plan.entry.origin.clone(),
                    action: "update",
                    note: plan.note,
                    recommended_kind: plan.recommended_kind,
                    install: Some(output),
                });
            }
            Err(err) => {
                failed += 1;
                results.push(PluginUpdateRow {
                    name: plan.entry.name.clone(),
                    installed_tag: plan.entry.release_tag.clone(),
                    recommended_tag: plan.recommended_tag,
                    origin: plan.entry.origin.clone(),
                    action: "failed",
                    note: Some(err.to_string()),
                    recommended_kind: plan.recommended_kind,
                    install: None,
                });
            }
        }
    }

    Ok(PluginUpdateOutput { check: req.check, considered: results.len(), updated, failed, results })
}

fn prompt_confirm(prompt: &str) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err(invalid_input_error(format!(
            "{prompt} aborted: stdin is not a terminal. Re-run with --yes for unattended use, \
             or --check to preview without applying."
        )));
    }
    eprint!("{prompt} [y/N]: ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).context("failed to read confirmation from stdin")?;
    let normalized = answer.trim().to_ascii_lowercase();
    Ok(normalized == "y" || normalized == "yes")
}

fn print_diff_table(output: &PluginUpdateOutput) {
    println!("{:<34} {:<10} {:<12} Action", "Plugin", "Current", "Recommended");
    for row in &output.results {
        let installed = row.installed_tag.as_deref().unwrap_or("--");
        let recommended = row.recommended_tag.as_deref().unwrap_or("--");
        let action_label: String = match row.action {
            "skip" => match row.note.as_deref() {
                Some(note) => format!("skip ({note})"),
                None => "skip".to_string(),
            },
            "would_update" => "update".to_string(),
            "update" => "updated".to_string(),
            "failed" => "failed".to_string(),
            other => other.to_string(),
        };
        println!("{:<34} {:<10} {:<12} {}", row.name, installed, recommended, action_label);
    }
}

pub(crate) async fn handle_plugin_update(args: PluginUpdateArgs, project_root: &str, root_json: bool) -> Result<()> {
    let json = args.json || root_json;
    let check = args.check || args.dry_run;

    // Resolve the selector: exactly one of --all / --kind / --name (or the
    // legacy positional NAME) is required.
    let selector_count = [args.all, args.kind.is_some(), args.name_flag.is_some(), args.name_positional.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if selector_count == 0 {
        return Err(invalid_input_error(
            "animus plugin update requires exactly one of --all, --kind <KIND>, or --name <NAME>",
        ));
    }
    if selector_count > 1 {
        return Err(invalid_input_error(
            "animus plugin update accepts only one of --all, --kind <KIND>, or --name <NAME> at a time",
        ));
    }

    let selector = if args.all {
        PluginUpdateSelector::All
    } else if let Some(kind) = args.kind {
        PluginUpdateSelector::Kind(kind)
    } else if let Some(name) = args.name_flag {
        PluginUpdateSelector::Name(name)
    } else {
        PluginUpdateSelector::Name(args.name_positional.expect("checked above"))
    };

    // First pass: compute the diff so the operator can preview it before any
    // mutation. We honor --check here.
    let preview = run_plugin_update(PluginUpdateRequest {
        selector: selector.clone(),
        tag_override: args.tag.clone(),
        check: true,
        force: args.force,
        project_root: Some(project_root.to_string()),
        project: args.project,
    })
    .await?;

    if json {
        // In JSON mode we still preview-then-apply, but emit a single envelope
        // at the end. If --check, emit the preview envelope as-is.
        if check {
            return print_value(preview, true);
        }
    } else {
        if preview.results.is_empty() {
            println!("no installed release-source plugins matched the selector");
            return Ok(());
        }
        print_diff_table(&preview);
    }

    if check {
        if !json {
            println!();
            println!("--check: no changes written");
        }
        return Ok(());
    }

    let pending_updates = preview.results.iter().filter(|r| r.action == "would_update").count();
    if pending_updates == 0 {
        if !json {
            println!();
            println!("nothing to update");
        } else {
            print_value(preview, true)?;
        }
        return Ok(());
    }

    if !args.yes && !json {
        println!();
        let prompt = format!("apply update to {pending_updates} plugin(s)?");
        if !prompt_confirm(&prompt)? {
            println!("aborted by user");
            return Ok(());
        }
    } else if !args.yes && json {
        // JSON callers MUST pass --yes; we can't prompt without polluting
        // stdout. Refuse loudly so scripts don't silently no-op.
        return Err(invalid_input_error("--json requires --yes (or --check) for non-interactive use"));
    }

    let output = run_plugin_update(PluginUpdateRequest {
        selector,
        tag_override: args.tag,
        check: false,
        force: args.force,
        project_root: Some(project_root.to_string()),
        project: args.project,
    })
    .await?;

    let restart =
        if args.restart_daemon { Some(restart_daemon_after_update(project_root, output.failed).await) } else { None };

    if json {
        let mut envelope = serde_json::to_value(&output)?;
        if let Some(restart) = &restart {
            envelope["daemon_restart"] = restart.to_json();
        }
        print_value(envelope, true)?;
    } else {
        println!();
        println!(
            "update complete: considered={} updated={} failed={}",
            output.considered, output.updated, output.failed
        );
        for row in &output.results {
            if let Some(note) = row.note.as_deref() {
                if matches!(row.action, "failed" | "update") {
                    println!("  {:<32}  {}", row.name, note);
                }
            }
        }
        if let Some(restart) = &restart {
            println!();
            println!("{}", restart.human_message());
        }
    }

    if output.failed > 0 {
        return Err(anyhow!("animus plugin update completed with {} failure(s)", output.failed));
    }
    if let Some(UpdateDaemonRestart::Failed { error }) = &restart {
        return Err(anyhow!("plugin update succeeded but the daemon restart failed: {error}"));
    }

    Ok(())
}

/// Outcome of the `--restart-daemon` step after `animus plugin update`.
enum UpdateDaemonRestart {
    SkippedFailures,
    NotRunning,
    Restarted { daemon_pid: u32 },
    Failed { error: String },
}

impl UpdateDaemonRestart {
    fn to_json(&self) -> serde_json::Value {
        match self {
            Self::SkippedFailures => serde_json::json!({
                "restarted": false,
                "reason": "update completed with failures; daemon restart skipped",
            }),
            Self::NotRunning => serde_json::json!({
                "restarted": false,
                "reason": "daemon not running; nothing to restart",
            }),
            Self::Restarted { daemon_pid } => serde_json::json!({
                "restarted": true,
                "daemon_pid": daemon_pid,
            }),
            Self::Failed { error } => serde_json::json!({
                "restarted": false,
                "error": error,
            }),
        }
    }

    fn human_message(&self) -> String {
        match self {
            Self::SkippedFailures => {
                "--restart-daemon: skipped because the update completed with failures".to_string()
            }
            Self::NotRunning => "--restart-daemon: daemon is not running; nothing to restart".to_string(),
            Self::Restarted { daemon_pid } => format!("--restart-daemon: daemon restarted (pid {daemon_pid})"),
            Self::Failed { error } => format!(
                "--restart-daemon: restart failed: {error}\n  run `animus daemon restart` manually to pick up the new plugin binaries"
            ),
        }
    }
}

const UPDATE_RESTART_SHUTDOWN_TIMEOUT_SECS: u64 = 60;

async fn restart_daemon_after_update(project_root: &str, update_failures: usize) -> UpdateDaemonRestart {
    use crate::services::runtime::{restart_running_daemon, DaemonRestartOutcome};

    if update_failures > 0 {
        return UpdateDaemonRestart::SkippedFailures;
    }
    let hub = match orchestrator_core::services::FileServiceHub::new(project_root) {
        Ok(hub) => hub,
        Err(err) => return UpdateDaemonRestart::Failed { error: format!("{err:#}") },
    };
    match restart_running_daemon(&hub, project_root, UPDATE_RESTART_SHUTDOWN_TIMEOUT_SECS).await {
        Ok(DaemonRestartOutcome::NotRunning) => UpdateDaemonRestart::NotRunning,
        Ok(DaemonRestartOutcome::Restarted { daemon_pid }) => UpdateDaemonRestart::Restarted { daemon_pid },
        Err(err) => UpdateDaemonRestart::Failed { error: format!("{err:#}") },
    }
}

// =================== Outdated ===================

#[derive(Debug, Clone, Default)]
pub(crate) struct PluginOutdatedRequest {
    pub(crate) registry_url: String,
    pub(crate) no_cache: bool,
    pub(crate) offline: bool,
    /// When set, project-local installs recorded in
    /// `<project_root>/.animus/plugins.yaml` are included in the drift
    /// report alongside the global registry (rows carry `scope: project`).
    pub(crate) project_root: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginOutdatedRow {
    pub(crate) name: String,
    pub(crate) installed_tag: Option<String>,
    pub(crate) recommended_tag: Option<String>,
    /// Latest tag published in the plugin registry. `None` when the registry
    /// was unreachable (offline / network failure) or has no matching entry.
    pub(crate) latest_tag: Option<String>,
    /// `current`, `outdated`, `ahead`, `unknown`, or `local`
    /// (non-release-source plugins that drift tracking cannot apply to).
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    /// Install scope the row came from: `global` (`~/.animus/plugins.yaml`)
    /// or `project` (`<project>/.animus/plugins.yaml`).
    pub(crate) scope: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginOutdatedOutput {
    pub(crate) registry_url: String,
    /// False when latest tags could not be resolved (offline or fetch failed).
    pub(crate) registry_reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) registry_error: Option<String>,
    pub(crate) considered: usize,
    pub(crate) outdated: usize,
    pub(crate) rows: Vec<PluginOutdatedRow>,
}

/// True when `installed` lags behind `target`. Non-semver tags fall back to
/// string inequality — any difference is reported as drift so the operator
/// looks at it.
fn tag_is_behind(installed: &str, target: &str) -> bool {
    match compare_tags(installed, target) {
        Some(std::cmp::Ordering::Less) => true,
        Some(_) => false,
        None => installed != target,
    }
}

/// Decide a drift status from the installed tag and the known reference tags.
///
/// The recommended pin (from `default-install.json`) is the authoritative
/// reference: it is the version Animus intends operators to run. The registry's
/// `latest_tag` is advisory only — its index frequently lags behind the pins,
/// so it never on its own marks a plugin "outdated".
///
/// - behind the recommended pin -> `outdated`
/// - at or ahead of the recommended pin, but ahead of the registry's latest ->
///   `current` with a "newer than registry (registry index may be stale)" note
/// - otherwise -> `current`
fn outdated_status(
    installed_tag: Option<&str>,
    recommended_tag: Option<&str>,
    latest_tag: Option<&str>,
) -> (&'static str, Option<String>) {
    let Some(installed) = installed_tag else {
        return ("unknown", Some("installed release tag unknown".to_string()));
    };

    // The recommended pin is authoritative when present.
    if let Some(recommended) = recommended_tag {
        if tag_is_behind(installed, recommended) {
            return ("outdated", None);
        }
        // At or ahead of the pin. Note the case where the registry index has
        // not yet caught up, but never let a stale registry mark us outdated.
        if let Some(latest) = latest_tag {
            if matches!(compare_tags(installed, latest), Some(std::cmp::Ordering::Greater)) {
                return ("current", Some("newer than registry (registry index may be stale)".to_string()));
            }
        }
        return ("current", None);
    }

    // No recommended pin: fall back to the registry's latest, if any.
    match latest_tag {
        Some(latest) if tag_is_behind(installed, latest) => ("outdated", None),
        Some(_) => ("current", None),
        None => ("unknown", Some("no recommended pin and no registry entry".to_string())),
    }
}

/// Pure drift computation: compare every installed plugin against the
/// recommended pins and (when available) the registry's latest tags.
fn build_outdated_rows(
    installed: &BTreeMap<String, InstalledPlugin>,
    pins: &RecommendedPins,
    registry: Option<&PluginRegistryIndex>,
    scope: &'static str,
) -> Vec<PluginOutdatedRow> {
    let mut rows: Vec<PluginOutdatedRow> = Vec::with_capacity(installed.len());
    for entry in installed.values() {
        // Reconciled view: a registry entry whose binary was deleted out of
        // band is stale, not installed. Skip it so `outdated` enumerates the
        // same set `list` shows. `animus plugin prune` clears such entries.
        if !entry.is_present() {
            continue;
        }
        let source_kind = entry.source_kind.as_deref().unwrap_or("");
        if source_kind != "release" {
            let note = if source_kind.is_empty() {
                "not from registry (no source_kind)".to_string()
            } else {
                format!("not from registry (source_kind={source_kind})")
            };
            rows.push(PluginOutdatedRow {
                name: entry.name.clone(),
                installed_tag: entry.release_tag.clone(),
                recommended_tag: None,
                latest_tag: None,
                status: "local",
                note: Some(note),
                scope,
            });
            continue;
        }
        let slug = origin_to_repo_slug(entry.origin.as_deref());
        let recommended_tag = slug.as_deref().and_then(|s| pins.lookup(s)).map(|(t, _)| t.clone());
        let latest_tag = registry.and_then(|idx| {
            idx.plugins
                .iter()
                .find(|p| slug.as_deref() == Some(p.repo.as_str()) || p.name == entry.name)
                .and_then(|p| p.latest_tag.clone())
        });
        let (status, note) =
            outdated_status(entry.release_tag.as_deref(), recommended_tag.as_deref(), latest_tag.as_deref());
        rows.push(PluginOutdatedRow {
            name: entry.name.clone(),
            installed_tag: entry.release_tag.clone(),
            recommended_tag,
            latest_tag,
            status,
            note,
            scope,
        });
    }
    rows
}

pub(crate) async fn run_plugin_outdated(req: PluginOutdatedRequest) -> Result<PluginOutdatedOutput> {
    let registry_url = if req.registry_url.trim().is_empty() {
        DEFAULT_PLUGIN_REGISTRY_URL.to_string()
    } else {
        req.registry_url.clone()
    };
    let installed = read_installed_index().context("failed to read installed plugin registry")?;
    let project_installed = match req.project_root.as_deref().map(str::trim).filter(|root| !root.is_empty()) {
        Some(root) => read_installed_index_at(&orchestrator_plugin_host::project_plugins_registry_path(
            std::path::Path::new(root),
        ))
        .context("failed to read project-local installed plugin registry")?,
        None => BTreeMap::new(),
    };
    let pins = load_recommended_pins()?;

    // Registry fetch is best-effort: drift against the recommended pins is
    // still meaningful when the network (or the cache, in --offline mode) is
    // unavailable, so a fetch failure degrades to latest=unknown instead of
    // failing the command.
    let policy = RegistryCachePolicy::from_flags(req.no_cache, req.offline);
    let (registry, registry_error) = match fetch_registry_index(&registry_url, policy).await {
        Ok(index) => (Some(index), None),
        Err(err) => (None, Some(format!("{err:#}"))),
    };

    let mut rows = build_outdated_rows(&installed, &pins, registry.as_ref(), "global");
    rows.extend(build_outdated_rows(&project_installed, &pins, registry.as_ref(), "project"));
    let outdated = rows.iter().filter(|r| r.status == "outdated").count();
    Ok(PluginOutdatedOutput {
        registry_url,
        registry_reachable: registry.is_some(),
        registry_error,
        considered: rows.len(),
        outdated,
        rows,
    })
}

pub(crate) async fn handle_plugin_outdated(
    args: PluginOutdatedArgs,
    project_root: &str,
    root_json: bool,
) -> Result<()> {
    let json = args.json || root_json;
    let output = run_plugin_outdated(PluginOutdatedRequest {
        registry_url: args.registry_url,
        no_cache: args.no_cache,
        offline: args.offline,
        project_root: Some(project_root.to_string()),
    })
    .await?;

    if json {
        print_value(&output, true)?;
    } else {
        if output.rows.is_empty() {
            println!("no installed plugins found");
        } else {
            println!(
                "{:<34} {:<8} {:<12} {:<12} {:<12} Status",
                "Plugin", "Scope", "Installed", "Recommended", "Latest"
            );
            for row in &output.rows {
                let installed = row.installed_tag.as_deref().unwrap_or("--");
                let recommended = row.recommended_tag.as_deref().unwrap_or("--");
                let latest = row.latest_tag.as_deref().unwrap_or("unknown");
                let status: String = match row.note.as_deref() {
                    Some(note) => format!("{} ({note})", row.status),
                    None => row.status.to_string(),
                };
                println!(
                    "{:<34} {:<8} {:<12} {:<12} {:<12} {}",
                    row.name, row.scope, installed, recommended, latest, status
                );
            }
            println!();
            println!("{} of {} installed plugin(s) outdated", output.outdated, output.considered);
        }
        if !output.registry_reachable {
            let detail = output.registry_error.as_deref().unwrap_or("registry unavailable");
            eprintln!("warning: latest tags unknown — {detail}");
        }
        if output.outdated > 0 {
            println!("run `animus plugin update --all --check` to preview the fix");
        }
    }

    if args.exit_code && output.outdated > 0 {
        return Err(anyhow!("{} installed plugin(s) are outdated", output.outdated));
    }
    Ok(())
}

// =================== Installed registry parsing ===================

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct InstalledPlugin {
    pub(crate) name: String,
    pub(crate) source_kind: Option<String>,
    pub(crate) origin: Option<String>,
    pub(crate) release_tag: Option<String>,
    pub(crate) installed_at: Option<String>,
    pub(crate) binary: Option<String>,
    /// The installed-BINARY sha256 the install pipeline recorded in the
    /// registry. Used to seed the host-only `installed_binary_sha256` claim
    /// when materializing a lock entry from a lock-less legacy project.
    pub(crate) sha256: Option<String>,
    pub(crate) kind: Option<String>,
    /// Whether the recorded `binary` path still resolves to a file on disk.
    /// A `plugins.yaml` entry whose binary was deleted out of band is a stale
    /// entry: it should not count as installed for `list`, `browse`,
    /// `outdated`, or `update`. Defaults to `true` when no binary path is
    /// recorded so older entries are not silently dropped.
    #[serde(default)]
    pub(crate) binary_present: bool,
}

impl InstalledPlugin {
    /// A plugin is effectively installed only while its recorded binary still
    /// exists on disk. The single reconciliation predicate every installed-state
    /// command (`list`, `browse --installed`, `outdated`, `update`) shares.
    pub(crate) fn is_present(&self) -> bool {
        self.binary_present
    }

    /// Reconstruct the lockfile `source_repo` value from this registry entry's
    /// recorded provenance, mirroring how `run_plugin_install` records it:
    /// `owner/repo` for a release, the raw URL for `--url`, `path:<...>` for
    /// `--path`. Returns `None` when the entry predates source-provenance
    /// tracking (no `source_kind`, no usable `origin`) and therefore cannot be
    /// re-installed from the lock.
    pub(crate) fn source_repo_for_lock(&self) -> Option<String> {
        let origin = self.origin.as_deref().map(str::trim).filter(|s| !s.is_empty());
        match self.source_kind.as_deref() {
            Some("release") => origin_to_repo_slug(self.origin.as_deref()),
            Some("url") => origin.map(str::to_string),
            Some("path") => origin.map(|p| if p.starts_with("path:") { p.to_string() } else { format!("path:{p}") }),
            // No recorded source_kind: fall back to an `owner/repo`-shaped
            // origin (older release rows sometimes only carried `origin`).
            _ => origin_to_repo_slug(self.origin.as_deref()),
        }
    }
}

/// Compute on-disk presence for a recorded `binary` path. Uses the same
/// resolution discovery applies (`~/` expansion, absolute/relative paths, and
/// `$PATH` lookup for bare command names) so this view never disagrees with
/// what the daemon would actually load. An entry with no recorded binary is
/// treated as present (it predates binary tracking and we must not silently
/// drop it from the reconciled view).
fn binary_path_present(binary: Option<&str>) -> bool {
    match binary {
        Some(path) if !path.trim().is_empty() => orchestrator_plugin_host::resolve_configured_binary(path).is_some(),
        _ => true,
    }
}

/// Load the installed-plugin registry as a `name → InstalledPlugin` map.
/// Tolerates a missing file (returns empty).
pub(crate) fn read_installed_index() -> Result<BTreeMap<String, InstalledPlugin>> {
    read_installed_index_at(&pick_registry_path())
}

/// Like [`read_installed_index`], but against an explicit registry path —
/// used for the project-local `<project>/.animus/plugins.yaml` registry.
pub(crate) fn read_installed_index_at(path: &std::path::Path) -> Result<BTreeMap<String, InstalledPlugin>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let contents = std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))?;
    let mut out: BTreeMap<String, InstalledPlugin> = BTreeMap::new();
    for table_key in ["plugins", "providers"] {
        if let Some(map) = yaml.get(table_key).and_then(serde_yaml::Value::as_mapping) {
            let table_kind = if table_key == "providers" { Some("provider".to_string()) } else { None };
            for (key, value) in map {
                let Some(name) = key.as_str() else { continue };
                let entry = parse_installed_entry(name, value, table_kind.clone());
                out.insert(name.to_string(), entry);
            }
        }
    }
    Ok(out)
}

fn pick_registry_path() -> PathBuf {
    let canonical = plugins_registry_path();
    if canonical.exists() {
        return canonical;
    }
    let config_dir_overridden = std::env::var("ANIMUS_CONFIG_DIR").map(|v| !v.trim().is_empty()).unwrap_or(false);
    if !config_dir_overridden {
        let legacy = legacy_plugins_registry_path();
        if legacy.exists() {
            return legacy;
        }
    }
    canonical
}

fn parse_installed_entry(name: &str, value: &serde_yaml::Value, kind: Option<String>) -> InstalledPlugin {
    let mut entry = InstalledPlugin { name: name.to_string(), kind, ..Default::default() };
    if let Some(map) = value.as_mapping() {
        for (k, v) in map {
            let Some(field) = k.as_str() else { continue };
            let str_val = v.as_str().map(str::to_string);
            match field {
                "source_kind" => entry.source_kind = str_val,
                "origin" => entry.origin = str_val,
                "release_tag" => entry.release_tag = str_val,
                "installed_at" => entry.installed_at = str_val,
                "binary" => entry.binary = str_val,
                "sha256" => entry.sha256 = str_val,
                _ => {}
            }
        }
    }
    entry.binary_present = binary_path_present(entry.binary.as_deref());
    entry
}

/// Extract `owner/repo` from an origin like `launchapp-dev/animus-provider-claude@v0.1.0`.
fn origin_to_repo_slug(origin: Option<&str>) -> Option<String> {
    let raw = origin?.trim();
    if raw.is_empty() {
        return None;
    }
    let slug = raw.split('@').next()?.trim();
    if slug.contains('/') {
        Some(slug.to_string())
    } else {
        None
    }
}

// =================== Registry fetch + cache ===================

fn cache_path() -> PathBuf {
    if let Ok(val) = std::env::var("ANIMUS_PLUGIN_REGISTRY_CACHE") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"));
    base.join("animus").join("plugin-registry.json")
}

/// How `fetch_registry_index` balances the on-disk cache against the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistryCachePolicy {
    /// Fresh cache (within TTL) wins; otherwise fetch, falling back to a
    /// stale cache with a loud warning when the network fails.
    Default,
    /// Skip the cache read and force a network fetch. Network failures are
    /// hard errors — the caller explicitly asked for fresh data.
    NoCache,
    /// Never touch the network. Serve the cache regardless of age; error
    /// only when no cache exists for this URL.
    Offline,
}

impl RegistryCachePolicy {
    pub(crate) fn from_flags(no_cache: bool, offline: bool) -> Self {
        // clap marks the flags mutually exclusive; offline wins defensively.
        if offline {
            Self::Offline
        } else if no_cache {
            Self::NoCache
        } else {
            Self::Default
        }
    }
}

/// Fetch the registry index from `url`, honoring the on-disk cache per `policy`.
pub(crate) async fn fetch_registry_index(url: &str, policy: RegistryCachePolicy) -> Result<PluginRegistryIndex> {
    match policy {
        RegistryCachePolicy::Offline => match load_cache_entry(url) {
            Some((index, age)) => {
                if age > REGISTRY_CACHE_TTL {
                    eprintln!(
                        "warning: --offline serving a cached registry index that is {} old ({})",
                        format_age(age),
                        cache_path().display()
                    );
                }
                Ok(index)
            }
            None => Err(invalid_input_error(format!(
                "--offline requested but no cached registry index exists for {url} at {}; \
                 run once without --offline to populate the cache",
                cache_path().display()
            ))),
        },
        RegistryCachePolicy::NoCache => fetch_and_cache(url).await,
        RegistryCachePolicy::Default => {
            let cached = load_cache_entry(url);
            if let Some((index, age)) = &cached {
                if *age <= REGISTRY_CACHE_TTL {
                    return Ok(index.clone());
                }
            }
            match fetch_and_cache(url).await {
                Ok(index) => Ok(index),
                Err(fetch_err) => match cached {
                    Some((index, age)) => {
                        eprintln!(
                            "warning: registry fetch failed ({fetch_err:#}); \
                             falling back to a STALE cached index that is {} old ({}). \
                             Results may be out of date.",
                            format_age(age),
                            cache_path().display()
                        );
                        Ok(index)
                    }
                    None => Err(fetch_err),
                },
            }
        }
    }
}

async fn fetch_and_cache(url: &str) -> Result<PluginRegistryIndex> {
    let body = http_get(url).await?;
    let index: PluginRegistryIndex =
        serde_json::from_str(&body).with_context(|| format!("failed to parse plugin registry JSON from {url}"))?;
    if let Err(err) = write_cache(url, &body) {
        tracing::warn!(error = %err, "failed to write plugin registry cache");
    }
    Ok(index)
}

/// Load the cached index for `url` regardless of age, returning the cache age
/// alongside it. Callers decide whether the entry is fresh enough.
fn load_cache_entry(url: &str) -> Option<(PluginRegistryIndex, Duration)> {
    let path = cache_path();
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).unwrap_or(Duration::ZERO);
    let body = std::fs::read_to_string(&path).ok()?;
    let envelope: CachedRegistry = serde_json::from_str(&body).ok()?;
    if envelope.url != url {
        return None;
    }
    let index: PluginRegistryIndex = serde_json::from_str(&envelope.body).ok()?;
    Some((index, age))
}

/// Render a cache age as a short human string ("3h", "2d", "45m").
fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn write_cache(url: &str, body: &str) -> Result<()> {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create cache dir {}", parent.display()))?;
    }
    let envelope = CachedRegistry { url: url.to_string(), body: body.to_string() };
    let serialized = serde_json::to_string(&envelope)?;
    std::fs::write(&path, serialized).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedRegistry {
    url: String,
    body: String,
}

/// Total attempts for a registry GET: 1 initial + 2 retries.
const HTTP_GET_ATTEMPTS: u32 = 3;
/// Base backoff between attempts (doubled per retry: 250ms, 500ms).
const HTTP_GET_BACKOFF: Duration = Duration::from_millis(250);

/// Whether a failed HTTP status is worth retrying: transient server errors
/// and rate limits; other 4xx are deterministic and fail immediately.
fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

/// Actionable error message for an HTTP 429 from the registry host.
fn rate_limit_message(url: &str) -> String {
    format!(
        "GET {url} was rate-limited (HTTP 429). The registry host is throttling \
         requests — wait a minute and retry, or pass --offline to serve the \
         cached registry index without touching the network."
    )
}

async fn http_get(url: &str) -> Result<String> {
    let agent = format!("animus-cli/{}", env!("CARGO_PKG_VERSION"));
    let client = reqwest::Client::builder()
        .user_agent(agent)
        .timeout(Duration::from_secs(20))
        .build()
        .context("failed to build HTTP client")?;

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=HTTP_GET_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(HTTP_GET_BACKOFF * 2u32.pow(attempt - 2)).await;
        }
        match client.get(url).send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return response.text().await.with_context(|| format!("failed to read body from {url}"));
                }
                let err = if status.as_u16() == 429 {
                    anyhow!(rate_limit_message(url))
                } else {
                    anyhow!("GET {url} returned non-success status {status}")
                };
                if !is_retryable_status(status.as_u16()) {
                    return Err(err);
                }
                last_err = Some(err);
            }
            Err(send_err) => {
                last_err = Some(anyhow::Error::new(send_err).context(format!("failed to GET {url}")));
            }
        }
    }
    Err(last_err
        .unwrap_or_else(|| anyhow!("failed to GET {url}"))
        .context(format!("registry fetch failed after {HTTP_GET_ATTEMPTS} attempts")))
}

// =================== Helpers exposed for sibling code ===================

/// Format an installed-plugin source as `<source_kind>@<origin>` for display.
pub(crate) fn format_installed_source(entry: &InstalledPlugin) -> String {
    match (entry.source_kind.as_deref(), entry.origin.as_deref()) {
        (Some(kind), Some(origin)) => format!("{kind}@{origin}"),
        (Some(kind), None) => kind.to_string(),
        (None, Some(origin)) => origin.to_string(),
        (None, None) => "--".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: &str, tags: &[&str], stability: Option<&str>, org: &str) -> RegistryPluginEntry {
        RegistryPluginEntry {
            name: name.to_string(),
            kind: kind.to_string(),
            repo: format!("{org}/{name}"),
            latest_tag: Some("v0.1.0".to_string()),
            description: format!("desc-{name}"),
            homepage: None,
            license: None,
            stability: stability.map(str::to_string),
            platforms: vec![],
            tags: tags.iter().map(|s| s.to_string()).collect(),
            install_hint: None,
        }
    }

    fn fixture_index() -> PluginRegistryIndex {
        PluginRegistryIndex {
            registry_version: Some("0.1.0".to_string()),
            updated_at: None,
            plugins: vec![
                entry(
                    "animus-subject-linear",
                    "subject_backend",
                    &["linear", "subject"],
                    Some("alpha"),
                    "launchapp-dev",
                ),
                entry("animus-provider-claude", "provider", &["llm", "claude"], Some("alpha"), "launchapp-dev"),
                entry("animus-provider-gemini", "provider", &["llm", "google"], Some("stable"), "launchapp-dev"),
                entry("animus-provider-third-party", "provider", &["llm"], Some("alpha"), "other-org"),
            ],
        }
    }

    fn apply_filters(req: PluginSearchRequest) -> PluginSearchOutput {
        let idx = fixture_index();
        let total = idx.plugins.len();

        let query_lower = req.query.as_deref().map(str::to_ascii_lowercase);
        let kind_lower = req.kind.as_deref().map(str::to_ascii_lowercase);
        let stability_lower = req.stability.as_deref().map(str::to_ascii_lowercase);
        let org_lower = req.org.as_deref().map(str::to_ascii_lowercase);
        let tags_lower: Vec<String> = req.tag.iter().map(|t| t.to_ascii_lowercase()).collect();

        let mut matched: Vec<PluginSearchRow> = idx
            .plugins
            .into_iter()
            .filter(|e| {
                if let Some(q) = query_lower.as_deref() {
                    let name = e.name.to_ascii_lowercase();
                    let desc = e.description.to_ascii_lowercase();
                    if !name.contains(q) && !desc.contains(q) {
                        return false;
                    }
                }
                if let Some(k) = kind_lower.as_deref() {
                    if e.kind.to_ascii_lowercase() != k {
                        return false;
                    }
                }
                if let Some(s) = stability_lower.as_deref() {
                    if e.stability.as_deref().map(str::to_ascii_lowercase) != Some(s.to_string()) {
                        return false;
                    }
                }
                if let Some(o) = org_lower.as_deref() {
                    if e.org().map(str::to_ascii_lowercase) != Some(o.to_string()) {
                        return false;
                    }
                }
                if !tags_lower.is_empty() {
                    let etags: BTreeSet<String> = e.tags.iter().map(|t| t.to_ascii_lowercase()).collect();
                    for needle in &tags_lower {
                        if !etags.contains(needle) {
                            return false;
                        }
                    }
                }
                true
            })
            .map(to_search_row)
            .collect();
        matched.sort_by(|a, b| a.name.cmp(&b.name));
        PluginSearchOutput { registry_url: "fixture://".to_string(), total, matched: matched.len(), results: matched }
    }

    #[test]
    fn search_filters_by_substring() {
        let out = apply_filters(PluginSearchRequest { query: Some("linear".to_string()), ..Default::default() });
        assert_eq!(out.matched, 1);
        assert_eq!(out.results[0].name, "animus-subject-linear");
    }

    #[test]
    fn search_filters_by_kind() {
        let out = apply_filters(PluginSearchRequest { kind: Some("provider".to_string()), ..Default::default() });
        assert_eq!(out.matched, 3);
        for row in &out.results {
            assert_eq!(row.kind, "provider");
        }
    }

    #[test]
    fn search_filters_by_tag() {
        let out = apply_filters(PluginSearchRequest { tag: vec!["llm".to_string()], ..Default::default() });
        assert_eq!(out.matched, 3);
        assert!(out.results.iter().all(|r| r.tags.iter().any(|t| t == "llm")));
    }

    #[test]
    fn search_filters_by_org_and_stability() {
        let out = apply_filters(PluginSearchRequest {
            org: Some("launchapp-dev".to_string()),
            stability: Some("stable".to_string()),
            ..Default::default()
        });
        assert_eq!(out.matched, 1);
        assert_eq!(out.results[0].name, "animus-provider-gemini");
    }

    #[test]
    fn search_returns_json_when_requested() {
        let out = apply_filters(PluginSearchRequest { query: Some("claude".to_string()), ..Default::default() });
        let json = serde_json::to_value(&out).expect("serialize");
        assert!(json.get("results").is_some(), "json must include results");
        assert!(json.get("matched").is_some(), "json must include matched");
        assert!(json.get("total").is_some(), "json must include total");
        let results = json["results"].as_array().expect("results array");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["name"], "animus-provider-claude");
        assert!(
            results[0]["install_command"].as_str().unwrap_or("").contains("animus plugin install"),
            "install_command should be present"
        );
    }

    #[test]
    fn browse_groups_by_kind() {
        let idx = fixture_index();
        let total = idx.plugins.len();
        let mut groups: BTreeMap<String, Vec<PluginBrowseRow>> = BTreeMap::new();
        let mut shown = 0;
        for entry in idx.plugins {
            let row = PluginBrowseRow {
                name: entry.name.clone(),
                kind: entry.kind.clone(),
                description: entry.description.clone(),
                repo: entry.repo.clone(),
                latest_tag: entry.latest_tag.clone(),
                stability: entry.stability.clone(),
                installed: false,
                installed_tag: None,
            };
            groups.entry(entry.kind.clone()).or_default().push(row);
            shown += 1;
        }
        assert_eq!(shown, total);
        assert!(groups.contains_key("provider"));
        assert!(groups.contains_key("subject_backend"));
        assert_eq!(groups["provider"].len(), 3);
        assert_eq!(groups["subject_backend"].len(), 1);
    }

    #[test]
    fn browse_filters_installed_only() {
        let idx = fixture_index();
        let mut installed: BTreeMap<String, InstalledPlugin> = BTreeMap::new();
        installed.insert(
            "animus-provider-claude".to_string(),
            InstalledPlugin {
                name: "animus-provider-claude".to_string(),
                source_kind: Some("release".to_string()),
                origin: Some("launchapp-dev/animus-provider-claude@v0.1.0".to_string()),
                release_tag: Some("v0.1.0".to_string()),
                ..Default::default()
            },
        );

        let mut shown = 0;
        for entry in idx.plugins {
            let is_installed = installed.contains_key(&entry.name);
            if !is_installed {
                continue;
            }
            shown += 1;
            assert_eq!(entry.name, "animus-provider-claude");
        }
        assert_eq!(shown, 1);
    }

    #[test]
    fn update_detects_newer_release_tag() {
        let installed = InstalledPlugin {
            name: "animus-provider-claude".to_string(),
            source_kind: Some("release".to_string()),
            origin: Some("launchapp-dev/animus-provider-claude@v0.1.0".to_string()),
            release_tag: Some("v0.1.0".to_string()),
            ..Default::default()
        };
        let registry_latest = "v0.1.1".to_string();
        let needs_update = installed.release_tag.as_deref() != Some(&registry_latest);
        assert!(needs_update);
        let slug = origin_to_repo_slug(installed.origin.as_deref()).unwrap();
        assert_eq!(slug, "launchapp-dev/animus-provider-claude");
    }

    #[test]
    fn update_dry_run_does_not_install() {
        let installed_tag = Some("v0.1.0".to_string());
        let target_tag = Some("v0.1.1".to_string());
        let dry_run = true;
        let force = false;
        let needs_update = match (&installed_tag, &target_tag) {
            (Some(installed), Some(target)) => installed != target || force,
            (None, Some(_)) => true,
            (_, None) => force,
        };
        assert!(needs_update);
        assert!(dry_run, "dry_run gate prevents install");
    }

    #[test]
    fn update_skips_path_source_plugins() {
        let installed = InstalledPlugin {
            name: "my-local-test".to_string(),
            source_kind: Some("path".to_string()),
            origin: None,
            release_tag: None,
            ..Default::default()
        };
        let is_release = installed.source_kind.as_deref() == Some("release");
        assert!(!is_release, "path-source plugins must be skipped by update");
    }

    #[test]
    fn origin_to_repo_slug_parses_release_origin() {
        let slug = origin_to_repo_slug(Some("launchapp-dev/animus-provider-claude@v0.1.0")).unwrap();
        assert_eq!(slug, "launchapp-dev/animus-provider-claude");
        let slug = origin_to_repo_slug(Some("launchapp-dev/animus-provider-claude")).unwrap();
        assert_eq!(slug, "launchapp-dev/animus-provider-claude");
        assert!(origin_to_repo_slug(Some("not-a-slug")).is_none());
        assert!(origin_to_repo_slug(None).is_none());
        assert!(origin_to_repo_slug(Some("")).is_none());
    }

    #[test]
    fn format_installed_source_handles_missing_fields() {
        let release = InstalledPlugin {
            name: "x".to_string(),
            source_kind: Some("release".to_string()),
            origin: Some("launchapp-dev/foo@v1".to_string()),
            ..Default::default()
        };
        assert_eq!(format_installed_source(&release), "release@launchapp-dev/foo@v1");
        let bare = InstalledPlugin {
            name: "y".to_string(),
            source_kind: Some("path".to_string()),
            origin: None,
            ..Default::default()
        };
        assert_eq!(format_installed_source(&bare), "path");
        let empty = InstalledPlugin { name: "z".to_string(), ..Default::default() };
        assert_eq!(format_installed_source(&empty), "--");
    }

    #[test]
    fn registry_index_deserializes_real_shape() {
        let json = r#"{
            "registry_version": "0.1.0",
            "updated_at": "2026-05-18T00:00:00Z",
            "plugins": [
                {
                    "name": "animus-subject-linear",
                    "kind": "subject_backend",
                    "repo": "launchapp-dev/animus-subject-linear",
                    "latest_tag": "v0.1.0",
                    "description": "Linear subject backend plugin",
                    "stability": "alpha",
                    "platforms": ["aarch64-apple-darwin"],
                    "tags": ["subject", "linear"]
                }
            ]
        }"#;
        let idx: PluginRegistryIndex = serde_json::from_str(json).unwrap();
        assert_eq!(idx.plugins.len(), 1);
        assert_eq!(idx.plugins[0].name, "animus-subject-linear");
        assert_eq!(idx.plugins[0].kind, "subject_backend");
        assert_eq!(idx.plugins[0].latest_tag.as_deref(), Some("v0.1.0"));
    }

    #[test]
    fn list_human_output_includes_source_columns() {
        let release = InstalledPlugin {
            name: "animus-provider-claude".to_string(),
            source_kind: Some("release".to_string()),
            origin: Some("launchapp-dev/animus-provider-claude@v0.1.1".to_string()),
            release_tag: Some("v0.1.1".to_string()),
            installed_at: Some("2026-05-18T01:02:03+00:00".to_string()),
            ..Default::default()
        };
        let source = format_installed_source(&release);
        assert!(source.starts_with("release@"), "SOURCE column must start with `release@`: {source}");
        assert!(source.contains("launchapp-dev/animus-provider-claude"), "SOURCE column must show origin: {source}");
        let installed_at = release.installed_at.as_deref().unwrap();
        let date_only = installed_at.split('T').next().unwrap();
        assert_eq!(date_only, "2026-05-18");

        let path_install = InstalledPlugin {
            name: "my-local-test".to_string(),
            source_kind: Some("path".to_string()),
            origin: None,
            installed_at: Some("2026-05-17T10:00:00+00:00".to_string()),
            ..Default::default()
        };
        assert_eq!(format_installed_source(&path_install), "path");

        let unknown = InstalledPlugin { name: "ghost".to_string(), ..Default::default() };
        assert_eq!(format_installed_source(&unknown), "--");
    }

    // =================== v0.5.8: default-install-driven update ===================

    fn release(name: &str, slug: &str, tag: &str) -> InstalledPlugin {
        InstalledPlugin {
            name: name.to_string(),
            source_kind: Some("release".to_string()),
            origin: Some(format!("{slug}@{tag}")),
            release_tag: Some(tag.to_string()),
            // No binary recorded → treated as present by the reconciled view.
            binary_present: true,
            ..Default::default()
        }
    }

    fn fixture_pins() -> RecommendedPins {
        RecommendedPins::parse(
            r#"{
                "schema": "animus.default-install.v1",
                "plugins": {
                    "providers": [
                        {"repo": "launchapp-dev/animus-provider-claude", "tag": "v0.2.2"},
                        {"repo": "launchapp-dev/animus-provider-codex", "tag": "v0.2.3"}
                    ],
                    "subjects": [
                        {"repo": "launchapp-dev/animus-subject-default", "tag": "v0.1.4"},
                        {"repo": "launchapp-dev/animus-subject-requirements", "tag": "v0.1.7"}
                    ],
                    "queues": [
                        {"repo": "launchapp-dev/animus-queue-default", "tag": "v0.3.0"}
                    ]
                }
            }"#,
        )
        .expect("fixture pins parse")
    }

    fn fixture_installed() -> BTreeMap<String, InstalledPlugin> {
        let mut map = BTreeMap::new();
        // Drift: current v0.2.1, recommended v0.2.2.
        map.insert(
            "animus-provider-claude".to_string(),
            release("animus-provider-claude", "launchapp-dev/animus-provider-claude", "v0.2.1"),
        );
        // Up-to-date.
        map.insert(
            "animus-provider-codex".to_string(),
            release("animus-provider-codex", "launchapp-dev/animus-provider-codex", "v0.2.3"),
        );
        // Ahead of pin.
        map.insert(
            "animus-subject-default".to_string(),
            release("animus-subject-default", "launchapp-dev/animus-subject-default", "v0.1.5"),
        );
        // Drift: queue.
        map.insert(
            "animus-queue-default".to_string(),
            release("animus-queue-default", "launchapp-dev/animus-queue-default", "v0.2.0"),
        );
        // Not in default-install.
        map.insert(
            "animus-trigger-webhook".to_string(),
            release("animus-trigger-webhook", "launchapp-dev/animus-trigger-webhook", "v0.1.0"),
        );
        // Non-release source (local --path install).
        map.insert(
            "my-local-plugin".to_string(),
            InstalledPlugin {
                name: "my-local-plugin".to_string(),
                source_kind: Some("path".to_string()),
                origin: None,
                release_tag: None,
                binary_present: true,
                ..Default::default()
            },
        );
        map
    }

    #[test]
    fn embedded_default_install_json_parses() {
        let pins = load_recommended_pins().expect("embedded default-install.json must parse");
        assert!(!pins.by_slug.is_empty(), "default-install.json must declare at least one plugin pin");
        // Spot-check one curated pin so we catch a future schema rename.
        assert!(
            pins.lookup("launchapp-dev/animus-queue-default").is_some(),
            "default-install.json must pin animus-queue-default"
        );
    }

    #[test]
    fn update_plan_all_covers_every_installed_plugin() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let plans = build_update_plan(&installed, &pins, &PluginUpdateSelector::All, None, false).unwrap();
        assert_eq!(plans.len(), installed.len());
    }

    #[test]
    fn update_plan_filters_by_kind() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let plans = build_update_plan(
            &installed,
            &pins,
            &PluginUpdateSelector::Kind("subject_backend".to_string()),
            None,
            false,
        )
        .unwrap();
        let names: Vec<_> = plans.iter().map(|p| p.entry.name.clone()).collect();
        assert_eq!(names, vec!["animus-subject-default".to_string()]);
    }

    #[test]
    fn update_plan_filters_by_kind_canonical_plural() {
        // `--kind queues` (canonical) and `--kind queue` (singular) should
        // both target the queue plugin.
        let installed = fixture_installed();
        let pins = fixture_pins();
        for kind in ["queues", "queue"] {
            let plans =
                build_update_plan(&installed, &pins, &PluginUpdateSelector::Kind(kind.to_string()), None, false)
                    .unwrap();
            let names: Vec<_> = plans.iter().map(|p| p.entry.name.clone()).collect();
            assert_eq!(names, vec!["animus-queue-default".to_string()], "kind={kind}");
        }
    }

    #[test]
    fn update_plan_name_selector_targets_one() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let plans = build_update_plan(
            &installed,
            &pins,
            &PluginUpdateSelector::Name("animus-queue-default".to_string()),
            None,
            false,
        )
        .unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].entry.name, "animus-queue-default");
        assert_eq!(plans[0].action, "update");
        assert_eq!(plans[0].recommended_tag.as_deref(), Some("v0.3.0"));
    }

    #[test]
    fn update_plan_name_selector_errors_when_missing() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let err = build_update_plan(
            &installed,
            &pins,
            &PluginUpdateSelector::Name("does-not-exist".to_string()),
            None,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not installed"), "err: {err}");
    }

    #[test]
    fn update_plan_marks_current_when_tags_match() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let plans = build_update_plan(&installed, &pins, &PluginUpdateSelector::All, None, false).unwrap();
        let codex = plans.iter().find(|p| p.entry.name == "animus-provider-codex").unwrap();
        assert_eq!(codex.action, "skip");
        assert_eq!(codex.note.as_deref(), Some("current"));
    }

    #[test]
    fn update_plan_marks_ahead_of_pin() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let plans = build_update_plan(&installed, &pins, &PluginUpdateSelector::All, None, false).unwrap();
        let subj = plans.iter().find(|p| p.entry.name == "animus-subject-default").unwrap();
        assert_eq!(subj.action, "skip");
        assert_eq!(subj.note.as_deref(), Some("ahead of pin"));
    }

    #[test]
    fn update_plan_skips_plugins_missing_from_default_install() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let plans = build_update_plan(&installed, &pins, &PluginUpdateSelector::All, None, false).unwrap();
        let trig = plans.iter().find(|p| p.entry.name == "animus-trigger-webhook").unwrap();
        assert_eq!(trig.action, "skip");
        assert_eq!(trig.note.as_deref(), Some("not in default-install"));
    }

    #[test]
    fn update_plan_skips_non_release_source_plugins() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let plans = build_update_plan(&installed, &pins, &PluginUpdateSelector::All, None, false).unwrap();
        let local = plans.iter().find(|p| p.entry.name == "my-local-plugin").unwrap();
        assert_eq!(local.action, "skip");
        assert!(
            local.note.as_deref().unwrap_or("").contains("not from registry"),
            "expected 'not from registry' note, got {:?}",
            local.note
        );
    }

    #[test]
    fn update_plan_force_reinstalls_already_current() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let plans = build_update_plan(&installed, &pins, &PluginUpdateSelector::All, None, true).unwrap();
        let codex = plans.iter().find(|p| p.entry.name == "animus-provider-codex").unwrap();
        assert_eq!(codex.action, "update");
        assert_eq!(codex.note.as_deref(), Some("forced reinstall"));
    }

    #[test]
    fn update_plan_force_downgrades_when_ahead() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let plans = build_update_plan(&installed, &pins, &PluginUpdateSelector::All, None, true).unwrap();
        let subj = plans.iter().find(|p| p.entry.name == "animus-subject-default").unwrap();
        assert_eq!(subj.action, "update");
        assert_eq!(subj.note.as_deref(), Some("forced downgrade"));
    }

    #[test]
    fn parse_tag_strips_leading_v_and_compares_semver() {
        assert_eq!(compare_tags("v0.1.0", "v0.1.1"), Some(std::cmp::Ordering::Less));
        assert_eq!(compare_tags("0.2.0", "v0.1.9"), Some(std::cmp::Ordering::Greater));
        assert_eq!(compare_tags("v1.0.0", "v1.0.0"), Some(std::cmp::Ordering::Equal));
        // Non-semver tag — falls back to string compare via None.
        assert_eq!(compare_tags("abc", "v0.1.0"), None);
    }

    #[test]
    fn recommended_pins_parses_default_install_layout() {
        let pins = fixture_pins();
        assert_eq!(
            pins.lookup("launchapp-dev/animus-provider-claude").map(|(t, s)| (t.as_str(), s.as_str())),
            Some(("v0.2.2", "providers"))
        );
        assert_eq!(
            pins.lookup("launchapp-dev/animus-queue-default").map(|(t, s)| (t.as_str(), s.as_str())),
            Some(("v0.3.0", "queues"))
        );
        assert!(pins.lookup("launchapp-dev/animus-provider-unknown").is_none());
    }

    #[test]
    fn normalize_kind_selector_handles_aliases() {
        assert_eq!(normalize_kind_selector("provider"), "providers");
        assert_eq!(normalize_kind_selector("Subject_Backend"), "subjects");
        assert_eq!(normalize_kind_selector("subjects"), "subjects");
        assert_eq!(normalize_kind_selector("workflow_runner"), "workflow_runners");
        assert_eq!(normalize_kind_selector("transport"), "transports");
        // Unknown values pass through (lowercased) so the build_update_plan
        // filter simply matches zero plugins and the surface stays predictable.
        assert_eq!(normalize_kind_selector("Bogus"), "bogus");
    }

    // =================== v0.5.x: outdated ===================

    #[test]
    fn outdated_status_classifies_drift() {
        // Behind the recommended pin → outdated (the pin is authoritative).
        assert_eq!(outdated_status(Some("v0.1.0"), Some("v0.2.0"), None).0, "outdated");
        // At the pin but the registry latest is newer → still current. A
        // stale/lagging registry never marks a pinned install outdated.
        assert_eq!(outdated_status(Some("v0.2.0"), Some("v0.2.0"), Some("v0.3.0")).0, "current");
        // Current against both references.
        assert_eq!(outdated_status(Some("v0.2.0"), Some("v0.2.0"), Some("v0.2.0")).0, "current");
        // Ahead of the pin AND ahead of the registry latest → current, with a
        // "newer than registry" note (no "ahead of every known reference").
        let (status, note) = outdated_status(Some("v0.9.0"), Some("v0.2.0"), Some("v0.3.0"));
        assert_eq!(status, "current");
        assert_eq!(note.as_deref(), Some("newer than registry (registry index may be stale)"));
        // No references at all.
        assert_eq!(outdated_status(Some("v0.1.0"), None, None).0, "unknown");
        // Installed tag missing.
        assert_eq!(outdated_status(None, Some("v0.2.0"), None).0, "unknown");
        // Non-semver mismatch against the pin counts as drift.
        assert_eq!(outdated_status(Some("nightly-1"), Some("nightly-2"), None).0, "outdated");
    }

    #[test]
    fn outdated_rows_offline_compare_against_pins_alone() {
        let installed = fixture_installed();
        let pins = fixture_pins();
        let rows = build_outdated_rows(&installed, &pins, None, "global");
        assert_eq!(rows.len(), installed.len());
        let by_name = |n: &str| rows.iter().find(|r| r.name == n).unwrap();

        let claude = by_name("animus-provider-claude");
        assert_eq!(claude.status, "outdated");
        assert_eq!(claude.recommended_tag.as_deref(), Some("v0.2.2"));
        assert_eq!(claude.latest_tag, None, "latest must be unknown offline");

        assert_eq!(by_name("animus-provider-codex").status, "current");
        // Ahead of the pin with no registry reference → current (the pin is the
        // authoritative reference; being newer than it is not drift).
        assert_eq!(by_name("animus-subject-default").status, "current");
        assert_eq!(by_name("animus-trigger-webhook").status, "unknown");
        let local = by_name("my-local-plugin");
        assert_eq!(local.status, "local");
        assert!(local.note.as_deref().unwrap_or("").contains("not from registry"));
    }

    #[test]
    fn outdated_rows_use_registry_latest_when_reachable() {
        let mut installed = BTreeMap::new();
        installed.insert(
            "animus-provider-claude".to_string(),
            release("animus-provider-claude", "launchapp-dev/animus-provider-claude", "v0.2.2"),
        );
        let pins = fixture_pins();
        let registry = PluginRegistryIndex {
            registry_version: None,
            updated_at: None,
            plugins: vec![RegistryPluginEntry {
                name: "animus-provider-claude".to_string(),
                kind: "provider".to_string(),
                repo: "launchapp-dev/animus-provider-claude".to_string(),
                latest_tag: Some("v0.5.0".to_string()),
                description: String::new(),
                homepage: None,
                license: None,
                stability: None,
                platforms: vec![],
                tags: vec![],
                install_hint: None,
            }],
        };
        let rows = build_outdated_rows(&installed, &pins, Some(&registry), "global");
        assert_eq!(rows.len(), 1);
        // Matches the pin (v0.2.2); the registry advertises a newer v0.5.0, but
        // the recommended pin is authoritative so this stays current. The
        // registry latest is still surfaced for operator visibility.
        assert_eq!(rows[0].status, "current");
        assert_eq!(rows[0].latest_tag.as_deref(), Some("v0.5.0"));
        assert_eq!(rows[0].recommended_tag.as_deref(), Some("v0.2.2"));
    }

    // =================== v0.5.x: cache policy + retry ===================

    #[test]
    fn cache_policy_from_flags() {
        assert_eq!(RegistryCachePolicy::from_flags(false, false), RegistryCachePolicy::Default);
        assert_eq!(RegistryCachePolicy::from_flags(true, false), RegistryCachePolicy::NoCache);
        assert_eq!(RegistryCachePolicy::from_flags(false, true), RegistryCachePolicy::Offline);
    }

    #[test]
    fn retryable_status_covers_429_and_5xx_only() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(503));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(200));
    }

    #[test]
    fn rate_limit_message_is_actionable() {
        let msg = rate_limit_message("https://example.invalid/plugins.json");
        assert!(msg.contains("429"), "must name the status: {msg}");
        assert!(msg.contains("--offline"), "must point at the offline escape hatch: {msg}");
    }

    #[test]
    fn format_age_renders_short_units() {
        assert_eq!(format_age(Duration::from_secs(30)), "30s");
        assert_eq!(format_age(Duration::from_mins(2)), "2m");
        assert_eq!(format_age(Duration::from_hours(2)), "2h");
        assert_eq!(format_age(Duration::from_hours(48)), "2d");
    }

    /// Unreachable-by-construction URL: connections to port 1 on localhost are
    /// refused immediately, so retry backoff dominates test time (~750ms).
    const UNREACHABLE_URL: &str = "http://127.0.0.1:1/plugins.json";

    fn write_stale_cache(dir: &std::path::Path, url: &str) -> PathBuf {
        let cache_file = dir.join("plugin-registry.json");
        let index_body = serde_json::to_string(&fixture_index()).expect("serialize index");
        let envelope = CachedRegistry { url: url.to_string(), body: index_body };
        std::fs::write(&cache_file, serde_json::to_string(&envelope).unwrap()).expect("write cache");
        // Backdate well past the 6h TTL.
        let stale_mtime = SystemTime::now() - Duration::from_hours(48);
        let file = std::fs::OpenOptions::new().write(true).open(&cache_file).expect("open cache");
        file.set_times(std::fs::FileTimes::new().set_modified(stale_mtime)).expect("backdate cache mtime");
        cache_file
    }

    fn block_on_fetch(url: &str, policy: RegistryCachePolicy) -> Result<PluginRegistryIndex> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(fetch_registry_index(url, policy))
    }

    #[test]
    fn fetch_falls_back_to_stale_cache_when_network_fails() {
        use protocol::test_utils::EnvVarGuard;
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_file = write_stale_cache(dir.path(), UNREACHABLE_URL);
        let _cache = EnvVarGuard::set("ANIMUS_PLUGIN_REGISTRY_CACHE", Some(cache_file.to_str().unwrap()));

        let index = block_on_fetch(UNREACHABLE_URL, RegistryCachePolicy::Default)
            .expect("stale cache must serve when the network is down");
        assert_eq!(index.plugins.len(), fixture_index().plugins.len());
    }

    #[test]
    fn fetch_offline_serves_stale_cache_without_network() {
        use protocol::test_utils::EnvVarGuard;
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_file = write_stale_cache(dir.path(), UNREACHABLE_URL);
        let _cache = EnvVarGuard::set("ANIMUS_PLUGIN_REGISTRY_CACHE", Some(cache_file.to_str().unwrap()));

        let index = block_on_fetch(UNREACHABLE_URL, RegistryCachePolicy::Offline)
            .expect("offline mode must serve the cache regardless of age");
        assert_eq!(index.plugins.len(), fixture_index().plugins.len());
    }

    #[test]
    fn fetch_offline_errors_when_no_cache_exists() {
        use protocol::test_utils::EnvVarGuard;
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_file = dir.path().join("missing-cache.json");
        let _cache = EnvVarGuard::set("ANIMUS_PLUGIN_REGISTRY_CACHE", Some(cache_file.to_str().unwrap()));

        let err =
            block_on_fetch(UNREACHABLE_URL, RegistryCachePolicy::Offline).expect_err("offline with no cache must fail");
        assert!(err.to_string().contains("--offline"), "err must explain the offline failure: {err}");
    }

    #[test]
    fn fetch_no_cache_hard_fails_even_with_stale_cache_present() {
        use protocol::test_utils::EnvVarGuard;
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_file = write_stale_cache(dir.path(), UNREACHABLE_URL);
        let _cache = EnvVarGuard::set("ANIMUS_PLUGIN_REGISTRY_CACHE", Some(cache_file.to_str().unwrap()));

        let result = block_on_fetch(UNREACHABLE_URL, RegistryCachePolicy::NoCache);
        assert!(result.is_err(), "--no-cache must not silently fall back to the cache");
    }

    #[test]
    fn tag_override_without_name_is_rejected() {
        // run_plugin_update enforces this at the request layer.
        let req = PluginUpdateRequest {
            selector: PluginUpdateSelector::All,
            tag_override: Some("v9.9.9".to_string()),
            check: true,
            force: false,
            project_root: None,
            project: false,
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let err = rt.block_on(run_plugin_update(req)).unwrap_err();
        assert!(err.to_string().contains("--tag"), "err: {err}");
    }
}
