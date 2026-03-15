//! Configuration management for CLI wrapper

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Directory for test workspaces
    pub test_workspace_dir: PathBuf,

    /// Default timeout for CLI operations (seconds)
    pub default_timeout_secs: u64,

    /// Enable verbose logging
    pub verbose: bool,

    /// Custom CLI configurations
    #[serde(default)]
    pub custom_clis: Vec<CustomCliConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomCliConfig {
    pub name: String,
    pub executable_path: PathBuf,
    pub auth_command: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            test_workspace_dir: std::env::temp_dir().join("llm-cli-wrapper-tests"),
            default_timeout_secs: 300,
            verbose: false,
            custom_clis: Vec::new(),
        }
    }
}

impl Config {
    pub fn load_from_file(path: &PathBuf) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}
