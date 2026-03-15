use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use fs2::FileExt;

use crate::DaemonRuntimeState;

pub struct DaemonRunGuard {
    project_root: String,
    pid: u32,
    _lock_file: File,
}

impl DaemonRunGuard {
    pub fn acquire(project_root: &str) -> Result<Self> {
        let canonical_project_root = canonicalize_lossy(project_root);
        let current_pid = std::process::id();
        if let Some(existing_pid) = DaemonRuntimeState::get_daemon_pid(&canonical_project_root)? {
            if existing_pid != current_pid && protocol::is_process_alive(existing_pid) {
                anyhow::bail!("daemon already running for project {} (pid {})", canonical_project_root, existing_pid);
            }
            if existing_pid != current_pid {
                let _ = DaemonRuntimeState::set_daemon_pid(&canonical_project_root, None);
            }
        }

        let lock_path = daemon_lock_path(&canonical_project_root);
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock_file = OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path)?;

        match lock_file.try_lock_exclusive() {
            Ok(_) => {
                lock_file.set_len(0)?;
                write!(&lock_file, "{current_pid}")?;
                lock_file.sync_all()?;
            }
            Err(_) => {
                if let Some(lock_pid) = read_daemon_lock_pid(&lock_path) {
                    if lock_pid != current_pid && protocol::is_process_alive(lock_pid) {
                        anyhow::bail!(
                            "failed to acquire daemon lock for project {} (held by pid {})",
                            canonical_project_root,
                            lock_pid
                        );
                    }
                }
                anyhow::bail!("failed to acquire daemon lock for project {} (lock busy)", canonical_project_root);
            }
        }

        DaemonRuntimeState::set_daemon_pid(&canonical_project_root, Some(current_pid))?;
        DaemonRuntimeState::set_runtime_paused(&canonical_project_root, false)?;

        Ok(Self { project_root: canonical_project_root, pid: current_pid, _lock_file: lock_file })
    }
}

impl Drop for DaemonRunGuard {
    fn drop(&mut self) {
        let _ = DaemonRuntimeState::set_runtime_paused(&self.project_root, true);
        if let Ok(Some(existing_pid)) = DaemonRuntimeState::get_daemon_pid(&self.project_root) {
            if existing_pid == self.pid {
                let _ = DaemonRuntimeState::set_daemon_pid(&self.project_root, None);
            }
        }
    }
}

fn canonicalize_lossy(path: &str) -> String {
    let candidate = PathBuf::from(path);
    candidate.canonicalize().unwrap_or(candidate).to_string_lossy().to_string()
}

fn daemon_lock_path(project_root: &str) -> PathBuf {
    let canonical = PathBuf::from(canonicalize_lossy(project_root));
    let base = protocol::scoped_state_root(&canonical)
        .map(|root| root.join("daemon"))
        .unwrap_or_else(|| canonical.join(".ao"));
    base.join("daemon.lock")
}

fn read_daemon_lock_pid(lock_path: &PathBuf) -> Option<u32> {
    fs::read_to_string(lock_path).ok().and_then(|content| content.trim().parse::<u32>().ok())
}
