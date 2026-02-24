use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub agent_runner_token: Option<String>,
}

impl Config {
    pub fn global_config_dir() -> PathBuf {
        if let Some(override_path) = config_dir_override() {
            return override_path;
        }

        #[cfg(target_os = "macos")]
        {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("com.launchpad.agent-orchestrator")
        }

        #[cfg(target_os = "windows")]
        {
            return dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("com.launchpad.agent-orchestrator");
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("agent-orchestrator")
        }
    }

    pub fn load_global() -> Result<Self> {
        let config_dir = Self::global_config_dir();
        fs::create_dir_all(&config_dir).ok();
        let config_path = config_dir.join("config.json");

        if config_path.exists() {
            let content = fs::read_to_string(&config_path)?;
            serde_json::from_str(&content).context("Failed to parse config file")
        } else {
            let default_config = Config {
                agent_runner_token: None,
            };
            let json = serde_json::to_string_pretty(&default_config)?;
            fs::write(&config_path, json)?;
            Ok(default_config)
        }
    }

    pub fn load_or_default(project_root: &str) -> Result<Self> {
        let config_path = Self::config_path(project_root)?;

        if config_path.exists() {
            let content = fs::read_to_string(&config_path)?;
            serde_json::from_str(&content).context("Failed to parse config file")
        } else {
            let default_config = Config {
                agent_runner_token: None,
            };
            default_config.save(project_root)?;
            Ok(default_config)
        }
    }

    pub fn save(&self, project_root: &str) -> Result<()> {
        let config_path = Self::config_path(project_root)?;

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)?;
        fs::write(&config_path, json)?;
        Ok(())
    }

    fn config_path(project_root: &str) -> Result<PathBuf> {
        let project_path = PathBuf::from(project_root)
            .canonicalize()
            .context("Invalid project root")?;
        Ok(project_path.join(".ao").join("config.json"))
    }

    pub fn get_token(&self) -> String {
        std::env::var("AGENT_RUNNER_TOKEN")
            .or_else(|_| {
                self.agent_runner_token
                    .as_ref()
                    .ok_or(anyhow::anyhow!("No token"))
                    .cloned()
            })
            .unwrap_or_else(|_| "dev-token".to_string())
    }
}

fn config_dir_override() -> Option<PathBuf> {
    std::env::var("AO_CONFIG_DIR")
        .ok()
        .or_else(|| std::env::var("AGENT_ORCHESTRATOR_CONFIG_DIR").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
