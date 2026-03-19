#[cfg(unix)]
use anyhow::bail;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
#[cfg(not(unix))]
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::runner::Runner;

use super::router;

static CONNECTION_COUNTER: AtomicU64 = AtomicU64::new(1);

const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 600;

pub struct IpcServer {
    endpoint: IpcEndpoint,
    runner: Arc<Mutex<Runner>>,
}

#[derive(Debug, Clone)]
enum IpcEndpoint {
    #[cfg(unix)]
    Unix(PathBuf),
    #[cfg(not(unix))]
    Tcp(String),
}

impl IpcServer {
    pub async fn new() -> Result<Self> {
        info!("Initializing IPC server");
        let (cleanup_tx, mut cleanup_rx) = tokio::sync::mpsc::channel(100);
        let runner = Arc::new(Mutex::new(Runner::new(cleanup_tx)));

        let runner_clone = Arc::clone(&runner);
        tokio::spawn(async move {
            while let Some(message) = cleanup_rx.recv().await {
                runner_clone.lock().await.cleanup_agent(message);
            }
        });

        #[cfg(unix)]
        {
            let socket_path = protocol::Config::global_config_dir().join("agent-runner.sock");
            prepare_socket_path(&socket_path)?;
            info!(
                socket_path = %socket_path.display(),
                "IPC endpoint configured for unix socket"
            );
            Ok(Self { endpoint: IpcEndpoint::Unix(socket_path), runner })
        }

        #[cfg(not(unix))]
        {
            info!("IPC endpoint configured for TCP loopback");
            Ok(Self { endpoint: IpcEndpoint::Tcp("127.0.0.1:9001".to_string()), runner })
        }
    }

    pub fn address(&self) -> String {
        match &self.endpoint {
            #[cfg(unix)]
            IpcEndpoint::Unix(path) => format!("unix://{}", path.display()),
            #[cfg(not(unix))]
            IpcEndpoint::Tcp(address) => format!("tcp://{}", address),
        }
    }

    pub async fn run(self) -> Result<()> {
        let idle_timeout = resolve_idle_timeout();
        let runner = Arc::clone(&self.runner);

        if idle_timeout.is_zero() {
            info!("Idle shutdown disabled (AO_RUNNER_IDLE_TIMEOUT_SECS=0)");
        } else {
            info!(idle_timeout_secs = idle_timeout.as_secs(), "Idle shutdown enabled");
        }

        match self.endpoint {
            #[cfg(unix)]
            IpcEndpoint::Unix(socket_path) => {
                let listener = UnixListener::bind(&socket_path)
                    .with_context(|| format!("Failed to bind IPC unix socket at {}", socket_path.display()))?;
                let _socket_guard = SocketCleanupGuard::new(socket_path.clone());

                info!(endpoint = %socket_path.display(), "IPC server listening on unix socket");

                let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .context("failed to register SIGTERM handler")?;

                let mut idle_since: Option<tokio::time::Instant> = Some(tokio::time::Instant::now());
                let mut idle_check =
                    tokio::time::interval_at(tokio::time::Instant::now() + IDLE_CHECK_INTERVAL, IDLE_CHECK_INTERVAL);

                loop {
                    tokio::select! {
                        accept_result = listener.accept() => {
                            match accept_result {
                                Ok((stream, _addr)) => {
                                    let connection_id = CONNECTION_COUNTER.fetch_add(1, Ordering::Relaxed);
                                    info!(connection_id, "Client connected via unix socket");
                                    let conn_runner = Arc::clone(&runner);
                                    tokio::spawn(async move {
                                        if let Err(e) =
                                            router::handle_connection(stream, conn_runner, connection_id).await
                                        {
                                            error!(connection_id, error = %e, "Connection error");
                                        }
                                        info!(connection_id, "Connection closed");
                                    });
                                }
                                Err(e) => error!(error = %e, "Failed to accept unix socket connection"),
                            }
                        }
                        _ = sigterm.recv() => {
                            info!("Received SIGTERM, initiating graceful shutdown");
                            break;
                        }
                        _ = idle_check.tick() => {
                            if check_idle(&runner, &mut idle_since, idle_timeout).await {
                                break;
                            }
                        }
                    }
                }
            }
            #[cfg(not(unix))]
            IpcEndpoint::Tcp(address) => {
                let listener = TcpListener::bind(&address).await.context("Failed to bind IPC server")?;

                info!("IPC server listening on tcp://{}", address);

                let mut idle_since: Option<tokio::time::Instant> = Some(tokio::time::Instant::now());
                let mut idle_check =
                    tokio::time::interval_at(tokio::time::Instant::now() + IDLE_CHECK_INTERVAL, IDLE_CHECK_INTERVAL);

                loop {
                    tokio::select! {
                        accept_result = listener.accept() => {
                            match accept_result {
                                Ok((stream, addr)) => {
                                    let connection_id = CONNECTION_COUNTER.fetch_add(1, Ordering::Relaxed);
                                    info!(connection_id, %addr, "Client connected over TCP");
                                    let conn_runner = Arc::clone(&runner);
                                    tokio::spawn(async move {
                                        if let Err(e) =
                                            router::handle_connection(stream, conn_runner, connection_id).await
                                        {
                                            error!(connection_id, error = %e, "Connection error");
                                        }
                                        info!(connection_id, "Connection closed");
                                    });
                                }
                                Err(e) => error!(error = %e, "Failed to accept TCP connection"),
                            }
                        }
                        _ = tokio::signal::ctrl_c() => {
                            info!("Received shutdown signal, initiating graceful shutdown");
                            break;
                        }
                        _ = idle_check.tick() => {
                            if check_idle(&runner, &mut idle_since, idle_timeout).await {
                                break;
                            }
                        }
                    }
                }
            }
        };

        graceful_shutdown(&runner).await;
        Ok(())
    }
}

#[cfg(unix)]
fn prepare_socket_path(socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create agent runner socket directory {}", parent.display()))?;
    }

    if socket_path.exists() {
        match std::os::unix::net::UnixStream::connect(socket_path) {
            Ok(_) => bail!("IPC socket already in use at {} (runner likely already running)", socket_path.display()),
            Err(_) => {
                warn!(
                    socket_path = %socket_path.display(),
                    "Found stale socket path; removing before bind"
                );
                std::fs::remove_file(socket_path)
                    .with_context(|| format!("Failed to remove stale agent runner socket {}", socket_path.display()))?;
            }
        }
    }

    Ok(())
}

#[cfg(unix)]
struct SocketCleanupGuard {
    path: PathBuf,
}

#[cfg(unix)]
impl SocketCleanupGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[cfg(unix)]
impl Drop for SocketCleanupGuard {
    fn drop(&mut self) {
        if self.path.exists() {
            if let Err(e) = std::fs::remove_file(&self.path) {
                warn!("Failed to remove agent runner socket on shutdown {}: {}", self.path.display(), e);
            }
        }
    }
}

fn resolve_idle_timeout() -> Duration {
    let secs = std::env::var("AO_RUNNER_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

async fn check_idle(
    runner: &Arc<Mutex<Runner>>,
    idle_since: &mut Option<tokio::time::Instant>,
    idle_timeout: Duration,
) -> bool {
    if idle_timeout.is_zero() {
        return false;
    }
    let agents = runner.lock().await.active_agent_count();
    if agents == 0 {
        let since = idle_since.get_or_insert_with(tokio::time::Instant::now);
        if since.elapsed() >= idle_timeout {
            info!(idle_secs = since.elapsed().as_secs(), "Idle timeout reached, shutting down");
            return true;
        }
    } else {
        *idle_since = None;
    }
    false
}

async fn graceful_shutdown(runner: &Arc<Mutex<Runner>>) {
    info!("Running graceful shutdown");
    let run_ids = runner.lock().await.active_run_ids();
    if !run_ids.is_empty() {
        info!(count = run_ids.len(), "Cancelling active agent runs");
        runner.lock().await.cancel_runs(&run_ids);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if let Err(e) = crate::cleanup::kill_all_tracked_processes() {
        warn!(error = %e, "Failed to cleanup tracked processes during shutdown");
    }
    info!("Graceful shutdown complete");
}
