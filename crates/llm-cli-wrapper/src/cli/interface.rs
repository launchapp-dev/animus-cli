//! Unified interface for interacting with different CLIs

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tracing::debug;

use super::types::CliMetadata;
use crate::error::{Error, Result};

/// Command to execute with a CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliCommand {
    pub prompt: String,
    pub files: Vec<PathBuf>,
    pub working_dir: Option<PathBuf>,
    pub timeout_secs: Option<u64>,
    pub env_vars: Vec<(String, String)>,
}

impl CliCommand {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            files: Vec::new(),
            working_dir: None,
            timeout_secs: Some(300), // 5 minutes default
            env_vars: Vec::new(),
        }
    }

    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    pub fn with_env(mut self, key: String, value: String) -> Self {
        self.env_vars.push((key, value));
        self
    }

    /// Resolve a model override from CLI-specific or generic env vars.
    ///
    /// Priority:
    /// 1) `<CLI>_MODEL` (e.g. `CLAUDE_MODEL`)
    /// 2) `CLI_MODEL`
    pub fn model_for_cli(&self, cli: &str) -> Option<&str> {
        let specific_key = match cli.to_ascii_lowercase().as_str() {
            "claude" => Some("CLAUDE_MODEL"),
            "codex" => Some("CODEX_MODEL"),
            "gemini" => Some("GEMINI_MODEL"),
            "opencode" => Some("OPENCODE_MODEL"),
            _ => None,
        };

        if let Some(key) = specific_key {
            if let Some((_, value)) = self.env_vars.iter().find(|(k, _)| k == key) {
                return Some(value.as_str());
            }
        }

        self.env_vars.iter().find(|(k, _)| k == "CLI_MODEL").map(|(_, value)| value.as_str())
    }
}

/// Output from CLI execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub files_modified: Vec<PathBuf>,
}

impl CliOutput {
    pub fn is_success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Trait for CLI implementations
#[async_trait]
pub trait CliInterface: Send + Sync {
    /// Get CLI metadata
    fn metadata(&self) -> &CliMetadata;

    /// Check if CLI is installed and available
    async fn is_available(&self) -> bool;

    /// Check authentication status
    async fn check_auth(&self) -> Result<bool>;

    /// Get CLI version
    async fn get_version(&self) -> Result<String>;

    /// Execute a command
    async fn execute(&self, command: &CliCommand) -> Result<CliOutput>;

    /// Test basic functionality
    async fn test_basic(&self) -> Result<()> {
        let test_cmd = CliCommand::new("echo 'test'");
        let output = self.execute(&test_cmd).await?;

        if output.is_success() {
            Ok(())
        } else {
            Err(Error::TestFailed(format!("Basic test failed with exit code: {:?}", output.exit_code)))
        }
    }

    /// Run the CLI process with given arguments
    async fn run_process(
        &self,
        args: &[&str],
        working_dir: Option<&PathBuf>,
        env_vars: &[(String, String)],
        timeout_secs: Option<u64>,
    ) -> Result<CliOutput> {
        let start = std::time::Instant::now();
        let metadata = self.metadata();

        debug!("Running {} with args: {:?}", metadata.cli_type.display_name(), args);
        debug!("Executable path: {:?}", metadata.executable_path);

        // Verify executable exists before spawning
        if !metadata.executable_path.exists() {
            return Err(Error::ExecutionFailed(format!(
                "Executable not found at path: {:?}",
                metadata.executable_path
            )));
        }

        let mut cmd = Command::new(&metadata.executable_path);
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

        if let Some(dir) = working_dir {
            debug!("Setting working directory to: {:?}", dir);
            if !dir.exists() {
                return Err(Error::ExecutionFailed(format!("Working directory does not exist: {:?}", dir)));
            }
            cmd.current_dir(dir);
        }

        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        cmd.env_remove("CLAUDECODE");
        cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");

        debug!("About to spawn command...");
        let child = cmd.spawn()?;
        debug!("Spawn successful!");

        let output = if let Some(timeout) = timeout_secs {
            match tokio::time::timeout(std::time::Duration::from_secs(timeout), child.wait_with_output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => return Err(Error::ExecutionFailed(e.to_string())),
                Err(_) => return Err(Error::ExecutionFailed(format!("Command timed out after {} seconds", timeout))),
            }
        } else {
            child.wait_with_output().await?
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(CliOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code(),
            duration_ms,
            files_modified: Vec::new(), // Will be populated by parsers
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CliCommand;
    use protocol::default_model_for_tool;

    #[test]
    fn test_model_for_cli_prefers_specific_key() {
        let cmd = CliCommand::new("test")
            .with_env("CLI_MODEL".to_string(), "fallback-model".to_string())
            .with_env("CLAUDE_MODEL".to_string(), "sonnet".to_string());

        assert_eq!(cmd.model_for_cli("claude"), Some("sonnet"));
    }

    #[test]
    fn test_model_for_cli_uses_generic_fallback() {
        let fallback_model = default_model_for_tool("codex").expect("default model for codex should be configured");
        let cmd = CliCommand::new("test").with_env("CLI_MODEL".to_string(), fallback_model.to_string());

        assert_eq!(cmd.model_for_cli("codex"), Some(fallback_model));
    }
}
