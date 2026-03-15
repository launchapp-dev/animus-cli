use async_trait::async_trait;
use tracing::debug;

use super::interface::{CliCommand, CliInterface, CliOutput};
use super::types::CliMetadata;
use crate::error::{Error, Result};

pub struct OaiRunnerCli {
    metadata: CliMetadata,
}

impl OaiRunnerCli {
    pub fn new(metadata: CliMetadata) -> Self {
        Self { metadata }
    }
}

#[async_trait]
impl CliInterface for OaiRunnerCli {
    fn metadata(&self) -> &CliMetadata {
        &self.metadata
    }

    async fn is_available(&self) -> bool {
        self.get_version().await.is_ok()
    }

    async fn check_auth(&self) -> Result<bool> {
        Ok(self.get_version().await.is_ok())
    }

    async fn get_version(&self) -> Result<String> {
        let output = self.run_process(&["--version"], None, &[], Some(10)).await?;

        if output.is_success() {
            Ok(output.stdout.trim().to_string())
        } else {
            Err(Error::ExecutionFailed("Failed to get ao-oai-runner version".to_string()))
        }
    }

    async fn execute(&self, command: &CliCommand) -> Result<CliOutput> {
        debug!("Executing ao-oai-runner command: {}", command.prompt);

        let mut args = vec!["run".to_string()];

        if let Some(model) = command.model_for_cli("oai-runner") {
            args.push("-m".to_string());
            args.push(model.to_string());
        }

        args.push("--format".to_string());
        args.push("json".to_string());
        args.push(command.prompt.clone());

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        self.run_process(&args_refs, command.working_dir.as_ref(), &command.env_vars, command.timeout_secs).await
    }
}
