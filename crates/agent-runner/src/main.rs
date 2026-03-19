use anyhow::Result;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,agent_runner=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let pid = std::process::id();
    let config_dir = agent_runner::config::app_config_dir();
    info!(
        pid,
        config_dir = %config_dir.display(),
        "Agent Runner starting"
    );

    let _lock_file = match agent_runner::lock::acquire_runner_lock() {
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
    if let Err(e) = agent_runner::cleanup::cleanup_orphaned_clis() {
        warn!(error = %e, "Failed to cleanup orphaned processes");
    }

    tokio::spawn(async {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(e) = agent_runner::cleanup::sweep_stale_tracker_entries() {
                warn!(error = %e, "Failed to sweep stale tracker entries");
            }
        }
    });

    let ipc_server = agent_runner::ipc::IpcServer::new().await?;
    info!(address = %ipc_server.address(), "IPC server configured");

    if let Err(e) = ipc_server.run().await {
        error!(error = %e, "IPC server exited with error");
        return Err(e);
    }

    info!("Agent Runner exiting cleanly");

    Ok(())
}
