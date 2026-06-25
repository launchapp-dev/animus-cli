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
                let holder_pid = read_daemon_lock_pid(&lock_path)
                    .filter(|pid| *pid != current_pid && protocol::is_process_alive(*pid))
                    .or_else(|| {
                        DaemonRuntimeState::get_daemon_pid(&canonical_project_root)
                            .ok()
                            .flatten()
                            .filter(|pid| *pid != current_pid && protocol::is_process_alive(*pid))
                    });
                if let Some(holder_pid) = holder_pid {
                    anyhow::bail!(
                        "failed to acquire daemon lock for project {} (held by pid {})",
                        canonical_project_root,
                        holder_pid
                    );
                }
                anyhow::bail!("failed to acquire daemon lock for project {} (lock busy)", canonical_project_root);
            }
        }

        DaemonRuntimeState::set_daemon_pid(&canonical_project_root, Some(current_pid))?;
        DaemonRuntimeState::set_runtime_paused(&canonical_project_root, false)?;
        // Register the `daemon.pid` file alongside the lock so liveness probes
        // (`daemon status`/`daemon health` and the daemon's own control-wire
        // handlers, which read this file) succeed for a foreground `daemon run`
        // too — previously only the parent of a detached `daemon start` wrote
        // it, so a healthy containerized daemon reported `running: false` /
        // `unhealthy`. Tying the write to the lock winner here is race-free; the
        // Drop impl clears it.
        DaemonRuntimeState::write_daemon_pid_file(&canonical_project_root, current_pid);

        Ok(Self { project_root: canonical_project_root, pid: current_pid, _lock_file: lock_file })
    }
}

impl Drop for DaemonRunGuard {
    fn drop(&mut self) {
        if let Ok(Some(existing_pid)) = DaemonRuntimeState::get_daemon_pid(&self.project_root) {
            if existing_pid == self.pid {
                let _ = DaemonRuntimeState::set_daemon_pid(&self.project_root, None);
            }
        }
        // Clear the live PID file iff it still holds our PID — never remove a PID
        // a successor daemon wrote.
        if DaemonRuntimeState::read_daemon_pid_file(&self.project_root) == Some(self.pid) {
            DaemonRuntimeState::remove_daemon_pid_file(&self.project_root);
        }
    }
}

fn canonicalize_lossy(path: &str) -> String {
    let candidate = PathBuf::from(path);
    candidate.canonicalize().unwrap_or(candidate).to_string_lossy().to_string()
}

fn daemon_lock_path(project_root: &str) -> PathBuf {
    let canonical = PathBuf::from(canonicalize_lossy(project_root));
    protocol::scoped_state_root(&canonical)
        .map(|root| root.join("daemon").join("daemon.lock"))
        .expect("scoped_state_root requires a home directory")
}

fn read_daemon_lock_pid(lock_path: &PathBuf) -> Option<u32> {
    fs::read_to_string(lock_path).ok().and_then(|content| content.trim().parse::<u32>().ok())
}
