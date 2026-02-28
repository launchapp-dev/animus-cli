//! MCP Server Manager - Manage MCP server lifecycle for CLI testing

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info, warn};

const MCP_BINARY_ENV_VAR: &str = "LLM_MCP_SERVER_BINARY";
const MCP_SERVER_MANIFEST_PATH: &str = "crates/llm-mcp-server/Cargo.toml";
const MCP_BUILD_COMMAND: &str =
    "cargo build --release --manifest-path crates/llm-mcp-server/Cargo.toml";
const MCP_BUILD_REMEDIATION: &str =
    "Run `cargo build --release --manifest-path crates/llm-mcp-server/Cargo.toml` from the repository root.";
const MCP_ENDPOINT_REMEDIATION: &str =
    "Probe `GET /health`, `GET /agents`, and `GET /agents/<agent_id>` on the configured loopback port.";
const MCP_BINARY_REMEDIATION: &str =
    "Set `LLM_MCP_SERVER_BINARY` (or `with_binary`) to a valid executable, then retry startup.";

const ERROR_BINARY_MISSING: &str = "MCP_SERVER_BINARY_MISSING";
const ERROR_BUILD_FAILED: &str = "MCP_SERVER_BUILD_FAILED";
const ERROR_SPAWN_FAILED: &str = "MCP_SERVER_SPAWN_FAILED";
const ERROR_ENDPOINT_CHECK_FAILED: &str = "MCP_SERVER_ENDPOINT_CHECK_FAILED";

const EXPECTED_AGENT_IDS: [&str; 3] = ["pm", "em", "review"];

#[derive(Debug, Clone)]
struct BinaryResolution {
    selected: PathBuf,
    attempted: Vec<PathBuf>,
    source: BinarySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinarySource {
    ExplicitOverride,
    EnvironmentOverride,
    WorkspaceDefault,
}

impl BinarySource {
    fn description(self) -> &'static str {
        match self {
            BinarySource::ExplicitOverride => "with_binary override",
            BinarySource::EnvironmentOverride => "LLM_MCP_SERVER_BINARY environment variable",
            BinarySource::WorkspaceDefault => "workspace default",
        }
    }
}

/// Manager for MCP server lifecycle
pub struct McpServerManager {
    process: Option<Child>,
    port: u16,
    root_path: PathBuf,
    workspace_root: PathBuf,
    server_binary_override: Option<PathBuf>,
}

impl McpServerManager {
    /// Create a new MCP server manager
    ///
    /// # Arguments
    /// * `root_path` - Root path for the project to serve
    /// * `port` - Port to run the server on (default: 3000)
    pub fn new(root_path: PathBuf, port: u16) -> Self {
        Self {
            process: None,
            port,
            root_path,
            workspace_root: detect_workspace_root(),
            server_binary_override: None,
        }
    }

    /// Set custom server binary path
    pub fn with_binary(mut self, binary_path: PathBuf) -> Self {
        self.server_binary_override = Some(binary_path);
        self
    }

    #[cfg(test)]
    fn with_workspace_root(mut self, workspace_root: PathBuf) -> Self {
        self.workspace_root = workspace_root;
        self
    }

    /// Start the MCP server
    ///
    /// This will:
    /// 1. Spawn the MCP server process
    /// 2. Wait for it to be ready
    /// 3. Return when server is accepting connections
    pub async fn start(&mut self) -> Result<()> {
        if self.has_running_process()? {
            warn!("MCP server already running");
            return Ok(());
        }

        let resolved_binary = self.resolve_server_binary();
        info!(
            "Starting MCP server on port {} with root: {} using binary {} ({})",
            self.port,
            self.root_path.display(),
            resolved_binary.selected.display(),
            resolved_binary.source.description()
        );

        let mut server_binary = resolved_binary.selected.clone();
        if resolved_binary.source == BinarySource::WorkspaceDefault {
            if let Some(existing_binary) = resolved_binary
                .attempted
                .iter()
                .find(|path| path.exists())
                .cloned()
            {
                server_binary = existing_binary;
            }
        }

        // Build the server first if binary doesn't exist
        if !server_binary.exists() {
            info!("Building MCP server...");
            self.build_server()?;

            if resolved_binary.source == BinarySource::WorkspaceDefault {
                if let Some(existing_binary) = resolved_binary
                    .attempted
                    .iter()
                    .find(|path| path.exists())
                    .cloned()
                {
                    server_binary = existing_binary;
                }
            }

            if !server_binary.exists() {
                return Err(contract_error(
                    ERROR_BINARY_MISSING,
                    format!(
                        "resolved MCP server binary `{}` does not exist after build (attempted: {})",
                        server_binary.display(),
                        format_attempted_paths(&resolved_binary.attempted)
                    ),
                    MCP_BINARY_REMEDIATION,
                ));
            }
        }

        // Start the server process
        let child = Command::new(&server_binary)
            .arg(&self.root_path)
            .env("PORT", self.port.to_string())
            .env("RUST_LOG", "info")
            // Avoid blocking on process output by discarding logs when running as a managed child.
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                contract_error(
                    ERROR_SPAWN_FAILED,
                    format!(
                        "failed to spawn MCP server binary `{}`: {error}",
                        server_binary.display()
                    ),
                    MCP_BINARY_REMEDIATION,
                )
            })?;

        debug!("MCP server process spawned: PID {:?}", child.id());
        self.process = Some(child);

        // Wait for server to be ready
        if let Err(error) = self.wait_for_ready().await {
            let _ = self.stop();
            return Err(error);
        }

        info!("MCP server ready at http://127.0.0.1:{}", self.port);
        Ok(())
    }

    /// Stop the MCP server
    pub fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.process.take() {
            info!("Stopping MCP server");
            match child
                .try_wait()
                .context("Failed to inspect MCP server process")?
            {
                Some(status) => {
                    debug!("MCP server already exited with status {status}");
                }
                None => {
                    child.kill().context("Failed to kill MCP server process")?;
                    child.wait().context("Failed to wait for MCP server")?;
                }
            }
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

    fn has_running_process(&mut self) -> Result<bool> {
        let Some(process) = self.process.as_mut() else {
            return Ok(false);
        };

        let process_status = process.try_wait().map_err(|error| {
            contract_error(
                ERROR_SPAWN_FAILED,
                format!("failed to inspect existing MCP server process before startup: {error}"),
                MCP_BINARY_REMEDIATION,
            )
        })?;

        match process_status {
            None => Ok(true),
            Some(status) => {
                info!(
                    "Clearing stale MCP server process handle before restart (exit status: {status})"
                );
                self.process.take();
                Ok(false)
            }
        }
    }

    fn resolve_server_binary(&self) -> BinaryResolution {
        let mut attempted = Vec::new();
        let default_path = self.default_server_binary_path();
        let legacy_path = self.legacy_default_server_binary_path();
        push_unique_path(&mut attempted, default_path.clone());
        push_unique_path(&mut attempted, legacy_path);

        if let Some(custom_binary) = &self.server_binary_override {
            let selected = self.resolve_candidate_path(custom_binary);
            push_unique_path(&mut attempted, selected.clone());
            if let Some(env_binary) = env_binary_override() {
                push_unique_path(&mut attempted, self.resolve_candidate_path(&env_binary));
            }

            return BinaryResolution {
                selected,
                attempted,
                source: BinarySource::ExplicitOverride,
            };
        }

        if let Some(env_binary) = env_binary_override() {
            let selected = self.resolve_candidate_path(&env_binary);
            push_unique_path(&mut attempted, selected.clone());
            return BinaryResolution {
                selected,
                attempted,
                source: BinarySource::EnvironmentOverride,
            };
        }

        BinaryResolution {
            selected: default_path,
            attempted,
            source: BinarySource::WorkspaceDefault,
        }
    }

    fn resolve_candidate_path(&self, candidate: &Path) -> PathBuf {
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.workspace_root.join(candidate)
        }
    }

    fn default_server_binary_path(&self) -> PathBuf {
        self.workspace_root
            .join(default_server_binary_relative_path())
    }

    fn legacy_default_server_binary_path(&self) -> PathBuf {
        self.workspace_root
            .join(legacy_default_server_binary_relative_path())
    }

    /// Build the MCP server binary
    fn build_server(&self) -> Result<()> {
        info!("Building MCP server from source...");
        let manifest_path = self.workspace_root.join(MCP_SERVER_MANIFEST_PATH);
        if !manifest_path.exists() {
            return Err(contract_error(
                ERROR_BUILD_FAILED,
                format!(
                    "missing MCP manifest at `{}` while preparing `{MCP_BUILD_COMMAND}`",
                    manifest_path.display()
                ),
                MCP_BUILD_REMEDIATION,
            ));
        }

        let output = Command::new("cargo")
            .current_dir(&self.workspace_root)
            .args([
                "build",
                "--release",
                "--manifest-path",
                MCP_SERVER_MANIFEST_PATH,
            ])
            .output()
            .map_err(|error| {
                contract_error(
                    ERROR_BUILD_FAILED,
                    format!("failed to run `{MCP_BUILD_COMMAND}`: {error}"),
                    MCP_BUILD_REMEDIATION,
                )
            })?;

        if !output.status.success() {
            return Err(contract_error(
                ERROR_BUILD_FAILED,
                format!(
                    "`{MCP_BUILD_COMMAND}` exited with status {} ({})",
                    output.status,
                    command_failure_excerpt(&output)
                ),
                MCP_BUILD_REMEDIATION,
            ));
        }

        info!("MCP server built successfully");
        Ok(())
    }

    /// Wait for server to be ready by polling the health endpoint
    async fn wait_for_ready(&mut self) -> Result<()> {
        let max_attempts = 30;
        let delay = Duration::from_millis(100);
        let mut last_error = None::<String>;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .context("failed to construct MCP readiness HTTP client")?;

        debug!(
            "Waiting for MCP server to satisfy readiness contract at {}",
            self.get_base_url()
        );

        for attempt in 1..=max_attempts {
            if let Some(process) = self.process.as_mut() {
                if let Some(status) = process.try_wait().map_err(|error| {
                    contract_error(
                        ERROR_ENDPOINT_CHECK_FAILED,
                        format!("failed to inspect MCP server process state: {error}"),
                        MCP_ENDPOINT_REMEDIATION,
                    )
                })? {
                    return Err(contract_error(
                        ERROR_ENDPOINT_CHECK_FAILED,
                        format!(
                            "MCP server exited before readiness checks passed (status: {status})"
                        ),
                        MCP_ENDPOINT_REMEDIATION,
                    ));
                }
            }

            match self.verify_endpoint_contract(&client).await {
                Ok(()) => {
                    debug!("MCP server is ready after {} attempts", attempt);
                    return Ok(());
                }
                Err(error) => {
                    debug!(
                        "MCP readiness check failed (attempt {}): {}",
                        attempt, error
                    );
                    last_error = Some(error.to_string());
                }
            }

            sleep(delay).await;
        }

        Err(contract_error(
            ERROR_ENDPOINT_CHECK_FAILED,
            format!(
                "MCP server failed readiness checks after {max_attempts} attempts ({})",
                last_error.unwrap_or_else(|| "unknown readiness failure".to_string())
            ),
            MCP_ENDPOINT_REMEDIATION,
        ))
    }

    async fn verify_endpoint_contract(&self, client: &reqwest::Client) -> Result<()> {
        let health_url = format!("{}/health", self.get_base_url());
        let health_payload = self
            .fetch_json_endpoint(client, &health_url, "/health")
            .await?;

        if health_payload.get("status").and_then(Value::as_str) != Some("ok") {
            return Err(contract_error(
                ERROR_ENDPOINT_CHECK_FAILED,
                format!("/health returned unexpected payload: {}", health_payload),
                MCP_ENDPOINT_REMEDIATION,
            ));
        }

        let agents_payload = self
            .fetch_json_endpoint(client, &self.get_agents_endpoint(), "/agents")
            .await?;
        let agents = agents_payload
            .get("agents")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                contract_error(
                    ERROR_ENDPOINT_CHECK_FAILED,
                    format!("/agents payload missing `agents` array: {}", agents_payload),
                    MCP_ENDPOINT_REMEDIATION,
                )
            })?;

        let selected_agent = EXPECTED_AGENT_IDS
            .iter()
            .find(|expected| agents.iter().any(|agent| agent.as_str() == Some(*expected)))
            .copied()
            .ok_or_else(|| {
                let discovered = agents
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                contract_error(
                    ERROR_ENDPOINT_CHECK_FAILED,
                    format!(
                        "/agents did not expose any expected agent ids {:?}; discovered: [{}]",
                        EXPECTED_AGENT_IDS, discovered
                    ),
                    MCP_ENDPOINT_REMEDIATION,
                )
            })?;

        let agent_details_url = format!("{}/agents/{selected_agent}", self.get_base_url());
        let agent_payload = self
            .fetch_json_endpoint(client, &agent_details_url, "/agents/:agent_id")
            .await?;

        let expected_endpoint = format!("/mcp/{selected_agent}");
        let observed_endpoint = agent_payload.get("endpoint").and_then(Value::as_str);
        if observed_endpoint != Some(expected_endpoint.as_str()) {
            return Err(contract_error(
                ERROR_ENDPOINT_CHECK_FAILED,
                format!(
                    "/agents/{selected_agent} returned endpoint {:?}, expected `{}`",
                    observed_endpoint, expected_endpoint
                ),
                MCP_ENDPOINT_REMEDIATION,
            ));
        }

        Ok(())
    }

    async fn fetch_json_endpoint(
        &self,
        client: &reqwest::Client,
        url: &str,
        endpoint_name: &str,
    ) -> Result<Value> {
        let response = client.get(url).send().await.map_err(|error| {
            contract_error(
                ERROR_ENDPOINT_CHECK_FAILED,
                format!("{endpoint_name} request failed at {url}: {error}"),
                MCP_ENDPOINT_REMEDIATION,
            )
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(contract_error(
                ERROR_ENDPOINT_CHECK_FAILED,
                format!("{endpoint_name} request at {url} returned HTTP {status}"),
                MCP_ENDPOINT_REMEDIATION,
            ));
        }

        response.json::<Value>().await.map_err(|error| {
            contract_error(
                ERROR_ENDPOINT_CHECK_FAILED,
                format!("{endpoint_name} response at {url} was not valid JSON: {error}"),
                MCP_ENDPOINT_REMEDIATION,
            )
        })
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

fn detect_workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(workspace_root) = manifest_dir.parent().and_then(Path::parent) {
        workspace_root.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

fn default_server_binary_relative_path() -> PathBuf {
    PathBuf::from("target")
        .join("release")
        .join(format!("llm-mcp-server{}", std::env::consts::EXE_SUFFIX))
}

fn legacy_default_server_binary_relative_path() -> PathBuf {
    PathBuf::from("crates")
        .join("llm-mcp-server")
        .join("target")
        .join("release")
        .join(format!("llm-mcp-server{}", std::env::consts::EXE_SUFFIX))
}

fn env_binary_override() -> Option<PathBuf> {
    std::env::var(MCP_BINARY_ENV_VAR).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    })
}

fn push_unique_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.iter().any(|existing| existing == &candidate) {
        paths.push(candidate);
    }
}

fn command_failure_excerpt(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(line) = stderr.lines().rev().find(|line| !line.trim().is_empty()) {
        return line.trim().to_string();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(line) = stdout.lines().rev().find(|line| !line.trim().is_empty()) {
        return line.trim().to_string();
    }

    "no compiler output captured".to_string()
}

fn format_attempted_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn contract_error(code: &str, reason: String, remediation: &str) -> anyhow::Error {
    anyhow!("{code}: {reason} | remediation: {remediation}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    const FAKE_CARGO_LOG_ENV_VAR: &str = "MCP_FAKE_CARGO_LOG";

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvVarGuard {
        key: String,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set_optional(key: &str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }

            Self {
                key: key.to_string(),
                previous,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(&self.key, previous);
            } else {
                std::env::remove_var(&self.key);
            }
        }
    }

    fn write_manifest(workspace_root: &Path) {
        let manifest_path = workspace_root.join(MCP_SERVER_MANIFEST_PATH);
        let manifest_parent = manifest_path
            .parent()
            .expect("manifest path should have a parent directory");
        fs::create_dir_all(manifest_parent).expect("create manifest parent directory");
        fs::write(
            manifest_path,
            "[package]\nname = \"llm-mcp-server\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .expect("write temporary manifest file");
    }

    #[cfg(unix)]
    fn write_cargo_shim(bin_dir: &Path, script_contents: &str) -> PathBuf {
        fs::create_dir_all(bin_dir).expect("create fake cargo bin directory");
        let cargo_path = bin_dir.join("cargo");
        fs::write(&cargo_path, script_contents).expect("write fake cargo shim");

        let mut permissions = fs::metadata(&cargo_path)
            .expect("read fake cargo shim metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&cargo_path, permissions).expect("set executable bit on cargo shim");

        cargo_path
    }

    #[test]
    fn test_manager_creation() {
        let temp = TempDir::new().unwrap();
        let manager = McpServerManager::new(temp.path().to_path_buf(), 3000);

        assert_eq!(manager.port, 3000);
        assert!(!manager.is_running());
        assert!(manager.server_binary_override.is_none());
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

        assert_eq!(manager.server_binary_override, Some(custom_bin));
    }

    #[test]
    fn test_binary_resolution_prefers_explicit_override_over_env() {
        let _lock = lock_env();
        let _env_guard = EnvVarGuard::set_optional(MCP_BINARY_ENV_VAR, Some("/env/path/server"));

        let temp = TempDir::new().unwrap();
        let custom = PathBuf::from("/custom/path/server");
        let manager = McpServerManager::new(temp.path().to_path_buf(), 3000).with_binary(custom);
        let resolution = manager.resolve_server_binary();

        assert_eq!(resolution.selected, PathBuf::from("/custom/path/server"));
        assert_eq!(resolution.source, BinarySource::ExplicitOverride);
        assert!(resolution
            .attempted
            .contains(&PathBuf::from("/env/path/server")));
    }

    #[test]
    fn test_binary_resolution_uses_environment_override() {
        let _lock = lock_env();
        let _env_guard = EnvVarGuard::set_optional(MCP_BINARY_ENV_VAR, Some("custom/server"));

        let temp = TempDir::new().unwrap();
        let manager = McpServerManager::new(temp.path().to_path_buf(), 3000);
        let resolution = manager.resolve_server_binary();

        assert_eq!(
            resolution.selected,
            manager.workspace_root.join("custom/server")
        );
        assert_eq!(resolution.source, BinarySource::EnvironmentOverride);
    }

    #[test]
    fn test_binary_resolution_defaults_to_workspace_binary() {
        let _lock = lock_env();
        let _env_guard = EnvVarGuard::set_optional(MCP_BINARY_ENV_VAR, None);

        let temp = TempDir::new().unwrap();
        let manager = McpServerManager::new(temp.path().to_path_buf(), 3000);
        let resolution = manager.resolve_server_binary();

        assert_eq!(resolution.selected, manager.default_server_binary_path());
        assert_eq!(resolution.source, BinarySource::WorkspaceDefault);
    }

    #[cfg(unix)]
    #[test]
    fn test_build_server_invokes_documented_command_from_workspace_root() {
        let _lock = lock_env();

        let temp = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        write_manifest(workspace.path());

        let fake_log_path = workspace.path().join("fake-cargo.log");
        let fake_bin_dir = workspace.path().join("bin");
        write_cargo_shim(
            &fake_bin_dir,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$PWD\" > \"${{{FAKE_CARGO_LOG_ENV_VAR}}}\"\nprintf '%s\\n' \"$*\" >> \"${{{FAKE_CARGO_LOG_ENV_VAR}}}\"\n",
            ),
        );

        let existing_path = std::env::var("PATH").unwrap_or_default();
        let path_value = format!("{}:{}", fake_bin_dir.display(), existing_path);
        let fake_log_value = fake_log_path.to_string_lossy().into_owned();
        let _path_guard = EnvVarGuard::set_optional("PATH", Some(path_value.as_str()));
        let _log_guard =
            EnvVarGuard::set_optional(FAKE_CARGO_LOG_ENV_VAR, Some(fake_log_value.as_str()));

        let manager = McpServerManager::new(temp.path().to_path_buf(), 3900)
            .with_workspace_root(workspace.path().to_path_buf());
        manager
            .build_server()
            .expect("build_server should invoke fake cargo shim");

        let log_contents = fs::read_to_string(&fake_log_path).expect("read fake cargo log");
        let mut lines = log_contents.lines();
        let recorded_working_dir = lines.next().expect("recorded working directory");
        let recorded_args = lines.next().expect("recorded cargo arguments");

        let expected_working_dir =
            fs::canonicalize(workspace.path()).expect("canonicalize expected working directory");
        let observed_working_dir = fs::canonicalize(recorded_working_dir)
            .expect("canonicalize recorded working directory");
        assert_eq!(observed_working_dir, expected_working_dir);
        assert_eq!(
            recorded_args,
            "build --release --manifest-path crates/llm-mcp-server/Cargo.toml"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_build_server_failure_contains_contract_code_and_remediation() {
        let _lock = lock_env();

        let temp = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        write_manifest(workspace.path());

        let fake_bin_dir = workspace.path().join("bin");
        write_cargo_shim(
            &fake_bin_dir,
            "#!/bin/sh\necho 'simulated compile failure' >&2\nexit 23\n",
        );

        let existing_path = std::env::var("PATH").unwrap_or_default();
        let path_value = format!("{}:{}", fake_bin_dir.display(), existing_path);
        let _path_guard = EnvVarGuard::set_optional("PATH", Some(path_value.as_str()));

        let manager = McpServerManager::new(temp.path().to_path_buf(), 3901)
            .with_workspace_root(workspace.path().to_path_buf());
        let error = manager
            .build_server()
            .expect_err("build_server should return a structured error on failure");
        let message = error.to_string();

        assert!(message.contains(ERROR_BUILD_FAILED), "{message}");
        assert!(message.contains(MCP_BUILD_COMMAND), "{message}");
        assert!(message.contains("simulated compile failure"), "{message}");
        assert!(message.contains(MCP_BUILD_REMEDIATION), "{message}");
    }

    #[tokio::test]
    async fn test_verify_endpoint_contract_returns_structured_error_when_expected_agents_missing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener for endpoint contract test");
        let port = listener
            .local_addr()
            .expect("read local endpoint contract test address")
            .port();

        let server_task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };

                let mut request_buffer = [0_u8; 2048];
                let Ok(bytes_read) = stream.read(&mut request_buffer).await else {
                    continue;
                };
                if bytes_read == 0 {
                    continue;
                }

                let request = String::from_utf8_lossy(&request_buffer[..bytes_read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");

                let (status, body) = match path {
                    "/health" => ("200 OK", r#"{"status":"ok"}"#),
                    "/agents" => ("200 OK", r#"{"agents":["unknown"],"count":1}"#),
                    _ => ("404 Not Found", r#"{"error":"not found"}"#),
                };

                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let temp = TempDir::new().unwrap();
        let manager = McpServerManager::new(temp.path().to_path_buf(), port);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .expect("create reqwest client for contract validation test");

        let error = manager
            .verify_endpoint_contract(&client)
            .await
            .expect_err("endpoint contract validation should fail for unexpected agent set");
        let message = error.to_string();
        assert!(message.contains(ERROR_ENDPOINT_CHECK_FAILED), "{message}");
        assert!(
            message.contains("did not expose any expected agent ids"),
            "{message}"
        );
        assert!(message.contains(MCP_ENDPOINT_REMEDIATION), "{message}");

        server_task.abort();
    }

    #[tokio::test]
    async fn test_start_returns_structured_error_when_workspace_manifest_is_missing() {
        let _lock = lock_env();
        let _env_guard = EnvVarGuard::set_optional(MCP_BINARY_ENV_VAR, None);

        let temp = TempDir::new().unwrap();
        let fake_workspace = TempDir::new().unwrap();
        let mut manager = McpServerManager::new(temp.path().to_path_buf(), 3000)
            .with_workspace_root(fake_workspace.path().to_path_buf())
            .with_binary(fake_workspace.path().join("missing-mcp-binary"));

        let error = manager
            .start()
            .await
            .expect_err("startup should fail when manifest is missing");
        let message = error.to_string();

        assert!(message.contains(ERROR_BUILD_FAILED), "{message}");
        assert!(message.contains(MCP_BUILD_COMMAND), "{message}");
    }

    #[tokio::test]
    async fn test_start_clears_stale_process_handle_before_restarting() {
        let _lock = lock_env();
        let _env_guard = EnvVarGuard::set_optional(MCP_BINARY_ENV_VAR, None);

        let temp = TempDir::new().unwrap();
        let fake_workspace = TempDir::new().unwrap();
        let mut manager = McpServerManager::new(temp.path().to_path_buf(), 3000)
            .with_workspace_root(fake_workspace.path().to_path_buf())
            .with_binary(fake_workspace.path().join("missing-mcp-binary"));

        let mut exited_process = Command::new("cargo")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cargo --version for stale process handle test");
        exited_process
            .wait()
            .expect("wait for cargo --version to exit");
        manager.process = Some(exited_process);

        let error = manager
            .start()
            .await
            .expect_err("startup should fail after stale process cleanup");
        let message = error.to_string();

        assert!(message.contains(ERROR_BUILD_FAILED), "{message}");
        assert!(
            manager.process.is_none(),
            "stale process handle should be cleared"
        );
    }
}
