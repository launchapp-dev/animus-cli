use anyhow::{Context, Result};
use cli_wrapper::McpServerManager;
use serde_json::Value;
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};
use tempfile::TempDir;

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

fn reserve_loopback_port() -> Result<u16> {
    let listener =
        TcpListener::bind("127.0.0.1:0").context("failed to bind ephemeral loopback port")?;
    let port = listener
        .local_addr()
        .context("failed to read bound loopback address")?
        .port();
    drop(listener);
    Ok(port)
}

#[tokio::test]
async fn mcp_manager_start_validates_health_and_agent_contract() -> Result<()> {
    let _lock = env_lock().lock().expect("env lock should not be poisoned");
    let _env_guard = EnvVarGuard::set_optional("LLM_MCP_SERVER_BINARY", None);

    let workspace = TempDir::new().context("failed to create temporary workspace")?;
    let port = reserve_loopback_port()?;
    let mut manager = McpServerManager::new(workspace.path().to_path_buf(), port);
    manager.start().await?;

    let health_url = format!("{}/health", manager.get_base_url());
    let health_payload: Value = reqwest::get(&health_url)
        .await
        .context("health request failed")?
        .error_for_status()
        .context("health endpoint returned non-success status")?
        .json()
        .await
        .context("failed to decode health payload")?;
    assert_eq!(health_payload["status"], "ok");

    let agents_payload: Value = reqwest::get(manager.get_agents_endpoint())
        .await
        .context("agents request failed")?
        .error_for_status()
        .context("agents endpoint returned non-success status")?
        .json()
        .await
        .context("failed to decode agents payload")?;
    let agents = agents_payload["agents"]
        .as_array()
        .context("agents payload missing array field")?;
    let selected_agent = ["pm", "em", "review"]
        .iter()
        .find(|expected| agents.iter().any(|agent| agent.as_str() == Some(*expected)))
        .copied()
        .context("expected at least one of pm/em/review in agents payload")?;

    let details_url = format!("{}/agents/{}", manager.get_base_url(), selected_agent);
    let details_payload: Value = reqwest::get(&details_url)
        .await
        .context("agent details request failed")?
        .error_for_status()
        .context("agent details endpoint returned non-success status")?
        .json()
        .await
        .context("failed to decode agent details payload")?;
    assert_eq!(
        details_payload["endpoint"],
        format!("/mcp/{selected_agent}")
    );

    manager.stop()?;
    Ok(())
}
