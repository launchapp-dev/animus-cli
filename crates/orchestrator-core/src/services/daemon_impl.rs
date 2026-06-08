use super::*;

fn daemon_pid_for_status(project_root: &Path) -> Option<u32> {
    #[cfg(test)]
    if let Some(pid) = test_daemon_pid_override() {
        return pid;
    }

    let pm_config_path = crate::daemon_project_config_path(project_root);
    let daemon_dir = pm_config_path.parent()?;
    std::fs::read_to_string(daemon_dir.join("daemon.pid")).ok()?.trim().parse::<u32>().ok()
}

fn daemon_process_alive_for_status(pid: u32) -> bool {
    #[cfg(test)]
    if let Some(alive) = test_daemon_process_alive_override() {
        return alive;
    }

    protocol::is_process_alive(pid)
}

async fn mutate_daemon_state<T>(hub: &FileServiceHub, mutator: impl FnOnce(&mut CoreState) -> Result<T>) -> Result<T> {
    #[cfg(test)]
    if test_skip_persist_override() {
        let mut lock = hub.state.write().await;
        return mutator(&mut lock);
    }

    let (output, _) = hub.mutate_persistent_state(mutator).await?;
    Ok(output)
}

/// Fast subset of [`DaemonHealth`] returned by
/// [`load_daemon_status_snapshot_fast`]. Skips the SQLite-backed queued-task
/// count and the provider-plugin executability scan that
/// [`load_daemon_health_snapshot`] does, so callers that only need
/// "is the daemon running?" pay one stat + one pid read + one liveness
/// probe instead of a multi-hundred-millisecond round trip.
#[derive(Debug, Clone)]
pub struct DaemonStatusSnapshot {
    pub status: DaemonStatus,
    pub daemon_pid: Option<u32>,
    pub process_alive: Option<bool>,
}

pub async fn load_daemon_status_snapshot_fast(project_root: &Path) -> Result<DaemonStatusSnapshot> {
    let state_file = protocol::scoped_state_root(project_root)
        .unwrap_or_else(|| project_root.join(".animus"))
        .join("core-state.json");
    let snapshot = state_store::load_daemon_state_snapshot(&state_file);

    let daemon_pid = daemon_pid_for_status(project_root);
    let process_alive = daemon_pid.map(daemon_process_alive_for_status);

    let mut status = snapshot.daemon_status;
    if matches!(status, DaemonStatus::Running | DaemonStatus::Paused)
        && daemon_pid.is_some()
        && process_alive == Some(false)
    {
        status = DaemonStatus::Crashed;
    }
    if matches!(status, DaemonStatus::Stopped) && process_alive == Some(true) {
        status = DaemonStatus::Running;
    }

    Ok(DaemonStatusSnapshot { status, daemon_pid, process_alive })
}

pub async fn load_daemon_health_snapshot(project_root: &Path) -> Result<DaemonHealth> {
    let state_file = protocol::scoped_state_root(project_root)
        .unwrap_or_else(|| project_root.join(".animus"))
        .join("core-state.json");
    let snapshot = state_store::load_daemon_state_snapshot(&state_file);

    let daemon_pid = daemon_pid_for_status(project_root);
    let process_alive = daemon_pid.map(daemon_process_alive_for_status);

    let mut status = snapshot.daemon_status;
    if matches!(status, DaemonStatus::Running | DaemonStatus::Paused)
        && daemon_pid.is_some()
        && process_alive == Some(false)
    {
        status = DaemonStatus::Crashed;
    }
    if matches!(status, DaemonStatus::Stopped) && process_alive == Some(true) {
        status = DaemonStatus::Running;
    }

    let active_agents = snapshot.active_process_count.unwrap_or(0);

    let pool_size = snapshot
        .daemon_pool_size
        .or_else(|| crate::load_daemon_project_config(project_root).ok().and_then(|config| config.pool_size));

    let pool_utilization_percent =
        pool_size.map(|pool_size| if pool_size == 0 { 0.0 } else { (active_agents as f64 / pool_size as f64) * 100.0 });
    let queued_tasks = crate::workflow::count_tasks_with_status(project_root, TaskStatus::Ready)
        .map(|count| count as u32)
        .unwrap_or(0);

    let provider_plugins_healthy = provider_plugins_healthy_for(project_root);

    Ok(DaemonHealth {
        healthy: matches!(status, DaemonStatus::Running | DaemonStatus::Paused),
        status,
        runner_connected: false,
        runner_pid: None,
        provider_plugins_healthy,
        active_agents,
        pool_size,
        project_root: Some(project_root.display().to_string()),
        daemon_pid,
        process_alive,
        pool_utilization_percent,
        queued_tasks: Some(queued_tasks),
        total_agents_spawned: None,
        total_agents_completed: None,
        total_agents_failed: None,
        flavor: active_flavor_for_project(project_root),
    })
}

/// v0.5: probe the working tree for an `animus.flavor.v1` manifest and
/// return its `id`. v0.5 always emits `"default"` when the canonical
/// manifest at `flavors/default.toml` is found; absent installs return
/// `None` so consumers can distinguish "not configured" from "default".
fn active_flavor_for_project(project_root: &Path) -> Option<String> {
    crate::flavor::load_flavor_in(project_root, crate::flavor::DEFAULT_FLAVOR_ID).ok().flatten().map(|m| m.id)
}

/// v0.5.3: discover installed provider plugins and report whether at
/// least one binary is present **and executable** on disk. The lookup
/// is intentionally cheap (no spawn / no probe) so health calls stay
/// sub-second even when many providers are configured. A path that
/// exists but has lost its execute bit fails to spawn at run time, so
/// surfacing it as healthy here would produce a false-green; match the
/// `animus runner health` executability check exactly.
fn provider_plugins_healthy_for(project_root: &Path) -> bool {
    use orchestrator_plugin_host::session::discover_provider_plugins;
    discover_provider_plugins(project_root).iter().any(|p| is_binary_executable(&p.binary_path))
}

#[cfg(unix)]
fn is_binary_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_binary_executable(path: &Path) -> bool {
    path.exists()
}

#[async_trait]
impl DaemonServiceApi for InMemoryServiceHub {
    async fn start(&self, config: DaemonStartConfig) -> Result<()> {
        let pool_size = config.pool_size;
        let mut lock = self.state.write().await;
        lock.daemon_status = DaemonStatus::Running;
        if let Some(ps) = pool_size {
            lock.daemon_pool_size = Some(ps);
        }
        lock.logs.push(LogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            message: match pool_size {
                Some(ps) => format!("daemon started (pool_size: {ps})"),
                None => "daemon started".to_string(),
            },
        });
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let mut lock = self.state.write().await;
        lock.daemon_status = DaemonStatus::Stopped;
        lock.runner_pid = None;
        lock.logs.push(LogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            message: "daemon stopped".to_string(),
        });
        Ok(())
    }

    async fn pause(&self) -> Result<()> {
        let mut lock = self.state.write().await;
        lock.daemon_status = DaemonStatus::Paused;
        lock.logs.push(LogEntry { timestamp: Utc::now(), level: LogLevel::Info, message: "daemon paused".to_string() });
        Ok(())
    }

    async fn resume(&self) -> Result<()> {
        let mut lock = self.state.write().await;
        lock.daemon_status = DaemonStatus::Running;
        lock.logs.push(LogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            message: "daemon resumed".to_string(),
        });
        Ok(())
    }

    async fn status(&self) -> Result<DaemonStatus> {
        Ok(self.state.read().await.daemon_status)
    }

    async fn health(&self) -> Result<DaemonHealth> {
        let lock = self.state.read().await;
        Ok(DaemonHealth {
            healthy: matches!(lock.daemon_status, DaemonStatus::Running | DaemonStatus::Paused),
            status: lock.daemon_status,
            runner_connected: false,
            runner_pid: None,
            provider_plugins_healthy: false,
            active_agents: 0,
            pool_size: lock.daemon_pool_size,
            project_root: None,
            daemon_pid: None,
            process_alive: None,
            pool_utilization_percent: lock.daemon_pool_size.map(|_| 0.0),
            queued_tasks: Some(0),
            total_agents_spawned: None,
            total_agents_completed: None,
            total_agents_failed: None,
            flavor: crate::flavor::load_flavor(crate::flavor::DEFAULT_FLAVOR_ID).ok().flatten().map(|m| m.id),
        })
    }

    async fn logs(&self, limit: Option<usize>) -> Result<Vec<LogEntry>> {
        let lock = self.state.read().await;
        let mut logs = lock.logs.clone();
        if let Some(limit) = limit {
            if logs.len() > limit {
                logs = logs.split_off(logs.len() - limit);
            }
        }
        Ok(logs)
    }

    async fn clear_logs(&self) -> Result<()> {
        self.state.write().await.logs.clear();
        Ok(())
    }

    async fn active_agents(&self) -> Result<usize> {
        Ok(0)
    }

    async fn set_active_process_count(&self, count: usize) -> Result<()> {
        self.state.write().await.active_process_count = Some(count);
        Ok(())
    }
}

#[async_trait]
impl DaemonServiceApi for FileServiceHub {
    async fn start(&self, config: DaemonStartConfig) -> Result<()> {
        let pool_size = config.pool_size;

        mutate_daemon_state(self, |state| {
            state.daemon_status = DaemonStatus::Running;
            if let Some(ps) = pool_size {
                state.daemon_pool_size = Some(ps);
            }
            // v0.5.3: agent-runner sidecar was deleted; runner_pid is
            // kept on the state struct for back-compat reads only.
            state.runner_pid = None;
            state.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: match pool_size {
                    Some(ps) => format!("daemon started (pool_size: {ps})"),
                    None => "daemon started".to_string(),
                },
            });
            Ok(())
        })
        .await
    }

    async fn stop(&self) -> Result<()> {
        mutate_daemon_state(self, |state| {
            state.daemon_status = DaemonStatus::Stopped;
            state.runner_pid = None;
            state.active_process_count = None;
            state.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: "daemon stopped".to_string(),
            });
            Ok(())
        })
        .await
    }

    async fn pause(&self) -> Result<()> {
        mutate_daemon_state(self, |state| {
            state.daemon_status = DaemonStatus::Paused;
            state.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: "daemon paused".to_string(),
            });
            Ok(())
        })
        .await
    }

    async fn resume(&self) -> Result<()> {
        mutate_daemon_state(self, |state| {
            state.daemon_status = DaemonStatus::Running;
            state.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: "daemon resumed".to_string(),
            });
            Ok(())
        })
        .await
    }

    async fn status(&self) -> Result<DaemonStatus> {
        let daemon_pid = daemon_pid_for_status(&self.project_root);
        let daemon_process_alive = daemon_pid.map(daemon_process_alive_for_status);

        let (status, should_mark_crashed) = {
            let lock = self.state.read().await;
            let should_mark_crashed = matches!(lock.daemon_status, DaemonStatus::Running | DaemonStatus::Paused)
                && daemon_pid.is_some()
                && daemon_process_alive == Some(false);
            (lock.daemon_status, should_mark_crashed)
        };

        if should_mark_crashed {
            return mutate_daemon_state(self, |state| {
                if matches!(state.daemon_status, DaemonStatus::Running | DaemonStatus::Paused)
                    && daemon_pid.is_some()
                    && daemon_process_alive == Some(false)
                {
                    state.daemon_status = DaemonStatus::Crashed;
                    state.logs.push(LogEntry {
                        timestamp: Utc::now(),
                        level: LogLevel::Error,
                        message: "daemon pid liveness check failed while daemon was active".to_string(),
                    });
                }
                Ok(state.daemon_status)
            })
            .await;
        }

        Ok(status)
    }

    async fn health(&self) -> Result<DaemonHealth> {
        let daemon_pid = daemon_pid_for_status(&self.project_root);
        let process_alive = daemon_pid.map(daemon_process_alive_for_status);
        let (mut status, should_mark_crashed) = {
            let lock = self.state.read().await;
            let should_mark_crashed = matches!(lock.daemon_status, DaemonStatus::Running | DaemonStatus::Paused)
                && daemon_pid.is_some()
                && process_alive == Some(false);
            (lock.daemon_status, should_mark_crashed)
        };

        if should_mark_crashed {
            status = mutate_daemon_state(self, |state| {
                if matches!(state.daemon_status, DaemonStatus::Running | DaemonStatus::Paused)
                    && daemon_pid.is_some()
                    && process_alive == Some(false)
                {
                    state.daemon_status = DaemonStatus::Crashed;
                    state.logs.push(LogEntry {
                        timestamp: Utc::now(),
                        level: LogLevel::Error,
                        message: "daemon pid liveness check failed while daemon was active".to_string(),
                    });
                }
                Ok(state.daemon_status)
            })
            .await?;
        }

        let persisted_process_count = self.state.read().await.active_process_count;
        let active_agents = persisted_process_count.unwrap_or(0);

        let lock = self.state.read().await;

        let pool_size = lock
            .daemon_pool_size
            .or_else(|| crate::load_daemon_project_config(&self.project_root).ok().and_then(|c| c.pool_size));

        if matches!(status, DaemonStatus::Stopped) && process_alive == Some(true) {
            status = DaemonStatus::Running;
        }

        let pool_utilization_percent =
            pool_size.map(|ps| if ps == 0 { 0.0 } else { (active_agents as f64 / ps as f64) * 100.0 });
        let queued_tasks = crate::workflow::count_tasks_with_status(&self.project_root, TaskStatus::Ready)
            .map(|count| count as u32)
            .unwrap_or(0);
        let provider_plugins_healthy = provider_plugins_healthy_for(&self.project_root);

        Ok(DaemonHealth {
            healthy: matches!(status, DaemonStatus::Running | DaemonStatus::Paused),
            status,
            runner_connected: false,
            runner_pid: None,
            provider_plugins_healthy,
            active_agents,
            pool_size,
            project_root: Some(self.project_root.display().to_string()),
            daemon_pid,
            process_alive,
            pool_utilization_percent,
            queued_tasks: Some(queued_tasks),
            total_agents_spawned: None,
            total_agents_completed: None,
            total_agents_failed: None,
            flavor: active_flavor_for_project(&self.project_root),
        })
    }

    async fn logs(&self, limit: Option<usize>) -> Result<Vec<LogEntry>> {
        load_logs(&self.logs_file, limit)
    }

    async fn clear_logs(&self) -> Result<()> {
        mutate_daemon_state(self, |state| {
            state.logs.clear();
            Ok(())
        })
        .await?;
        clear_logs_file(&self.logs_file)
    }

    async fn active_agents(&self) -> Result<usize> {
        let lock = self.state.read().await;
        Ok(lock.active_process_count.unwrap_or(0))
    }

    async fn set_active_process_count(&self, count: usize) -> Result<()> {
        mutate_daemon_state(self, |state| {
            state.active_process_count = Some(count);
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
fn test_daemon_pid_override() -> Option<Option<u32>> {
    None
}

#[cfg(test)]
fn test_daemon_process_alive_override() -> Option<bool> {
    None
}

#[cfg(test)]
fn test_skip_persist_override() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn new_file_hub(temp: &TempDir) -> FileServiceHub {
        let state_file = temp.path().join(".animus").join("core-state.json");
        let logs_file = state_store::logs_file_for_state_file(&state_file);
        std::fs::create_dir_all(state_file.parent().expect("state file should have a parent directory"))
            .expect("state dir should exist");
        FileServiceHub {
            state: std::sync::Arc::new(tokio::sync::RwLock::new(CoreState::default_with_stopped())),
            state_file,
            logs_file,
            project_root: temp.path().to_path_buf(),
        }
    }

    #[tokio::test]
    async fn file_hub_start_sets_running() {
        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        DaemonServiceApi::start(&hub, Default::default()).await.expect("daemon start should succeed");

        let state = hub.state.read().await;
        assert_eq!(state.daemon_status, DaemonStatus::Running);
        assert_eq!(state.runner_pid, None, "v0.5.3: runner_pid is always None after sidecar removal");
    }

    #[tokio::test]
    async fn fast_snapshot_returns_stopped_for_empty_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let snapshot = load_daemon_status_snapshot_fast(temp.path())
            .await
            .expect("fast snapshot should succeed for empty project");
        assert_eq!(snapshot.status, DaemonStatus::Stopped);
        assert!(snapshot.daemon_pid.is_none());
        assert!(snapshot.process_alive.is_none());
    }

    #[tokio::test]
    async fn fast_snapshot_matches_full_snapshot_status_for_empty_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let fast = load_daemon_status_snapshot_fast(temp.path()).await.expect("fast snapshot");
        let full = load_daemon_health_snapshot(temp.path()).await.expect("full snapshot");
        assert_eq!(fast.status, full.status, "fast snapshot must report the same DaemonStatus as the full snapshot");
        assert_eq!(fast.daemon_pid, full.daemon_pid);
        assert_eq!(fast.process_alive, full.process_alive);
    }

    #[tokio::test]
    async fn fast_snapshot_does_not_open_sqlite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scoped = protocol::scoped_state_root(temp.path()).unwrap_or_else(|| temp.path().join(".animus"));
        let _ = load_daemon_status_snapshot_fast(temp.path()).await.expect("fast snapshot");
        // The full snapshot opens the per-project SQLite store
        // (count_tasks_with_status). The fast snapshot must not — if it
        // did, the scoped store directory would carry the typical
        // `tasks.db`, `tasks.db-wal`, or `tasks.db-shm` files. Assert
        // none materialized.
        let store_dir = scoped.join("store");
        if store_dir.exists() {
            for entry in std::fs::read_dir(&store_dir).expect("read store dir") {
                let entry = entry.expect("dir entry");
                let name = entry.file_name();
                let name = name.to_string_lossy();
                assert!(
                    !name.ends_with(".db") && !name.ends_with(".db-wal") && !name.ends_with(".db-shm"),
                    "fast snapshot must not open SQLite; found {name}"
                );
            }
        }
    }

    #[tokio::test]
    async fn file_hub_start_skips_runner_when_requested_noop() {
        // v0.5.3: `skip_runner` is a no-op since there is no runner sidecar.
        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        DaemonServiceApi::start(&hub, DaemonStartConfig { skip_runner: true, ..Default::default() })
            .await
            .expect("daemon start should succeed without starting runner");

        let state = hub.state.read().await;
        assert_eq!(state.daemon_status, DaemonStatus::Running);
        assert_eq!(state.runner_pid, None);
    }
}
