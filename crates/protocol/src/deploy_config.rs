use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for ao-cloud deployments on Fly.io.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct DeployConfig {
    /// Fly.io API token for authentication
    pub fly_token: Option<String>,
    /// Application name on Fly.io
    pub app_name: Option<String>,
    /// Fly.io organization ID
    pub org: Option<String>,
    /// Deployment region
    pub region: Option<String>,
    /// Deployment status (active, inactive, etc.)
    pub status: Option<String>,
    /// Last deployment timestamp
    pub last_deployed_at: Option<String>,
    /// Machine IDs for the deployment
    pub machine_ids: Vec<String>,
}

impl DeployConfig {
    pub fn load_global() -> Self {
        let path = Self::global_path();
        Self::try_load_from(&path).unwrap_or_default()
    }

    pub fn load_for_project(project_root: &str) -> Self {
        let project_path = PathBuf::from(project_root).join(".ao").join("deploy.json");
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
        let path = PathBuf::from(project_root).join(".ao").join("deploy.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    fn merge_with_global(self) -> Self {
        let global = Self::load_global();
        DeployConfig {
            fly_token: self.fly_token.or(global.fly_token),
            app_name: self.app_name.or(global.app_name),
            org: self.org.or(global.org),
            region: self.region.or(global.region),
            status: self.status.or(global.status),
            last_deployed_at: self.last_deployed_at,
            machine_ids: if self.machine_ids.is_empty() { global.machine_ids } else { self.machine_ids },
        }
    }

    fn global_path() -> PathBuf {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".ao").join("deploy.json");
        }
        PathBuf::from(".ao").join("deploy.json")
    }

    fn try_load_from(path: &PathBuf) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn is_configured(&self) -> bool {
        self.fly_token.is_some() && self.app_name.is_some()
    }

    pub fn fly_token(&self) -> anyhow::Result<String> {
        self.fly_token.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "Fly.io token not configured. Provide --fly-token or set via: ao cloud deploy --fly-token <token>"
            )
        })
    }

    pub fn app_name(&self) -> anyhow::Result<String> {
        self.app_name.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "Application name not configured. Provide --app-name or set via: ao cloud deploy --app-name <name>"
            )
        })
    }
}
