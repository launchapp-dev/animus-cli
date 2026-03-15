//! CLI registry for discovering and managing installed CLIs

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};
use which::which;

use super::claude::ClaudeCli;
use super::codex::CodexCli;
use super::gemini::GeminiCli;
use super::interface::CliInterface;
use super::oai_runner::OaiRunnerCli;
use super::opencode::OpenCodeCli;
use super::types::{CliMetadata, CliStatus, CliType};
use crate::error::{Error, Result};

/// Registry of discovered and configured CLIs
pub struct CliRegistry {
    clis: HashMap<CliType, Arc<dyn CliInterface>>,
}

impl CliRegistry {
    pub fn new() -> Self {
        Self { clis: HashMap::new() }
    }

    /// Discover all available CLIs on the system
    pub async fn discover_clis(&mut self) -> Result<usize> {
        info!("Discovering installed CLIs...");

        let mut discovered = 0;

        // Try to discover each CLI type
        for cli_type in
            [CliType::Claude, CliType::Codex, CliType::Gemini, CliType::OpenCode, CliType::OaiRunner, CliType::Aider]
        {
            if let Ok(path) = which(cli_type.executable_name()) {
                info!("Found {} at {:?}", cli_type.display_name(), path);

                let cli = self.create_cli_instance(cli_type, path).await?;
                self.clis.insert(cli_type, cli);
                discovered += 1;
            } else {
                warn!("{} not found in PATH", cli_type.display_name());
            }
        }

        info!("Discovered {} CLIs", discovered);
        Ok(discovered)
    }

    /// Register a CLI manually
    pub fn register(&mut self, cli: Arc<dyn CliInterface>) {
        let cli_type = cli.metadata().cli_type;
        self.clis.insert(cli_type, cli);
    }

    /// Get a CLI by type
    pub fn get(&self, cli_type: CliType) -> Option<Arc<dyn CliInterface>> {
        self.clis.get(&cli_type).cloned()
    }

    /// Get all registered CLIs
    pub fn all(&self) -> Vec<Arc<dyn CliInterface>> {
        self.clis.values().cloned().collect()
    }

    /// Check which CLIs are available and authenticated
    pub async fn check_all_status(&self) -> HashMap<CliType, CliStatus> {
        let mut statuses = HashMap::new();

        for (cli_type, cli) in &self.clis {
            let status = if !cli.is_available().await {
                CliStatus::NotInstalled
            } else if let Ok(false) = cli.check_auth().await {
                CliStatus::NotAuthenticated
            } else {
                CliStatus::Available
            };

            statuses.insert(*cli_type, status);
        }

        statuses
    }

    async fn create_cli_instance(&self, cli_type: CliType, path: PathBuf) -> Result<Arc<dyn CliInterface>> {
        let metadata = CliMetadata::new(cli_type, path);

        let cli: Arc<dyn CliInterface> = match cli_type {
            CliType::Claude => Arc::new(ClaudeCli::new(metadata)),
            CliType::Codex => Arc::new(CodexCli::new(metadata)),
            CliType::Gemini => Arc::new(GeminiCli::new(metadata)),
            CliType::OpenCode => Arc::new(OpenCodeCli::new(metadata)),
            CliType::OaiRunner => Arc::new(OaiRunnerCli::new(metadata)),
            _ => return Err(Error::CliNotFound(format!("No implementation for {:?}", cli_type))),
        };

        Ok(cli)
    }
}

impl Default for CliRegistry {
    fn default() -> Self {
        Self::new()
    }
}
