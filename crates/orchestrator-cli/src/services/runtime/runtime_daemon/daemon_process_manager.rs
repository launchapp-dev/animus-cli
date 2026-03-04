use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::process::Stdio;
use tokio::process::{Child, Command};

#[derive(Debug, Clone)]
pub struct WorkflowProcess {
    pub task_id: String,
    pub child: Arc<Mutex<Child>>,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct CompletedProcess {
    pub task_id: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub failure_reason: Option<String>,
}

#[derive(Default)]
pub struct ProcessManager {
    processes: Vec<WorkflowProcess>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Vec::new(),
        }
    }

    pub fn spawn_workflow_runner(
        &mut self,
        task_id: &str,
        pipeline_id: &str,
        project_root: &str,
    ) -> Result<WorkflowProcess> {
        let mut command = Command::new("ao-workflow-runner");
        command
            .arg("execute")
            .arg("--task-id")
            .arg(task_id)
            .arg("--pipeline")
            .arg(pipeline_id)
            .arg("--project-root")
            .arg(project_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = command.spawn().context("failed to spawn ao-workflow-runner")?;
        let process = WorkflowProcess {
            task_id: task_id.to_string(),
            child: Arc::new(Mutex::new(child)),
            started_at: Utc::now(),
        };

        self.processes.push(process.clone());

        Ok(process)
    }

    pub fn check_running(&mut self) -> Vec<CompletedProcess> {
        let now = Utc::now();
        let mut completed = Vec::new();
        let mut active = Vec::with_capacity(self.processes.len());

        for process in self.processes.drain(..) {
            let status = {
                let mut maybe_child = match process.child.lock() {
                    Ok(guard) => guard,
                    Err(error) => {
                        completed.push(CompletedProcess {
                            task_id: process.task_id,
                            started_at: process.started_at,
                            completed_at: now,
                            exit_code: None,
                            success: false,
                            failure_reason: Some(format!(
                                "failed to lock workflow process handle: {}",
                                error
                            )),
                        });
                        continue;
                    }
                };

                maybe_child.try_wait()
            };

            match status {
                Ok(Some(status)) => {
                    let exit_code = status.code();
                    let (success, failure_reason) = if status.success() {
                        (true, None)
                    } else {
                        (
                            false,
                            Some(format!(
                                "workflow runner exited unsuccessfully with status {:?}",
                                exit_code
                            )),
                        )
                    };

                    completed.push(CompletedProcess {
                        task_id: process.task_id,
                        started_at: process.started_at,
                        completed_at: now,
                        exit_code,
                        success,
                        failure_reason,
                    });
                }
                Ok(None) => {
                    active.push(process);
                }
                Err(error) => {
                    completed.push(CompletedProcess {
                        task_id: process.task_id,
                        started_at: process.started_at,
                        completed_at: now,
                        exit_code: None,
                        success: false,
                        failure_reason: Some(format!("failed to probe workflow process status: {}", error)),
                    });
                }
            }
        }

        self.processes = active;
        completed
    }

    pub fn active_count(&self) -> usize {
        self.processes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::test_utils::EnvVarGuard;
    use std::env;
    use std::fs;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn new_process_manager_starts_empty() {
        let manager = ProcessManager::new();
        assert_eq!(manager.active_count(), 0);
    }

    #[tokio::test]
    async fn spawn_workflow_runner_tracks_active_processes() {
        let _lock = crate::shared::test_env_lock().lock().expect("env lock should be available");

        let temp_dir = TempDir::new().expect("temp directory should be created");
        let runner_path = {
            #[cfg(unix)]
            let path = temp_dir.path().join("ao-workflow-runner");
            #[cfg(not(unix))]
            let path = temp_dir.path().join("ao-workflow-runner.exe");
            path
        };

        #[cfg(unix)]
        let runner_payload = "#!/bin/sh\nexit 0\n";
        #[cfg(not(unix))]
        let runner_payload = "@echo off\r\nexit /B 0\r\n";

        fs::write(&runner_path, runner_payload).expect("mock runner should be written");
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&runner_path)
                .expect("mock runner metadata should be available")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&runner_path, permissions)
                .expect("mock runner should be executable");
        }

        let original_path = env::var_os("PATH").unwrap_or_default();
        let mut paths = env::split_paths(&original_path).collect::<Vec<_>>();
        paths.insert(0, temp_dir.path().to_path_buf());
        let candidate_path = env::join_paths(paths).expect("path list should join");
        let candidate_path = candidate_path.to_string_lossy();
        let _path_guard = EnvVarGuard::set("PATH", Some(candidate_path.as_ref()));

        let mut manager = ProcessManager::new();
        let process =
            manager
                .spawn_workflow_runner("task-123", "standard", temp_dir.path().to_string_lossy().as_ref())
                .expect("mock runner should be discovered from PATH and spawned");
        assert_eq!(process.task_id, "task-123");
        assert_eq!(manager.active_count(), 1);
        let _ = manager.check_running();

        drop(process);
    }
}
