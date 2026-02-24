use anyhow::Result;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod cleanup;
mod config;
mod ipc;
mod lock;
mod output;
mod providers;
mod runner;
mod sandbox;
mod telemetry;

use cleanup::cleanup_orphaned_clis;
use ipc::IpcServer;
use lock::acquire_runner_lock;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,agent_runner=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let pid = std::process::id();
    let config_dir = config::app_config_dir();
    info!(
        pid,
        config_dir = %config_dir.display(),
        "Agent Runner starting"
    );

    let _lock_file = match acquire_runner_lock() {
        Ok(lock) => {
            info!("Runner singleton lock acquired");
            lock
        }
        Err(e) => {
            error!(error = %e, "Failed to acquire runner lock");
            return Err(e);
        }
    };

    info!("Checking for orphaned CLI processes from previous sessions");
    if let Err(e) = cleanup_orphaned_clis() {
        warn!(error = %e, "Failed to cleanup orphaned processes");
    }

    let ipc_server = IpcServer::new().await?;
    info!(address = %ipc_server.address(), "IPC server configured");

    if let Err(e) = ipc_server.run().await {
        error!(error = %e, "IPC server exited with error");
        return Err(e);
    }

    info!("Agent Runner exiting cleanly");

    Ok(())
}
