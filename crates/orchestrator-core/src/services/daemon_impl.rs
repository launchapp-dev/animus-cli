use super::*;

async fn ensure_runner_started(project_root: &Path) -> Result<Option<u32>> {
    #[cfg(test)]
    if let Some(result) = take_test_ensure_result() {
        return result;
    }

    ensure_agent_runner_running(project_root).await
}

async fn stop_runner_for_retry(project_root: &Path) -> Result<bool> {
    #[cfg(test)]
    if let Some(result) = take_test_stop_result() {
        return result;
    }

    stop_agent_runner_process(project_root).await
}

async fn runner_ready_for_status(config_dir: &Path) -> bool {
    #[cfg(test)]
    if let Some(ready) = test_runner_ready_override() {
        return ready;
    }

    is_agent_runner_ready(config_dir).await
}

fn runner_pid_from_lock_for_status(config_dir: &Path) -> Option<u32> {
    #[cfg(test)]
    if let Some(pid) = test_runner_pid_override() {
        return pid;
    }

    read_runner_pid_from_lock(config_dir)
}

fn runner_process_alive_for_status(pid: u32) -> bool {
    #[cfg(test)]
    if let Some(alive) = test_runner_alive_override() {
        return alive;
    }

    is_runner_process_alive(pid)
}

async fn mutate_daemon_state<T>(
    hub: &FileServiceHub,
    mutator: impl FnOnce(&mut CoreState) -> Result<T>,
) -> Result<T> {
    #[cfg(test)]
    if test_skip_persist_override() {
        let mut lock = hub.state.write().await;
        return mutator(&mut lock);
    }

    let (output, _) = hub.mutate_persistent_state(mutator).await?;
    Ok(output)
}

#[async_trait]
impl DaemonServiceApi for InMemoryServiceHub {
    async fn start(&self, config: DaemonStartConfig) -> Result<()> {
        let pool_size = config.pool_size;
        let mut lock = self.state.write().await;
        lock.daemon_status = DaemonStatus::Running;
        lock.daemon_pool_size = pool_size;
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
        lock.logs.push(LogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            message: "daemon paused".to_string(),
        });
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
            healthy: matches!(
                lock.daemon_status,
                DaemonStatus::Running | DaemonStatus::Paused
            ),
            status: lock.daemon_status,
            runner_connected: lock.runner_pid.is_some(),
            runner_pid: lock.runner_pid,
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
}

#[async_trait]
impl DaemonServiceApi for FileServiceHub {
    async fn start(&self, config: DaemonStartConfig) -> Result<()> {
        let pool_size = config.pool_size;
        let runner_pid = match ensure_runner_started(&self.project_root).await {
            Ok(pid) => pid,
            Err(first_error) => {
                // Self-heal once: terminate any partial/stale runner process
                // and retry startup before surfacing an error.
                let _ = stop_runner_for_retry(&self.project_root).await;
                ensure_runner_started(&self.project_root)
                    .await
                    .with_context(|| format!("runner start retry failed after: {first_error}"))?
            }
        };

        mutate_daemon_state(self, |state| {
            state.daemon_status = DaemonStatus::Running;
            state.daemon_pool_size = pool_size;
            state.runner_pid = runner_pid;
            state.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: match (runner_pid, pool_size) {
                    (Some(pid), Some(ps)) => {
                        format!("daemon started (runner pid: {pid}, pool_size: {ps})")
                    }
                    (Some(pid), None) => format!("daemon started (runner pid: {pid})"),
                    (None, Some(ps)) => format!("daemon started (pool_size: {ps})"),
                    (None, None) => "daemon started".to_string(),
                },
            });
            Ok(())
        })
        .await
    }

    async fn stop(&self) -> Result<()> {
        let stopped_runner = stop_agent_runner_process(&self.project_root)
            .await
            .unwrap_or(false);
        mutate_daemon_state(self, |state| {
            state.daemon_status = DaemonStatus::Stopped;
            state.runner_pid = None;
            state.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: if stopped_runner {
                    "daemon stopped (runner terminated)".to_string()
                } else {
                    "daemon stopped".to_string()
                },
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
        let config_dir = runner_config_dir(&self.project_root);
        let runner_ready = runner_ready_for_status(&config_dir).await;
        let runner_pid_from_lock = runner_pid_from_lock_for_status(&config_dir);

        let (status, should_mark_crashed, runner_alive) = {
            let mut lock = self.state.write().await;
            if lock.runner_pid.is_none() && runner_ready {
                lock.runner_pid = runner_pid_from_lock;
            }
            let runner_pid = lock.runner_pid.or(runner_pid_from_lock);
            if lock.runner_pid.is_none() {
                lock.runner_pid = runner_pid;
            }
            let runner_alive = runner_pid
                .map(runner_process_alive_for_status)
                .unwrap_or(false);
            let should_mark_crashed = matches!(
                lock.daemon_status,
                DaemonStatus::Running | DaemonStatus::Paused
            ) && runner_pid.is_some()
                && !runner_ready
                && !runner_alive;
            (lock.daemon_status, should_mark_crashed, runner_alive)
        };

        if should_mark_crashed {
            return mutate_daemon_state(self, |state| {
                if state.runner_pid.is_none() {
                    state.runner_pid = runner_pid_from_lock;
                }
                if matches!(
                    state.daemon_status,
                    DaemonStatus::Running | DaemonStatus::Paused
                ) && state.runner_pid.is_some()
                    && !runner_ready
                    && !runner_alive
                {
                    state.daemon_status = DaemonStatus::Crashed;
                    state.logs.push(LogEntry {
                        timestamp: Utc::now(),
                        level: LogLevel::Error,
                        message: "agent-runner health check failed while daemon was active"
                            .to_string(),
                    });
                }
                Ok(state.daemon_status)
            })
            .await;
        }

        Ok(status)
    }

    async fn health(&self) -> Result<DaemonHealth> {
        let status = self.status().await?;
        let config_dir = runner_config_dir(&self.project_root);
        let runner_connected = is_agent_runner_ready(&config_dir).await;
        let active_agents = if runner_connected {
            query_runner_status(&config_dir)
                .await
                .map(|status| status.active_agents)
                .unwrap_or(0)
        } else {
            0
        };
        let lock = self.state.read().await;
        let pool_utilization_percent = lock.daemon_pool_size.map(|ps| {
            if ps == 0 {
                0.0
            } else {
                (active_agents as f64 / ps as f64) * 100.0
            }
        });
        let queued_tasks = lock
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Ready)
            .count() as u32;

        Ok(DaemonHealth {
            healthy: matches!(status, DaemonStatus::Running | DaemonStatus::Paused)
                && runner_connected,
            status,
            runner_connected,
            runner_pid: lock.runner_pid,
            active_agents,
            pool_size: lock.daemon_pool_size,
            project_root: Some(self.project_root.display().to_string()),
            daemon_pid: None,
            process_alive: None,
            pool_utilization_percent,
            queued_tasks: Some(queued_tasks),
            total_agents_spawned: None,
            total_agents_completed: None,
            total_agents_failed: None,
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
        mutate_daemon_state(self, |state| {
            state.logs.clear();
            Ok(())
        })
        .await
    }

    async fn active_agents(&self) -> Result<usize> {
        let config_dir = runner_config_dir(&self.project_root);
        if !is_agent_runner_ready(&config_dir).await {
            return Ok(0);
        }
        Ok(query_runner_status(&config_dir)
            .await
            .map(|status| status.active_agents)
            .unwrap_or(0))
    }
}

#[cfg(test)]
#[derive(Default)]
struct RunnerLifecycleTestHooks {
    ensure_results: std::collections::VecDeque<Result<Option<u32>>>,
    stop_results: std::collections::VecDeque<Result<bool>>,
    stop_calls: usize,
    runner_ready: Option<bool>,
    runner_pid_from_lock: Option<Option<u32>>,
    runner_alive: Option<bool>,
    skip_persist: bool,
}

#[cfg(test)]
fn runner_lifecycle_test_hooks() -> &'static std::sync::Mutex<RunnerLifecycleTestHooks> {
    static HOOKS: std::sync::OnceLock<std::sync::Mutex<RunnerLifecycleTestHooks>> =
        std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(RunnerLifecycleTestHooks::default()))
}

#[cfg(test)]
fn runner_lifecycle_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
fn with_runner_lifecycle_test_hooks<T>(f: impl FnOnce(&mut RunnerLifecycleTestHooks) -> T) -> T {
    let mut hooks = runner_lifecycle_test_hooks()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut hooks)
}

#[cfg(test)]
fn reset_runner_lifecycle_test_hooks() {
    with_runner_lifecycle_test_hooks(|hooks| *hooks = RunnerLifecycleTestHooks::default());
}

#[cfg(test)]
fn take_test_ensure_result() -> Option<Result<Option<u32>>> {
    with_runner_lifecycle_test_hooks(|hooks| hooks.ensure_results.pop_front())
}

#[cfg(test)]
fn take_test_stop_result() -> Option<Result<bool>> {
    with_runner_lifecycle_test_hooks(|hooks| {
        let result = hooks.stop_results.pop_front();
        if result.is_some() {
            hooks.stop_calls += 1;
        }
        result
    })
}

#[cfg(test)]
fn test_runner_ready_override() -> Option<bool> {
    with_runner_lifecycle_test_hooks(|hooks| hooks.runner_ready)
}

#[cfg(test)]
fn test_runner_pid_override() -> Option<Option<u32>> {
    with_runner_lifecycle_test_hooks(|hooks| hooks.runner_pid_from_lock)
}

#[cfg(test)]
fn test_runner_alive_override() -> Option<bool> {
    with_runner_lifecycle_test_hooks(|hooks| hooks.runner_alive)
}

#[cfg(test)]
fn test_skip_persist_override() -> bool {
    with_runner_lifecycle_test_hooks(|hooks| hooks.skip_persist)
}

#[cfg(test)]
struct RunnerLifecycleHooksGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl RunnerLifecycleHooksGuard {
    fn new() -> Self {
        let lock = runner_lifecycle_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_runner_lifecycle_test_hooks();
        Self { _lock: lock }
    }
}

#[cfg(test)]
impl Drop for RunnerLifecycleHooksGuard {
    fn drop(&mut self) {
        reset_runner_lifecycle_test_hooks();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use tempfile::TempDir;

    fn new_file_hub(temp: &TempDir) -> FileServiceHub {
        let state_file = temp.path().join(".ao").join("core-state.json");
        std::fs::create_dir_all(
            state_file
                .parent()
                .expect("state file should have a parent directory"),
        )
        .expect("state dir should exist");
        FileServiceHub {
            state: std::sync::Arc::new(tokio::sync::RwLock::new(CoreState::default_with_stopped())),
            state_file,
            project_root: temp.path().to_path_buf(),
        }
    }

    #[tokio::test]
    async fn file_hub_start_uses_initial_runner_start_result() {
        let _guard = RunnerLifecycleHooksGuard::new();
        with_runner_lifecycle_test_hooks(|hooks| {
            hooks.ensure_results = std::collections::VecDeque::from([Ok(Some(7001))]);
            hooks.stop_results = std::collections::VecDeque::from([Ok(true)]);
            hooks.skip_persist = true;
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        DaemonServiceApi::start(&hub, Default::default())
            .await
            .expect("daemon start should succeed");

        let state = hub.state.read().await;
        assert_eq!(state.daemon_status, DaemonStatus::Running);
        assert_eq!(state.runner_pid, Some(7001));
        drop(state);

        let stop_calls = with_runner_lifecycle_test_hooks(|hooks| hooks.stop_calls);
        assert_eq!(stop_calls, 0);
    }

    #[tokio::test]
    async fn file_hub_start_retries_runner_once_after_initial_failure() {
        let _guard = RunnerLifecycleHooksGuard::new();
        with_runner_lifecycle_test_hooks(|hooks| {
            hooks.ensure_results = std::collections::VecDeque::from([
                Err(anyhow!("first start failure")),
                Ok(Some(7002)),
            ]);
            hooks.stop_results = std::collections::VecDeque::from([Ok(true)]);
            hooks.skip_persist = true;
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        DaemonServiceApi::start(&hub, Default::default())
            .await
            .expect("daemon start should succeed after retry");

        let state = hub.state.read().await;
        assert_eq!(state.daemon_status, DaemonStatus::Running);
        assert_eq!(state.runner_pid, Some(7002));
        drop(state);

        let stop_calls = with_runner_lifecycle_test_hooks(|hooks| hooks.stop_calls);
        assert_eq!(stop_calls, 1);
    }

    #[tokio::test]
    async fn file_hub_start_retries_even_when_cleanup_stop_fails() {
        let _guard = RunnerLifecycleHooksGuard::new();
        with_runner_lifecycle_test_hooks(|hooks| {
            hooks.ensure_results = std::collections::VecDeque::from([
                Err(anyhow!("first start failure")),
                Ok(Some(7003)),
            ]);
            hooks.stop_results = std::collections::VecDeque::from([Err(anyhow!("stop failed"))]);
            hooks.skip_persist = true;
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        DaemonServiceApi::start(&hub, Default::default())
            .await
            .expect("daemon start should succeed on retry even if stop fails");

        let state = hub.state.read().await;
        assert_eq!(state.daemon_status, DaemonStatus::Running);
        assert_eq!(state.runner_pid, Some(7003));
        drop(state);

        let stop_calls = with_runner_lifecycle_test_hooks(|hooks| hooks.stop_calls);
        assert_eq!(stop_calls, 1);
    }

    #[tokio::test]
    async fn file_hub_start_retry_failure_includes_initial_error_context() {
        let _guard = RunnerLifecycleHooksGuard::new();
        with_runner_lifecycle_test_hooks(|hooks| {
            hooks.ensure_results = std::collections::VecDeque::from([
                Err(anyhow!("first start failure")),
                Err(anyhow!("second start failure")),
            ]);
            hooks.stop_results = std::collections::VecDeque::from([Ok(false)]);
            hooks.skip_persist = true;
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        let error = DaemonServiceApi::start(&hub, Default::default())
            .await
            .expect_err("daemon start should fail when retry fails");

        let display = format!("{error:#}");
        assert!(display.contains("runner start retry failed after: first start failure"));
        assert!(display.contains("second start failure"));
        let stop_calls = with_runner_lifecycle_test_hooks(|hooks| hooks.stop_calls);
        assert_eq!(stop_calls, 1);
    }

    #[tokio::test]
    async fn file_hub_status_keeps_running_when_runner_health_check_fails_but_pid_is_alive() {
        let _guard = RunnerLifecycleHooksGuard::new();
        with_runner_lifecycle_test_hooks(|hooks| {
            hooks.runner_ready = Some(false);
            hooks.runner_pid_from_lock = Some(Some(8123));
            hooks.runner_alive = Some(true);
            hooks.skip_persist = true;
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        {
            let mut lock = hub.state.write().await;
            lock.daemon_status = DaemonStatus::Running;
            lock.runner_pid = Some(8123);
        }

        let status = DaemonServiceApi::status(&hub).await.expect("status");
        assert_eq!(status, DaemonStatus::Running);
        let state = hub.state.read().await;
        assert_eq!(state.daemon_status, DaemonStatus::Running);
    }

    #[tokio::test]
    async fn file_hub_status_marks_daemon_crashed_when_runner_is_not_ready_and_not_alive() {
        let _guard = RunnerLifecycleHooksGuard::new();
        with_runner_lifecycle_test_hooks(|hooks| {
            hooks.runner_ready = Some(false);
            hooks.runner_pid_from_lock = Some(Some(8124));
            hooks.runner_alive = Some(false);
            hooks.skip_persist = true;
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        {
            let mut lock = hub.state.write().await;
            lock.daemon_status = DaemonStatus::Running;
            lock.runner_pid = Some(8124);
        }

        let status = DaemonServiceApi::status(&hub).await.expect("status");
        assert_eq!(status, DaemonStatus::Crashed);
        let state = hub.state.read().await;
        assert_eq!(state.daemon_status, DaemonStatus::Crashed);
        assert!(state.logs.iter().any(|entry| {
            entry.level == LogLevel::Error
                && entry.message == "agent-runner health check failed while daemon was active"
        }));
    }

    #[tokio::test]
    async fn file_hub_status_marks_paused_daemon_crashed_when_runner_is_not_ready_and_not_alive() {
        let _guard = RunnerLifecycleHooksGuard::new();
        with_runner_lifecycle_test_hooks(|hooks| {
            hooks.runner_ready = Some(false);
            hooks.runner_pid_from_lock = Some(Some(8125));
            hooks.runner_alive = Some(false);
            hooks.skip_persist = true;
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let hub = new_file_hub(&temp);
        {
            let mut lock = hub.state.write().await;
            lock.daemon_status = DaemonStatus::Paused;
            lock.runner_pid = Some(8125);
        }

        let status = DaemonServiceApi::status(&hub).await.expect("status");
        assert_eq!(status, DaemonStatus::Crashed);
        let state = hub.state.read().await;
        assert_eq!(state.daemon_status, DaemonStatus::Crashed);
        assert!(state.logs.iter().any(|entry| {
            entry.level == LogLevel::Error
                && entry.message == "agent-runner health check failed while daemon was active"
        }));
    }
}
