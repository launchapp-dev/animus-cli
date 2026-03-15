use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::config;

fn get_runner_lock_path() -> PathBuf {
    let app_dir = config::app_config_dir();
    std::fs::create_dir_all(&app_dir).ok();
    app_dir.join("agent-runner.lock")
}

pub fn acquire_runner_lock() -> Result<File> {
    let lock_path = get_runner_lock_path();
    info!(lock_path = %lock_path.display(), "Attempting to acquire runner lock");

    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .context("Failed to open lock file")?;

    match lock_file.try_lock_exclusive() {
        Ok(_) => {
            lock_file.set_len(0)?;
            let pid = std::process::id();
            let ipc_address = get_ipc_address();
            write!(&lock_file, "{}|{}", pid, ipc_address)?;
            lock_file.sync_all()?;
            info!(pid, ipc_address = %ipc_address, "Runner lock acquired");
            Ok(lock_file)
        }
        Err(_) => {
            let (existing_pid, ipc_addr) = read_runner_lock(&lock_path)?;
            if process_exists(existing_pid) {
                warn!(
                    existing_pid,
                    ipc_address = %ipc_addr,
                    "Runner lock is held by an active process"
                );
                bail!("Another agent runner is already running (PID: {}, IPC: {}).", existing_pid, ipc_addr);
            }
            warn!(
                existing_pid,
                ipc_address = %ipc_addr,
                "Found stale runner lock; removing and retrying"
            );
            drop(lock_file);
            fs::remove_file(&lock_path)?;
            info!(lock_path = %lock_path.display(), "Removed stale runner lock");
            acquire_runner_lock()
        }
    }
}

fn read_runner_lock(lock_path: &Path) -> Result<(i32, String)> {
    let content = fs::read_to_string(lock_path)?;
    let parts: Vec<&str> = content.split('|').collect();
    let pid = parts.first().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
    let ipc_addr = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
    Ok((pid, ipc_addr))
}

fn get_ipc_address() -> String {
    #[cfg(unix)]
    {
        config::app_config_dir().join("agent-runner.sock").display().to_string()
    }

    #[cfg(not(unix))]
    {
        "127.0.0.1:9001".to_string()
    }
}

use protocol::process_exists;
