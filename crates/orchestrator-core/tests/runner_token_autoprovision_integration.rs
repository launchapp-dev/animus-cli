use anyhow::{ensure, Context, Result};
use orchestrator_core::{DaemonServiceApi, FileServiceHub};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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

fn agent_runner_binary_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "agent-runner.exe"
    }

    #[cfg(not(target_os = "windows"))]
    {
        "agent-runner"
    }
}

fn agent_runner_binary_candidate() -> Result<PathBuf> {
    let current_exe = std::env::current_exe().context("resolve current test executable path")?;
    let deps_dir = current_exe
        .parent()
        .context("resolve test executable parent directory")?;
    let debug_dir = deps_dir.parent().context("resolve target debug directory")?;
    Ok(debug_dir.join(agent_runner_binary_name()))
}

fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join("../..")
        .canonicalize()
        .context("canonicalize workspace root")
}

fn ensure_agent_runner_binary_available() -> Result<PathBuf> {
    let candidate = agent_runner_binary_candidate()?;
    if candidate.exists() {
        return Ok(candidate);
    }

    let workspace_root = workspace_root()?;
    let status = Command::new("cargo")
        .args(["build", "-p", "agent-runner"])
        .current_dir(&workspace_root)
        .status()
        .context("build agent-runner for integration test")?;
    ensure!(
        status.success(),
        "cargo build -p agent-runner failed with status {status}"
    );

    ensure!(
        candidate.exists(),
        "agent-runner binary still missing at {}",
        candidate.display()
    );
    Ok(candidate)
}

fn path_with_prepended_dir(dir: &Path) -> String {
    let dir = dir.to_string_lossy();
    let existing = std::env::var("PATH").unwrap_or_default();
    if existing.is_empty() {
        return dir.to_string();
    }
    let separator = if cfg!(windows) { ";" } else { ":" };
    format!("{dir}{separator}{existing}")
}

#[tokio::test]
async fn daemon_start_autoprovisions_runner_token_when_missing() -> Result<()> {
    let _env_lock = env_lock()
        .lock()
        .expect("env lock should not be poisoned for runner startup integration test");
    let runner_binary = ensure_agent_runner_binary_available()?;

    let temp = tempfile::tempdir().context("create temp root for integration test")?;
    let project_root = temp.path().join("project");
    let home_dir = temp.path().join("home");
    let runner_config_dir = temp.path().join("runner-config");
    std::fs::create_dir_all(&project_root).context("create temp project root")?;
    std::fs::create_dir_all(&home_dir).context("create temp home directory")?;

    let runner_config_value = runner_config_dir.to_string_lossy().to_string();
    let runner_path_value = path_with_prepended_dir(
        runner_binary
            .parent()
            .context("resolve agent-runner parent directory")?,
    );
    let _path_guard = EnvVarGuard::set_optional("PATH", Some(&runner_path_value));
    let home_value = home_dir.to_string_lossy().to_string();
    let _home_guard = EnvVarGuard::set_optional("HOME", Some(&home_value));
    let _xdg_guard = EnvVarGuard::set_optional("XDG_CONFIG_HOME", Some(&home_value));
    let _runner_config_guard =
        EnvVarGuard::set_optional("AO_RUNNER_CONFIG_DIR", Some(&runner_config_value));
    let _skip_guard = EnvVarGuard::set_optional("AO_SKIP_RUNNER_START", Some("0"));
    let _token_guard = EnvVarGuard::set_optional("AGENT_RUNNER_TOKEN", None);

    let config_path = runner_config_dir.join("config.json");
    ensure!(
        !config_path.exists(),
        "test precondition failed: expected no existing runner config at {}",
        config_path.display()
    );

    let hub = FileServiceHub::new(&project_root).context("create file service hub")?;
    if let Err(start_error) = DaemonServiceApi::start(&hub).await {
        let _ = DaemonServiceApi::stop(&hub).await;
        return Err(start_error).context("daemon start should launch agent-runner");
    }

    let checks = async {
        let health = DaemonServiceApi::health(&hub)
            .await
            .context("query daemon health after startup")?;
        ensure!(
            health.runner_connected,
            "runner should be connected after startup; health={health:?}"
        );
        ensure!(
            health.runner_pid.is_some(),
            "runner pid should be recorded after startup"
        );

        let config = protocol::Config::load_from_dir(&runner_config_dir)
            .context("load runner config after startup")?;
        let token = config
            .agent_runner_token
            .context("runner token should be generated and persisted")?;
        ensure!(
            !token.trim().is_empty(),
            "generated runner token should not be empty"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;

    let stop_result = DaemonServiceApi::stop(&hub).await;
    checks?;
    stop_result.context("stop daemon and agent-runner after integration test")?;

    Ok(())
}
