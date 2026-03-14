use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::services::ServiceHub;

use crate::{connect_runner, runner_config_dir, unavailable_error};

#[cfg(unix)]
pub(super) async fn connect_runner_for_agent_command(
    hub: &Arc<dyn ServiceHub>,
    project_root: &str,
    start_runner: bool,
) -> Result<tokio::net::UnixStream> {
    if start_runner {
        hub.daemon().start(Default::default()).await?;
    }
    let config_dir = runner_config_dir(Path::new(project_root));
    connect_runner(&config_dir)
        .await
        .map_err(|e| unavailable_error(e.to_string()))
}

#[cfg(not(unix))]
pub(super) async fn connect_runner_for_agent_command(
    hub: &Arc<dyn ServiceHub>,
    project_root: &str,
    start_runner: bool,
) -> Result<tokio::net::TcpStream> {
    if start_runner {
        hub.daemon().start(Default::default()).await?;
    }
    let config_dir = runner_config_dir(Path::new(project_root));
    connect_runner(&config_dir)
        .await
        .map_err(|e| unavailable_error(e.to_string()))
}
