use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub struct DaemonRuntimeState;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DaemonRuntimeStateRecord {
    #[serde(default)]
    runtime_paused: bool,
    #[serde(default)]
    daemon_pid: Option<u32>,
    #[serde(default)]
    shutdown_requested: bool,
    #[serde(default)]
    shutdown_timeout_secs: Option<u64>,
}

impl DaemonRuntimeState {
    pub fn is_runtime_paused(project_root: &str) -> Result<bool> {
        Ok(load_daemon_runtime_state(project_root)?.runtime_paused)
    }

    pub fn get_daemon_pid(project_root: &str) -> Result<Option<u32>> {
        Ok(load_daemon_runtime_state(project_root)?.daemon_pid)
    }

    pub fn set_daemon_pid(project_root: &str, daemon_pid: Option<u32>) -> Result<()> {
        let mut state = load_daemon_runtime_state(project_root)?;
        state.daemon_pid = daemon_pid;
        save_daemon_runtime_state(project_root, &state)
    }

    pub fn set_runtime_paused(project_root: &str, paused: bool) -> Result<()> {
        let mut state = load_daemon_runtime_state(project_root)?;
        state.runtime_paused = paused;
        save_daemon_runtime_state(project_root, &state)
    }

    pub fn set_shutdown_requested(project_root: &str, requested: bool, timeout_secs: Option<u64>) -> Result<()> {
        let mut state = load_daemon_runtime_state(project_root)?;
        state.shutdown_requested = requested;
        state.shutdown_timeout_secs = if requested { timeout_secs } else { None };
        save_daemon_runtime_state(project_root, &state)
    }

    pub fn is_shutdown_requested(project_root: &str) -> Result<(bool, Option<u64>)> {
        let state = load_daemon_runtime_state(project_root)?;
        Ok((state.shutdown_requested, state.shutdown_timeout_secs))
    }

    pub fn write_daemon_pid_file(project_root: &str, pid: u32) {
        let path = daemon_pid_path(project_root);
        let _ = fs::write(path, pid.to_string());
    }

    pub fn remove_daemon_pid_file(project_root: &str) {
        let _ = fs::remove_file(daemon_pid_path(project_root));
    }

    pub fn read_daemon_pid_file(project_root: &str) -> Option<u32> {
        fs::read_to_string(daemon_pid_path(project_root)).ok().and_then(|value| value.trim().parse().ok())
    }
}

fn canonicalize_lossy(path: &str) -> String {
    let candidate = PathBuf::from(path);
    candidate.canonicalize().unwrap_or(candidate).to_string_lossy().to_string()
}

fn scoped_daemon_dir(project_root: &str) -> PathBuf {
    let canonical = PathBuf::from(canonicalize_lossy(project_root));
    protocol::scoped_state_root(&canonical)
        .map(|root| root.join("daemon"))
        .unwrap_or_else(|| canonical.join(".ao"))
}

fn daemon_runtime_state_path(project_root: &str) -> PathBuf {
    scoped_daemon_dir(project_root).join("daemon-state.json")
}

fn daemon_pid_path(project_root: &str) -> PathBuf {
    scoped_daemon_dir(project_root).join("daemon.pid")
}

fn load_daemon_runtime_state(project_root: &str) -> Result<DaemonRuntimeStateRecord> {
    let path = daemon_runtime_state_path(project_root);
    if !path.exists() {
        return Ok(DaemonRuntimeStateRecord::default());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read daemon runtime state at {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(DaemonRuntimeStateRecord::default());
    }

    serde_json::from_str(&content).with_context(|| format!("invalid daemon runtime state JSON at {}", path.display()))
}

fn save_daemon_runtime_state(project_root: &str, state: &DaemonRuntimeStateRecord) -> Result<()> {
    let path = daemon_runtime_state_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create daemon runtime state directory {}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(state).context("failed to serialize daemon runtime state JSON")?;
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, format!("{content}\n"))
        .with_context(|| format!("failed to write temp daemon state at {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &path)
        .with_context(|| format!("failed to persist daemon runtime state at {}", path.display()))?;
    Ok(())
}
