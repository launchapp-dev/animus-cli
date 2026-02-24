//! MCP Server Manager - Manage MCP server lifecycle for CLI testing

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info, warn};

/// Manager for MCP server lifecycle
pub struct McpServerManager {
    process: Option<Child>,
    port: u16,
    root_path: PathBuf,
    server_binary: PathBuf,
}

impl McpServerManager {
    /// Create a new MCP server manager
    ///
    /// # Arguments
    /// * `root_path` - Root path for the project to serve
    /// * `port` - Port to run the server on (default: 3000)
    pub fn new(root_path: PathBuf, port: u16) -> Self {
        let server_binary = PathBuf::from("cli-wrapper/mcp-server/target/release/mcp-server");

        Self {
            process: None,
            port,
            root_path,
            server_binary,
        }
    }

    /// Set custom server binary path
    pub fn with_binary(mut self, binary_path: PathBuf) -> Self {
        self.server_binary = binary_path;
        self
    }

    /// Start the MCP server
    ///
    /// This will:
    /// 1. Spawn the MCP server process
    /// 2. Wait for it to be ready
    /// 3. Return when server is accepting connections
    pub async fn start(&mut self) -> Result<()> {
        if self.process.is_some() {
            warn!("MCP server already running");
            return Ok(());
        }

        info!(
            "Starting MCP server on port {} with root: {}",
            self.port,
            self.root_path.display()
        );

        // Build the server first if binary doesn't exist
        if !self.server_binary.exists() {
            info!("Building MCP server...");
            self.build_server()?;
        }

        // Start the server process
        let child = Command::new(&self.server_binary)
            .arg(self.root_path.to_str().context("Invalid path")?)
            .env("PORT", self.port.to_string())
            .env("RUST_LOG", "info")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn MCP server")?;

        debug!("MCP server process spawned: PID {:?}", child.id());
        self.process = Some(child);

        // Wait for server to be ready
        self.wait_for_ready().await?;

        info!("MCP server ready at http://127.0.0.1:{}", self.port);
        Ok(())
    }

    /// Stop the MCP server
    pub fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.process.take() {
            info!("Stopping MCP server");
            child.kill().context("Failed to kill MCP server process")?;
            child.wait().context("Failed to wait for MCP server")?;
            info!("MCP server stopped");
        }
        Ok(())
    }

    /// Check if server is running
    pub fn is_running(&self) -> bool {
        self.process.is_some()
    }

    /// Get the endpoint URL for a specific agent
    ///
    /// # Arguments
    /// * `agent_id` - Agent identifier (e.g., "pm", "em", "review")
    ///
    /// # Returns
    /// Full URL to the agent's MCP endpoint
    pub fn get_endpoint(&self, agent_id: &str) -> String {
        format!("http://127.0.0.1:{}/mcp/{}", self.port, agent_id)
    }

    /// Get the base URL for the server
    pub fn get_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Get the agents list endpoint
    pub fn get_agents_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/agents", self.port)
    }

    /// Build the MCP server binary
    fn build_server(&self) -> Result<()> {
        info!("Building MCP server from source...");

        let status = std::process::Command::new("cargo")
            .args([
                "build",
                "--release",
                "--manifest-path",
                "cli-wrapper/mcp-server/Cargo.toml",
            ])
            .status()
            .context("Failed to build MCP server")?;

        if !status.success() {
            anyhow::bail!("MCP server build failed");
        }

        info!("MCP server built successfully");
        Ok(())
    }

    /// Wait for server to be ready by polling the health endpoint
    async fn wait_for_ready(&self) -> Result<()> {
        let health_url = format!("{}/health", self.get_base_url());
        let max_attempts = 30;
        let delay = Duration::from_millis(100);

        debug!("Waiting for MCP server to be ready at {}", health_url);

        for attempt in 1..=max_attempts {
            match reqwest::get(&health_url).await {
                Ok(response) if response.status().is_success() => {
                    debug!("MCP server is ready after {} attempts", attempt);
                    return Ok(());
                }
                Ok(response) => {
                    debug!(
                        "Health check returned status {}, retrying...",
                        response.status()
                    );
                }
                Err(e) => {
                    debug!("Health check failed (attempt {}): {}", attempt, e);
                }
            }

            sleep(delay).await;
        }

        anyhow::bail!(
            "MCP server failed to become ready after {} attempts",
            max_attempts
        )
    }
}

impl Drop for McpServerManager {
    fn drop(&mut self) {
        // Attempt to clean up on drop
        if self.is_running() {
            let _ = self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_manager_creation() {
        let temp = TempDir::new().unwrap();
        let manager = McpServerManager::new(temp.path().to_path_buf(), 3000);

        assert_eq!(manager.port, 3000);
        assert!(!manager.is_running());
    }

    #[test]
    fn test_endpoint_generation() {
        let temp = TempDir::new().unwrap();
        let manager = McpServerManager::new(temp.path().to_path_buf(), 3000);

        assert_eq!(manager.get_endpoint("pm"), "http://127.0.0.1:3000/mcp/pm");
        assert_eq!(manager.get_endpoint("em"), "http://127.0.0.1:3000/mcp/em");
        assert_eq!(
            manager.get_agents_endpoint(),
            "http://127.0.0.1:3000/agents"
        );
    }

    #[test]
    fn test_custom_binary_path() {
        let temp = TempDir::new().unwrap();
        let custom_bin = PathBuf::from("/custom/path/mcp-server");

        let manager =
            McpServerManager::new(temp.path().to_path_buf(), 3000).with_binary(custom_bin.clone());

        assert_eq!(manager.server_binary, custom_bin);
    }
}
