//! Claude Code CLI implementation

use async_trait::async_trait;
use tracing::debug;

use super::interface::{CliCommand, CliInterface, CliOutput};
use super::types::CliMetadata;
use crate::error::{Error, Result};

pub struct ClaudeCli {
    metadata: CliMetadata,
}

impl ClaudeCli {
    pub fn new(metadata: CliMetadata) -> Self {
        Self { metadata }
    }
}

#[async_trait]
impl CliInterface for ClaudeCli {
    fn metadata(&self) -> &CliMetadata {
        &self.metadata
    }

    async fn is_available(&self) -> bool {
        self.get_version().await.is_ok()
    }

    async fn check_auth(&self) -> Result<bool> {
        // Claude Code stores auth in config, just check if version works
        // If --version succeeds, CLI is properly configured
        self.get_version()
            .await
            .is_ok()
            .then_some(true)
            .ok_or(Error::AuthenticationRequired(
                "Claude not configured".to_string(),
            ))
    }

    async fn get_version(&self) -> Result<String> {
        let output = self
            .run_process(&["--version"], None, &[], Some(10))
            .await?;

        if output.is_success() {
            Ok(output.stdout.trim().to_string())
        } else {
            Err(Error::ExecutionFailed(
                "Failed to get Claude version".to_string(),
            ))
        }
    }

    async fn execute(&self, command: &CliCommand) -> Result<CliOutput> {
        debug!("Executing Claude command: {}", command.prompt);

        let mut args: Vec<String> = vec![
            "--print".to_string(),
            "--no-session-persistence".to_string(),
        ];

        if let Some(model) = command.model_for_cli("claude") {
            args.push("--model".to_string());
            args.push(model.to_string());
        }

        // Add prompt
        args.push(command.prompt.clone());

        // Add files if specified
        for file in &command.files {
            args.push(file.to_string_lossy().to_string());
        }

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        self.run_process(
            &args_refs,
            command.working_dir.as_ref(),
            &command.env_vars,
            command.timeout_secs,
        )
        .await
    }
}
