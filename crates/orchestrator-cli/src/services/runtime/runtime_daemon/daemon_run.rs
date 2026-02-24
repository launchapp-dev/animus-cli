use crate::cli_types::DaemonRunArgs;
use anyhow::Result;
use fs2::FileExt;
use orchestrator_core::{FileServiceHub, ServiceHub};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use super::daemon_events::{emit_daemon_event, next_daemon_event};
use super::daemon_registry::{
    canonicalize_lossy, get_registry_daemon_pid, set_registry_daemon_pid,
    set_registry_runtime_paused, sync_project_registry,
};
use super::daemon_scheduler::project_tick;

struct DaemonRunGuard {
    project_root: String,
    pid: u32,
    _lock_file: File,
}

impl Drop for DaemonRunGuard {
    fn drop(&mut self) {
        let _ = set_registry_runtime_paused(&self.project_root, true);
        if let Ok(Some(existing_pid)) = get_registry_daemon_pid(&self.project_root) {
            if existing_pid == self.pid {
                let _ = set_registry_daemon_pid(&self.project_root, None);
            }
        }
    }
}

fn daemon_lock_path(project_root: &str) -> PathBuf {
    PathBuf::from(canonicalize_lossy(project_root))
        .join(".ao")
        .join("daemon.lock")
}

fn read_daemon_lock_pid(lock_path: &PathBuf) -> Option<u32> {
    fs::read_to_string(lock_path)
        .ok()
        .and_then(|content| content.trim().parse::<u32>().ok())
}

fn acquire_daemon_run_guard(project_root: &str) -> Result<DaemonRunGuard> {
    let canonical_project_root = canonicalize_lossy(project_root);
    let current_pid = std::process::id();
    if let Some(existing_pid) = get_registry_daemon_pid(&canonical_project_root)? {
        if existing_pid != current_pid && super::is_process_alive(existing_pid) {
            anyhow::bail!(
                "daemon already running for project {} (pid {})",
                canonical_project_root,
                existing_pid
            );
        }
        if existing_pid != current_pid {
            let _ = set_registry_daemon_pid(&canonical_project_root, None);
        }
    }

    let lock_path = daemon_lock_path(&canonical_project_root);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;

    match lock_file.try_lock_exclusive() {
        Ok(_) => {
            lock_file.set_len(0)?;
            write!(&lock_file, "{current_pid}")?;
            lock_file.sync_all()?;
        }
        Err(_) => {
            if let Some(lock_pid) = read_daemon_lock_pid(&lock_path) {
                if lock_pid != current_pid && super::is_process_alive(lock_pid) {
                    anyhow::bail!(
                        "failed to acquire daemon lock for project {} (held by pid {})",
                        canonical_project_root,
                        lock_pid
                    );
                }
            }
            anyhow::bail!(
                "failed to acquire daemon lock for project {} (lock busy)",
                canonical_project_root
            );
        }
    }

    set_registry_daemon_pid(&canonical_project_root, Some(current_pid))?;
    set_registry_runtime_paused(&canonical_project_root, false)?;

    Ok(DaemonRunGuard {
        project_root: canonical_project_root,
        pid: current_pid,
        _lock_file: lock_file,
    })
}

fn restore_env_override(key: &str, original: Option<String>) {
    if let Some(value) = original {
        std::env::set_var(key, value);
    } else {
        std::env::remove_var(key);
    }
}

pub(super) async fn handle_daemon_run(
    args: DaemonRunArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let phase_timeout_override = args.phase_timeout_secs;
    let idle_timeout_override = args.idle_timeout_secs;
    let phase_timeout_original = phase_timeout_override
        .map(|_| std::env::var("AO_PHASE_TIMEOUT_SECS").ok())
        .flatten();
    let idle_timeout_original = idle_timeout_override
        .map(|_| std::env::var("AO_RUN_IDLE_TIMEOUT_SECS").ok())
        .flatten();

    if let Some(timeout_secs) = phase_timeout_override {
        std::env::set_var("AO_PHASE_TIMEOUT_SECS", timeout_secs.to_string());
    }
    if let Some(timeout_secs) = idle_timeout_override {
        std::env::set_var("AO_RUN_IDLE_TIMEOUT_SECS", timeout_secs.to_string());
    }

    let run_result = async {
        let _run_guard = acquire_daemon_run_guard(project_root)?;
        let daemon = hub.daemon();
        let primary_root = canonicalize_lossy(project_root);
        let initial_status = daemon.status().await?;
        let mut started_daemon_roots: HashSet<String> = HashSet::new();
        if !matches!(
            initial_status,
            orchestrator_core::DaemonStatus::Running | orchestrator_core::DaemonStatus::Paused
        ) {
            daemon.start().await?;
            started_daemon_roots.insert(primary_root.clone());
        }
        let _ = set_registry_runtime_paused(project_root, false);

        let mut seq = 0u64;
        emit_daemon_event(
            &next_daemon_event(
                &mut seq,
                "status",
                Some(primary_root.clone()),
                serde_json::json!({"status":"running"}),
            ),
            json,
        )?;

        if args.startup_cleanup {
            let entries = sync_project_registry(project_root, args.include_registry)?;
            emit_daemon_event(
                &next_daemon_event(
                    &mut seq,
                    "recovery",
                    None,
                    serde_json::json!({
                        "startup_cleanup": true,
                        "projects_discovered": entries.len(),
                    }),
                ),
                json,
            )?;
        }

        let interval = Duration::from_secs(args.interval_secs.max(1));
        loop {
            let entries = sync_project_registry(project_root, args.include_registry)?;
            for entry in entries {
                if entry.runtime_paused {
                    emit_daemon_event(
                        &next_daemon_event(
                            &mut seq,
                            "project",
                            Some(canonicalize_lossy(&entry.path)),
                            serde_json::json!({"paused": true}),
                        ),
                        json,
                    )?;
                    continue;
                }

                match project_tick(&entry.path, &args).await {
                    Ok(summary) => {
                        if summary.started_daemon {
                            started_daemon_roots.insert(summary.project_root.clone());
                        }
                        emit_daemon_event(
                            &next_daemon_event(
                                &mut seq,
                                "health",
                                Some(summary.project_root.clone()),
                                summary.health.clone(),
                            ),
                            json,
                        )?;
                        emit_daemon_event(
                            &next_daemon_event(
                                &mut seq,
                                "queue",
                                Some(summary.project_root.clone()),
                                serde_json::json!({
                                    "tasks_total": summary.tasks_total,
                                    "tasks_ready": summary.tasks_ready,
                                    "tasks_in_progress": summary.tasks_in_progress,
                                    "tasks_blocked": summary.tasks_blocked,
                                    "tasks_done": summary.tasks_done,
                                    "workflows_running": summary.workflows_running,
                                    "workflows_completed": summary.workflows_completed,
                                    "workflows_failed": summary.workflows_failed,
                                    "started_ready_workflows": summary.started_ready_workflows,
                                    "executed_workflow_phases": summary.executed_workflow_phases,
                                    "failed_workflow_phases": summary.failed_workflow_phases,
                                }),
                            ),
                            json,
                        )?;

                        emit_daemon_event(
                            &next_daemon_event(
                                &mut seq,
                                "workflow",
                                Some(summary.project_root.clone()),
                                serde_json::json!({
                                    "resumed_workflows": summary.resumed_workflows,
                                    "cleaned_stale_workflows": summary.cleaned_stale_workflows,
                                    "reconciled_stale_tasks": summary.reconciled_stale_tasks,
                                    "executed_workflow_phases": summary.executed_workflow_phases,
                                    "failed_workflow_phases": summary.failed_workflow_phases,
                                }),
                            ),
                            json,
                        )?;

                        for transition in &summary.requirement_lifecycle_transitions {
                            emit_daemon_event(
                                &next_daemon_event(
                                    &mut seq,
                                    "requirement-lifecycle",
                                    Some(summary.project_root.clone()),
                                    serde_json::json!({
                                        "requirement_id": transition.requirement_id,
                                        "requirement_title": transition.requirement_title,
                                        "phase": transition.phase,
                                        "status": transition.status,
                                        "transition_at": transition.transition_at,
                                        "comment": transition.comment,
                                    }),
                                ),
                                json,
                            )?;
                        }

                        for phase_event in &summary.phase_execution_events {
                            emit_daemon_event(
                                &next_daemon_event(
                                    &mut seq,
                                    &phase_event.event_type,
                                    Some(summary.project_root.clone()),
                                    serde_json::json!({
                                        "workflow_id": phase_event.workflow_id,
                                        "task_id": phase_event.task_id,
                                        "phase_id": phase_event.phase_id,
                                        "phase_mode": phase_event.phase_mode,
                                        "metadata": phase_event.metadata,
                                        "payload": phase_event.payload,
                                    }),
                                ),
                                json,
                            )?;
                        }
                    }
                    Err(error) => {
                        emit_daemon_event(
                            &next_daemon_event(
                                &mut seq,
                                "log",
                                Some(canonicalize_lossy(&entry.path)),
                                serde_json::json!({
                                    "level": "error",
                                    "message": error.to_string(),
                                }),
                            ),
                            json,
                        )?;
                    }
                }
            }

            if args.once {
                break;
            }

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    break;
                }
                _ = sleep(interval) => {}
            }
        }

        for root in &started_daemon_roots {
            if let Ok(project_hub) = FileServiceHub::new(root) {
                let _ = project_hub.daemon().stop().await;
            }
        }
        emit_daemon_event(
            &next_daemon_event(
                &mut seq,
                "status",
                Some(primary_root),
                serde_json::json!({"status":"stopped"}),
            ),
            json,
        )?;
        Ok(())
    }
    .await;

    if phase_timeout_override.is_some() {
        restore_env_override("AO_PHASE_TIMEOUT_SECS", phase_timeout_original);
    }
    if idle_timeout_override.is_some() {
        restore_env_override("AO_RUN_IDLE_TIMEOUT_SECS", idle_timeout_original);
    }

    run_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::runtime::runtime_daemon::{daemon_events_log_path, DaemonEventRecord};
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test]
    async fn daemon_run_once_processes_registry_projects() {
        let _lock = env_lock().lock().expect("env lock should be available");

        let config_root = TempDir::new().expect("config temp dir");
        let _config_guard = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(config_root.path().to_string_lossy().as_ref()),
        );
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
        let _skip_runner = EnvVarGuard::set("AO_SKIP_RUNNER_START", Some("1"));

        let primary = TempDir::new().expect("primary project dir");
        let secondary = TempDir::new().expect("secondary project dir");
        let primary_root = primary.path().to_string_lossy().to_string();
        let secondary_root = secondary.path().to_string_lossy().to_string();

        let primary_hub = Arc::new(FileServiceHub::new(&primary_root).expect("primary hub"));
        let _secondary_hub = Arc::new(FileServiceHub::new(&secondary_root).expect("secondary hub"));

        set_registry_runtime_paused(&primary_root, false).expect("primary registry entry");
        set_registry_runtime_paused(&secondary_root, false).expect("secondary registry entry");

        let args = DaemonRunArgs {
            interval_secs: 1,
            include_registry: true,
            ai_task_generation: false,
            auto_run_ready: false,
            startup_cleanup: true,
            resume_interrupted: false,
            reconcile_stale: false,
            max_tasks_per_tick: 1,
            phase_timeout_secs: None,
            idle_timeout_secs: None,
            once: true,
        };
        handle_daemon_run(
            args,
            primary_hub as Arc<dyn ServiceHub>,
            &primary_root,
            true,
        )
        .await
        .expect("daemon run should succeed");

        let events_path = daemon_events_log_path();
        let events_content =
            std::fs::read_to_string(events_path).expect("daemon events log should exist");
        let events: Vec<DaemonEventRecord> = events_content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<DaemonEventRecord>(line).expect("event json"))
            .collect();

        let roots: HashSet<String> = events
            .iter()
            .filter(|event| event.event_type == "health" || event.event_type == "queue")
            .filter_map(|event| event.project_root.clone())
            .collect();
        assert!(roots.contains(&canonicalize_lossy(&primary_root)));
        assert!(roots.contains(&canonicalize_lossy(&secondary_root)));
    }
}
