use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const MARKETPLACE_STATE_FILE: &str = "pack-marketplaces.v1.json";
const MARKETPLACE_CACHE_DIR: &str = "marketplace-cache";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceEntry {
    pub id: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceState {
    pub registries: Vec<MarketplaceEntry>,
}

impl Default for MarketplaceState {
    fn default() -> Self {
        Self { registries: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceManifest {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub plugins: Vec<MarketplacePackEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplacePackEntry {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub source: Option<serde_json::Value>,
}

fn marketplace_state_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ao")
        .join("state")
}

fn marketplace_cache_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ao")
        .join(MARKETPLACE_CACHE_DIR)
}

pub fn load_marketplace_state() -> Result<MarketplaceState> {
    let path = marketplace_state_dir().join(MARKETPLACE_STATE_FILE);
    if !path.exists() {
        return Ok(MarketplaceState::default());
    }
    let content = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn save_marketplace_state(state: &MarketplaceState) -> Result<()> {
    let dir = marketplace_state_dir();
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join(MARKETPLACE_STATE_FILE);
    let content = serde_json::to_string_pretty(state)?;
    fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))
}

pub fn add_marketplace_registry(id: &str, url: &str) -> Result<()> {
    let mut state = load_marketplace_state()?;
    if state.registries.iter().any(|r| r.id == id) {
        state.registries.iter_mut().filter(|r| r.id == id).for_each(|r| {
            r.url = url.to_string();
            r.last_synced = None;
        });
    } else {
        state.registries.push(MarketplaceEntry {
            id: id.to_string(),
            url: url.to_string(),
            last_synced: None,
        });
    }
    save_marketplace_state(&state)?;
    sync_registry(id, url)?;
    Ok(())
}

pub fn remove_marketplace_registry(id: &str) -> Result<()> {
    let mut state = load_marketplace_state()?;
    let before = state.registries.len();
    state.registries.retain(|r| r.id != id);
    if state.registries.len() == before {
        return Err(anyhow!("registry '{}' not found", id));
    }
    save_marketplace_state(&state)?;
    let cache = marketplace_cache_dir().join(id);
    if cache.exists() {
        fs::remove_dir_all(&cache).ok();
    }
    Ok(())
}

pub fn sync_registry(id: &str, url: &str) -> Result<()> {
    let cache_dir = marketplace_cache_dir();
    fs::create_dir_all(&cache_dir)?;
    let target = cache_dir.join(id);

    if target.exists() {
        let status = Command::new("git")
            .args(["pull", "--ff-only"])
            .current_dir(&target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {}
            _ => {
                fs::remove_dir_all(&target).ok();
                git_clone(url, &target)?;
            }
        }
    } else {
        git_clone(url, &target)?;
    }

    let mut state = load_marketplace_state()?;
    let now = chrono_timestamp();
    for entry in state.registries.iter_mut() {
        if entry.id == id {
            entry.last_synced = Some(now.clone());
        }
    }
    save_marketplace_state(&state)?;
    Ok(())
}

pub fn sync_all_registries() -> Result<Vec<String>> {
    let state = load_marketplace_state()?;
    let mut synced = Vec::new();
    for entry in &state.registries {
        sync_registry(&entry.id, &entry.url)?;
        synced.push(entry.id.clone());
    }
    Ok(synced)
}

fn git_clone(url: &str, target: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["clone", "--depth", "1", url, &target.display().to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("failed to run git clone for {}", url))?;
    if !status.success() {
        return Err(anyhow!("git clone failed for {}", url));
    }
    Ok(())
}

pub fn load_marketplace_manifest(registry_id: &str) -> Result<Option<MarketplaceManifest>> {
    let manifest_path = marketplace_cache_dir()
        .join(registry_id)
        .join(".claude-plugin")
        .join("marketplace.json");
    if !manifest_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&manifest_path)?;
    let manifest: MarketplaceManifest = serde_json::from_str(&content)?;
    Ok(Some(manifest))
}

pub fn search_marketplace_packs(
    query: Option<&str>,
    category: Option<&str>,
    registry_filter: Option<&str>,
) -> Result<Vec<MarketplaceSearchResult>> {
    let state = load_marketplace_state()?;
    let mut results = Vec::new();

    for entry in &state.registries {
        if let Some(filter) = registry_filter {
            if entry.id != filter {
                continue;
            }
        }
        if let Some(manifest) = load_marketplace_manifest(&entry.id)? {
            for pack in &manifest.plugins {
                let matches_query = query
                    .map(|q| {
                        let q = q.to_lowercase();
                        pack.name.to_lowercase().contains(&q)
                            || pack.description.as_deref().unwrap_or("").to_lowercase().contains(&q)
                    })
                    .unwrap_or(true);
                let matches_category = category
                    .map(|c| pack.category.as_deref().unwrap_or("").eq_ignore_ascii_case(c))
                    .unwrap_or(true);
                if matches_query && matches_category {
                    results.push(MarketplaceSearchResult {
                        registry_id: entry.id.clone(),
                        name: pack.name.clone(),
                        description: pack.description.clone(),
                        version: pack.version.clone(),
                        category: pack.category.clone(),
                        source: pack.source.clone(),
                    });
                }
            }
        }
    }
    Ok(results)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceSearchResult {
    pub registry_id: String,
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<serde_json::Value>,
}

pub fn resolve_marketplace_pack_url(registry_id: &str, pack_name: &str) -> Result<String> {
    let manifest = load_marketplace_manifest(registry_id)?
        .ok_or_else(|| anyhow!("registry '{}' not synced or has no manifest", registry_id))?;
    let pack = manifest
        .plugins
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(pack_name))
        .ok_or_else(|| anyhow!("pack '{}' not found in registry '{}'", pack_name, registry_id))?;

    if let Some(source) = &pack.source {
        if let Some(url) = source.get("url").and_then(|v| v.as_str()) {
            return Ok(url.to_string());
        }
        if let Some(source_str) = source.as_str() {
            return Err(anyhow!(
                "pack '{}' has local source '{}'; remote install not supported",
                pack_name,
                source_str
            ));
        }
    }

    let state = load_marketplace_state()?;
    let registry = state
        .registries
        .iter()
        .find(|r| r.id == registry_id)
        .ok_or_else(|| anyhow!("registry '{}' not found", registry_id))?;

    let base_url = registry.url.trim_end_matches(".git");
    Ok(format!("{}.git", base_url))
}

pub fn clone_marketplace_pack(registry_id: &str, pack_name: &str) -> Result<PathBuf> {
    let url = resolve_marketplace_pack_url(registry_id, pack_name)?;
    let temp_dir = tempfile::tempdir().context("failed to create temp directory")?;
    let clone_target = temp_dir.path().join(pack_name);
    git_clone(&url, &clone_target)?;

    let persistent_dir = marketplace_cache_dir().join("pack-downloads").join(pack_name);
    if persistent_dir.exists() {
        fs::remove_dir_all(&persistent_dir).ok();
    }
    fs::create_dir_all(persistent_dir.parent().unwrap())?;
    copy_dir_recursive(&clone_target, &persistent_dir)?;
    Ok(persistent_dir)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if entry.file_name().to_string_lossy() == ".git" {
            continue;
        }
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn chrono_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    format!("{}", secs)
}
