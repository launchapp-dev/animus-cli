use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

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
        Self::load_from_dir(&Self::global_config_dir())
    }

    pub fn load_from_dir(config_dir: &Path) -> Result<Self> {
        fs::create_dir_all(config_dir).with_context(|| {
            format!("Failed to create config directory {}", config_dir.display())
        })?;
        Self::load_or_initialize(&config_dir.join("config.json"))
    }

    pub fn load_or_default(project_root: &str) -> Result<Self> {
        let config_path = Self::config_path(project_root)?;
        Self::load_or_initialize(&config_path)
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

    fn load_or_initialize(config_path: &Path) -> Result<Self> {
        if config_path.exists() {
            let content = fs::read_to_string(config_path)?;
            return serde_json::from_str(&content).context("Failed to parse config file");
        }

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let default_config = Self {
            agent_runner_token: None,
        };
        let json = serde_json::to_string_pretty(&default_config)?;
        fs::write(config_path, json)?;
        Ok(default_config)
    }

    pub fn ensure_token_exists(config_dir: &Path) -> Result<()> {
        let config_path = config_dir.join("config.json");
        let mut config = Self::load_from_dir(config_dir)?;
        if config
            .agent_runner_token
            .as_deref()
            .map_or(true, |t| t.trim().is_empty())
        {
            config.agent_runner_token = Some(Uuid::new_v4().to_string());
            let json = serde_json::to_string_pretty(&config)?;
            fs::write(&config_path, json)
                .with_context(|| format!("Failed to write token to {}", config_path.display()))?;
        }
        Ok(())
    }

    pub fn get_token(&self) -> Result<String> {
        if let Ok(token) = std::env::var("AGENT_RUNNER_TOKEN") {
            return normalize_token("AGENT_RUNNER_TOKEN", token);
        }

        normalize_token(
            "agent_runner_token",
            self.agent_runner_token.clone().unwrap_or_default(),
        )
    }
}

fn normalize_token(source: &str, raw: String) -> Result<String> {
    let token = raw.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("{source} is missing or empty");
    }
    Ok(token)
}

fn config_dir_override() -> Option<PathBuf> {
    std::env::var("AO_CONFIG_DIR")
        .ok()
        .or_else(|| std::env::var("AGENT_ORCHESTRATOR_CONFIG_DIR").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
