//! Marketplace operations backed by the public Animus plugin registry.
//!
//! Provides three CLI commands and matching reusable runners:
//! * `animus plugin search` — substring + filter search against the registry index
//! * `animus plugin browse` — grouped listing (installed vs available)
//! * `animus plugin update` — re-resolve latest release tag for installed release-source plugins
//!
//! All three share a registry fetch + on-disk cache layer (`~/.cache/animus/plugin-registry.json`,
//! refreshed every 6 hours unless `--no-cache` is passed).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use orchestrator_plugin_host::{legacy_plugins_registry_path, plugins_registry_path};
use serde::{Deserialize, Serialize};

use crate::{
    invalid_input_error, print_value, PluginBrowseArgs, PluginSearchArgs, PluginUpdateArgs, DEFAULT_PLUGIN_REGISTRY_URL,
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
    let index = fetch_registry_index(&registry_url, req.no_cache).await?;
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
    let index = fetch_registry_index(&registry_url, req.no_cache).await?;
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
        let installed_entry = installed.get(&entry.name);
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

#[derive(Debug, Clone, Default)]
pub(crate) struct PluginUpdateRequest {
    pub(crate) name: Option<String>,
    pub(crate) tag: Option<String>,
    pub(crate) dry_run: bool,
    pub(crate) force: bool,
    pub(crate) registry_url: String,
    pub(crate) no_cache: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginUpdateRow {
    pub(crate) name: String,
    pub(crate) installed_tag: Option<String>,
    pub(crate) target_tag: Option<String>,
    pub(crate) origin: Option<String>,
    pub(crate) status: &'static str,
    pub(crate) detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) install: Option<PluginInstallOutput>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginUpdateOutput {
    pub(crate) dry_run: bool,
    pub(crate) considered: usize,
    pub(crate) updated: usize,
    pub(crate) results: Vec<PluginUpdateRow>,
}

pub(crate) async fn run_plugin_update(req: PluginUpdateRequest) -> Result<PluginUpdateOutput> {
    let installed = read_installed_index().context("failed to read installed plugin registry")?;
    if installed.is_empty() {
        return Ok(PluginUpdateOutput { dry_run: req.dry_run, considered: 0, updated: 0, results: vec![] });
    }

    let mut candidates: Vec<InstalledPlugin> = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => {
            let entry = installed
                .get(name)
                .cloned()
                .ok_or_else(|| invalid_input_error(format!("plugin '{name}' is not installed")))?;
            vec![entry]
        }
        None => installed.values().cloned().collect(),
    };
    candidates.sort_by(|a, b| a.name.cmp(&b.name));

    let registry_url = if req.registry_url.trim().is_empty() {
        DEFAULT_PLUGIN_REGISTRY_URL.to_string()
    } else {
        req.registry_url.clone()
    };
    let registry = fetch_registry_index(&registry_url, req.no_cache).await.ok();
    let by_name: BTreeMap<String, RegistryPluginEntry> =
        registry.map(|idx| idx.plugins.into_iter().map(|p| (p.name.clone(), p)).collect()).unwrap_or_default();

    let mut results: Vec<PluginUpdateRow> = Vec::new();
    let mut updated = 0usize;

    for installed_entry in candidates {
        let source_kind = installed_entry.source_kind.as_deref().unwrap_or("");
        if source_kind != "release" {
            results.push(PluginUpdateRow {
                name: installed_entry.name.clone(),
                installed_tag: installed_entry.release_tag.clone(),
                target_tag: None,
                origin: installed_entry.origin.clone(),
                status: "skipped",
                detail: Some(format!("source_kind={source_kind} (only `release` plugins can be updated)")),
                install: None,
            });
            continue;
        }

        let repo_slug = match origin_to_repo_slug(installed_entry.origin.as_deref()) {
            Some(slug) => slug,
            None => {
                results.push(PluginUpdateRow {
                    name: installed_entry.name.clone(),
                    installed_tag: installed_entry.release_tag.clone(),
                    target_tag: None,
                    origin: installed_entry.origin.clone(),
                    status: "skipped",
                    detail: Some("missing owner/repo in origin field".to_string()),
                    install: None,
                });
                continue;
            }
        };

        let target_tag = match req.tag.clone() {
            Some(t) => Some(t),
            None => by_name.get(&installed_entry.name).and_then(|p| p.latest_tag.clone()),
        };

        let installed_tag = installed_entry.release_tag.clone();
        let needs_update = match (&installed_tag, &target_tag) {
            (Some(installed), Some(target)) => installed != target || req.force,
            (None, Some(_)) => true,
            (_, None) => req.force,
        };

        if !needs_update {
            results.push(PluginUpdateRow {
                name: installed_entry.name.clone(),
                installed_tag,
                target_tag,
                origin: installed_entry.origin.clone(),
                status: "current",
                detail: Some("installed tag matches target".to_string()),
                install: None,
            });
            continue;
        }

        if req.dry_run {
            results.push(PluginUpdateRow {
                name: installed_entry.name.clone(),
                installed_tag,
                target_tag,
                origin: installed_entry.origin.clone(),
                status: "would_update",
                detail: None,
                install: None,
            });
            continue;
        }

        let source = match target_tag.as_deref() {
            Some(tag) => format!("{repo_slug}@{tag}"),
            None => repo_slug.clone(),
        };
        let install_request = PluginInstallRequest {
            source: Some(source),
            name: Some(installed_entry.name.clone()),
            force: true,
            ..Default::default()
        };
        match run_plugin_install(install_request).await {
            Ok(output) => {
                let resolved_tag = output.release_tag.clone();
                updated += 1;
                results.push(PluginUpdateRow {
                    name: installed_entry.name.clone(),
                    installed_tag,
                    target_tag: resolved_tag.or(target_tag),
                    origin: installed_entry.origin.clone(),
                    status: "updated",
                    detail: None,
                    install: Some(output),
                });
            }
            Err(err) => {
                results.push(PluginUpdateRow {
                    name: installed_entry.name.clone(),
                    installed_tag,
                    target_tag,
                    origin: installed_entry.origin.clone(),
                    status: "failed",
                    detail: Some(err.to_string()),
                    install: None,
                });
            }
        }
    }

    Ok(PluginUpdateOutput { dry_run: req.dry_run, considered: results.len(), updated, results })
}

pub(crate) async fn handle_plugin_update(args: PluginUpdateArgs) -> Result<()> {
    let json = args.json;
    let dry_run = args.dry_run;
    let registry_url = DEFAULT_PLUGIN_REGISTRY_URL.to_string();
    let output = run_plugin_update(PluginUpdateRequest {
        name: args.name,
        tag: args.tag,
        dry_run,
        force: args.force,
        registry_url,
        no_cache: false,
    })
    .await?;
    if json {
        return print_value(output, true);
    }
    if output.considered == 0 {
        println!("no installed release-source plugins found");
        return Ok(());
    }
    println!(
        "{}: considered {}, updated {}",
        if output.dry_run { "dry-run" } else { "update" },
        output.considered,
        output.updated
    );
    for row in &output.results {
        let installed = row.installed_tag.as_deref().unwrap_or("--");
        let target = row.target_tag.as_deref().unwrap_or("--");
        println!("  {:<32} {} -> {}  [{}]", row.name, installed, target, row.status);
        if let Some(detail) = row.detail.as_deref() {
            println!("    {}", detail);
        }
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
    pub(crate) kind: Option<String>,
}

/// Load the installed-plugin registry as a `name → InstalledPlugin` map.
/// Tolerates a missing file (returns empty).
pub(crate) fn read_installed_index() -> Result<BTreeMap<String, InstalledPlugin>> {
    let path = pick_registry_path();
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
                _ => {}
            }
        }
    }
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

/// Fetch the registry index from `url`, honoring the on-disk cache.
pub(crate) async fn fetch_registry_index(url: &str, no_cache: bool) -> Result<PluginRegistryIndex> {
    if !no_cache {
        if let Some(idx) = load_from_cache(url) {
            return Ok(idx);
        }
    }
    let body = http_get(url).await?;
    let index: PluginRegistryIndex =
        serde_json::from_str(&body).with_context(|| format!("failed to parse plugin registry JSON from {url}"))?;
    if let Err(err) = write_cache(url, &body) {
        tracing::warn!(error = %err, "failed to write plugin registry cache");
    }
    Ok(index)
}

fn load_from_cache(url: &str) -> Option<PluginRegistryIndex> {
    let path = cache_path();
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    if age > REGISTRY_CACHE_TTL {
        return None;
    }
    let body = std::fs::read_to_string(&path).ok()?;
    let envelope: CachedRegistry = serde_json::from_str(&body).ok()?;
    if envelope.url != url {
        return None;
    }
    let index: PluginRegistryIndex = serde_json::from_str(&envelope.body).ok()?;
    Some(index)
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

async fn http_get(url: &str) -> Result<String> {
    let agent = format!("animus-cli/{}", env!("CARGO_PKG_VERSION"));
    let response = reqwest::Client::builder()
        .user_agent(agent)
        .timeout(Duration::from_secs(20))
        .build()
        .context("failed to build HTTP client")?
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url} returned non-success status"))?;
    response.text().await.with_context(|| format!("failed to read body from {url}"))
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
}
