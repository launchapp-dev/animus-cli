use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct SyncConfig {
    pub server: Option<String>,
    pub token: Option<String>,
    pub project_id: Option<String>,
    pub last_synced_at: Option<String>,
}

impl SyncConfig {
    pub fn load_global() -> Self {
        let path = Self::global_path();
        Self::try_load_from(&path).unwrap_or_default()
    }

    pub fn load_for_project(project_root: &str) -> Self {
        let project_path = PathBuf::from(project_root).join(".ao").join("sync.json");
        if let Some(project_config) = Self::try_load_from(&project_path) {
            return project_config.merge_with_global();
        }
        Self::load_global()
    }

    pub fn save_global(&self) -> anyhow::Result<()> {
        let path = Self::global_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    pub fn save_for_project(&self, project_root: &str) -> anyhow::Result<()> {
        let path = PathBuf::from(project_root).join(".ao").join("sync.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    fn merge_with_global(self) -> Self {
        let global = Self::load_global();
        SyncConfig {
            server: self.server.or(global.server),
            token: self.token.or(global.token),
            project_id: self.project_id.or(global.project_id),
            last_synced_at: self.last_synced_at.or(global.last_synced_at),
        }
    }

    fn global_path() -> PathBuf {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".ao").join("sync.json");
        }
        PathBuf::from(".ao").join("sync.json")
    }

    fn try_load_from(path: &PathBuf) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn is_configured(&self) -> bool {
        self.server.is_some() && self.token.is_some()
    }

    pub fn server_url(&self) -> anyhow::Result<String> {
        self.server.clone().ok_or_else(|| {
            anyhow::anyhow!("Sync server not configured. Run: ao sync setup --server <url> --token <token>")
        })
    }

    pub fn bearer_token(&self) -> anyhow::Result<String> {
        self.token.clone().ok_or_else(|| {
            anyhow::anyhow!("Sync token not configured. Run: ao sync setup --server <url> --token <token>")
        })
    }
}
