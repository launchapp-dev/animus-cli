use super::*;

#[async_trait]
impl DaemonServiceApi for InMemoryServiceHub {
    async fn start(&self) -> Result<()> {
        let max_agents = max_agents_override_from_env();
        let mut lock = self.state.write().await;
        lock.daemon_status = DaemonStatus::Running;
        lock.daemon_max_agents = max_agents;
        lock.logs.push(LogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            message: match max_agents {
                Some(max) => format!("daemon started (max_agents: {max})"),
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
            max_agents: lock.daemon_max_agents,
            project_root: None,
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
    async fn start(&self) -> Result<()> {
        let max_agents = max_agents_override_from_env();
        let runner_pid = match ensure_agent_runner_running(&self.project_root).await {
            Ok(pid) => pid,
            Err(first_error) => {
                // Self-heal once: terminate any partial/stale runner process
                // and retry startup before surfacing an error.
                let _ = stop_agent_runner_process(&self.project_root).await;
                ensure_agent_runner_running(&self.project_root)
                    .await
                    .with_context(|| format!("runner start retry failed after: {first_error}"))?
            }
        };

        let snapshot = {
            let mut lock = self.state.write().await;
            lock.daemon_status = DaemonStatus::Running;
            lock.daemon_max_agents = max_agents;
            lock.runner_pid = runner_pid;
            lock.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: match (runner_pid, max_agents) {
                    (Some(pid), Some(max)) => {
                        format!("daemon started (runner pid: {pid}, max_agents: {max})")
                    }
                    (Some(pid), None) => format!("daemon started (runner pid: {pid})"),
                    (None, Some(max)) => format!("daemon started (max_agents: {max})"),
                    (None, None) => "daemon started".to_string(),
                },
            });
            lock.clone()
        };
        Self::persist_snapshot(&self.state_file, &snapshot)
    }

    async fn stop(&self) -> Result<()> {
        let stopped_runner = stop_agent_runner_process(&self.project_root)
            .await
            .unwrap_or(false);
        let snapshot = {
            let mut lock = self.state.write().await;
            lock.daemon_status = DaemonStatus::Stopped;
            lock.runner_pid = None;
            lock.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: if stopped_runner {
                    "daemon stopped (runner terminated)".to_string()
                } else {
                    "daemon stopped".to_string()
                },
            });
            lock.clone()
        };
        Self::persist_snapshot(&self.state_file, &snapshot)
    }

    async fn pause(&self) -> Result<()> {
        let snapshot = {
            let mut lock = self.state.write().await;
            lock.daemon_status = DaemonStatus::Paused;
            lock.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: "daemon paused".to_string(),
            });
            lock.clone()
        };
        Self::persist_snapshot(&self.state_file, &snapshot)
    }

    async fn resume(&self) -> Result<()> {
        let snapshot = {
            let mut lock = self.state.write().await;
            lock.daemon_status = DaemonStatus::Running;
            lock.logs.push(LogEntry {
                timestamp: Utc::now(),
                level: LogLevel::Info,
                message: "daemon resumed".to_string(),
            });
            lock.clone()
        };
        Self::persist_snapshot(&self.state_file, &snapshot)
    }

    async fn status(&self) -> Result<DaemonStatus> {
        let config_dir = runner_config_dir(&self.project_root);
        let runner_ready = is_agent_runner_ready(&config_dir).await;

        let snapshot = {
            let mut lock = self.state.write().await;
            if lock.runner_pid.is_none() && runner_ready {
                lock.runner_pid = read_runner_pid_from_lock(&config_dir);
            }
            let runner_pid = lock
                .runner_pid
                .or_else(|| read_runner_pid_from_lock(&config_dir));
            if lock.runner_pid.is_none() {
                lock.runner_pid = runner_pid;
            }
            let runner_alive = runner_pid.map(is_runner_process_alive).unwrap_or(false);
            if matches!(
                lock.daemon_status,
                DaemonStatus::Running | DaemonStatus::Paused
            ) && lock.runner_pid.is_some()
                && !runner_ready
                && !runner_alive
            {
                lock.daemon_status = DaemonStatus::Crashed;
                lock.logs.push(LogEntry {
                    timestamp: Utc::now(),
                    level: LogLevel::Error,
                    message: "agent-runner health check failed while daemon was active".to_string(),
                });
                Some(lock.clone())
            } else {
                None
            }
        };

        if let Some(snapshot) = snapshot {
            Self::persist_snapshot(&self.state_file, &snapshot)?;
        }

        Ok(self.state.read().await.daemon_status)
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

        Ok(DaemonHealth {
            healthy: matches!(status, DaemonStatus::Running | DaemonStatus::Paused)
                && runner_connected,
            status,
            runner_connected,
            runner_pid: lock.runner_pid,
            active_agents,
            max_agents: lock.daemon_max_agents,
            project_root: Some(self.project_root.display().to_string()),
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
        let snapshot = {
            let mut lock = self.state.write().await;
            lock.logs.clear();
            lock.clone()
        };
        Self::persist_snapshot(&self.state_file, &snapshot)
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
