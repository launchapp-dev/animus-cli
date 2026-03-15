//! Google Gemini CLI implementation

use async_trait::async_trait;
use tracing::debug;

use super::interface::{CliCommand, CliInterface, CliOutput};
use super::types::CliMetadata;
use crate::error::{Error, Result};

pub struct GeminiCli {
    metadata: CliMetadata,
}

impl GeminiCli {
    pub fn new(metadata: CliMetadata) -> Self {
        Self { metadata }
    }
}

#[async_trait]
impl CliInterface for GeminiCli {
    fn metadata(&self) -> &CliMetadata {
        &self.metadata
    }

    async fn is_available(&self) -> bool {
        self.get_version().await.is_ok()
    }

    async fn check_auth(&self) -> Result<bool> {
        // Gemini stores auth in config, just check if it's available
        // If --version succeeds, CLI is properly configured
        Ok(self.get_version().await.is_ok())
    }

    async fn get_version(&self) -> Result<String> {
        let output = self.run_process(&["--version"], None, &[], Some(10)).await?;

        if output.is_success() {
            Ok(output.stdout.trim().to_string())
        } else {
            Err(Error::ExecutionFailed("Failed to get Gemini version".to_string()))
        }
    }

    async fn execute(&self, command: &CliCommand) -> Result<CliOutput> {
        debug!("Executing Gemini command: {}", command.prompt);

        let mut args: Vec<String> = vec![];

        if let Some(model) = command.model_for_cli("gemini") {
            args.push("--model".to_string());
            args.push(model.to_string());
        }

        // Add prompt using -p flag for non-interactive mode
        args.push("-p".to_string());
        args.push(command.prompt.clone());

        // Add files if specified
        for file in &command.files {
            args.push(format!("--file={}", file.display()));
        }

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        self.run_process(&args_refs, command.working_dir.as_ref(), &command.env_vars, command.timeout_secs).await
    }
}
