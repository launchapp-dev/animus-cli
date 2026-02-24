use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProjectRegistry {
    #[serde(default)]
    entries: Vec<ProjectRegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ProjectRegistryEntry {
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) name: String,
    pub(super) path: String,
    #[serde(default)]
    pub(super) last_opened_at: Option<String>,
    #[serde(default)]
    pub(super) pinned: bool,
    #[serde(default)]
    pub(super) archived: bool,
    #[serde(default)]
    pub(super) runtime_paused: bool,
    #[serde(default)]
    pub(super) daemon_pid: Option<u32>,
}

fn project_registry_path() -> PathBuf {
    protocol::Config::global_config_dir().join("projects.json")
}

fn load_project_registry() -> Result<ProjectRegistry> {
    let path = project_registry_path();
    if !path.exists() {
        return Ok(ProjectRegistry::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str::<ProjectRegistry>(&content)?)
}

fn save_project_registry(registry: &ProjectRegistry) -> Result<()> {
    let path = project_registry_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(registry)?)?;
    Ok(())
}

pub(crate) fn canonicalize_lossy(path: &str) -> String {
    let candidate = PathBuf::from(path);
    candidate
        .canonicalize()
        .unwrap_or(candidate)
        .to_string_lossy()
        .to_string()
}

pub(super) fn set_registry_runtime_paused(project_root: &str, paused: bool) -> Result<()> {
    let canonical = canonicalize_lossy(project_root);
    let mut registry = load_project_registry()?;
    let mut updated = false;
    for entry in &mut registry.entries {
        if canonicalize_lossy(&entry.path) == canonical {
            entry.runtime_paused = paused;
            entry.last_opened_at = Some(Utc::now().to_rfc3339());
            updated = true;
            break;
        }
    }

    if !updated {
        let name = PathBuf::from(&canonical)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("project")
            .to_string();
        registry.entries.push(ProjectRegistryEntry {
            id: None,
            name,
            path: canonical,
            last_opened_at: Some(Utc::now().to_rfc3339()),
            pinned: false,
            archived: false,
            runtime_paused: paused,
            daemon_pid: None,
        });
    }

    save_project_registry(&registry)
}

pub(super) fn get_registry_daemon_pid(project_root: &str) -> Result<Option<u32>> {
    let canonical = canonicalize_lossy(project_root);
    let registry = load_project_registry()?;
    Ok(registry
        .entries
        .iter()
        .find(|entry| canonicalize_lossy(&entry.path) == canonical)
        .and_then(|entry| entry.daemon_pid))
}

pub(super) fn set_registry_daemon_pid(project_root: &str, daemon_pid: Option<u32>) -> Result<()> {
    let canonical = canonicalize_lossy(project_root);
    let mut registry = load_project_registry()?;
    let mut updated = false;
    for entry in &mut registry.entries {
        if canonicalize_lossy(&entry.path) == canonical {
            entry.daemon_pid = daemon_pid;
            entry.last_opened_at = Some(Utc::now().to_rfc3339());
            updated = true;
            break;
        }
    }

    if !updated {
        let name = PathBuf::from(&canonical)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("project")
            .to_string();
        registry.entries.push(ProjectRegistryEntry {
            id: None,
            name,
            path: canonical,
            last_opened_at: Some(Utc::now().to_rfc3339()),
            pinned: false,
            archived: false,
            runtime_paused: false,
            daemon_pid,
        });
    }

    save_project_registry(&registry)
}

pub(super) fn sync_project_registry(
    primary_project_root: &str,
    include_registry: bool,
) -> Result<Vec<ProjectRegistryEntry>> {
    let canonical_primary = canonicalize_lossy(primary_project_root);
    let mut registry = if include_registry {
        load_project_registry()?
    } else {
        ProjectRegistry::default()
    };

    registry.entries.retain(|entry| {
        if entry.path.trim().is_empty() {
            return false;
        }
        let path = PathBuf::from(&entry.path);
        if path.exists() {
            true
        } else {
            entry.pinned
        }
    });

    let mut has_primary = false;
    for entry in &mut registry.entries {
        if canonicalize_lossy(&entry.path) == canonical_primary {
            has_primary = true;
            entry.archived = false;
            entry.last_opened_at = Some(Utc::now().to_rfc3339());
            break;
        }
    }

    if !has_primary {
        let name = PathBuf::from(&canonical_primary)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("project")
            .to_string();
        registry.entries.push(ProjectRegistryEntry {
            id: None,
            name,
            path: canonical_primary.clone(),
            last_opened_at: Some(Utc::now().to_rfc3339()),
            pinned: false,
            archived: false,
            runtime_paused: false,
            daemon_pid: None,
        });
    }

    save_project_registry(&registry)?;
    Ok(registry
        .entries
        .into_iter()
        .filter(|entry| !entry.archived)
        .collect())
}
