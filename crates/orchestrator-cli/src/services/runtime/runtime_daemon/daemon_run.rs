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
use super::daemon_notifications::{DaemonNotificationRuntime, NotificationLifecycleEvent};
use super::daemon_registry::{
    canonicalize_lossy, get_registry_daemon_pid, set_registry_daemon_pid,
    set_registry_runtime_paused, sync_project_registry,
};
use super::daemon_scheduler::{
    clear_running_workflow_phase_pool, drain_running_workflow_phases_for_project,
    pause_running_workflow_phase_spawns, project_tick, resume_running_workflow_phase_spawns,
    subscribe_phase_completion_wake,
};

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

fn emit_notification_lifecycle_events(
    seq: &mut u64,
    events: Vec<NotificationLifecycleEvent>,
    json: bool,
) -> Result<()> {
    for event in events {
        emit_daemon_event(
            &next_daemon_event(seq, &event.event_type, event.project_root, event.data),
            json,
        )?;
    }
    Ok(())
}

fn emit_notification_runtime_error(
    seq: &mut u64,
    project_root: Option<String>,
    stage: &str,
    error: &str,
    json: bool,
) -> Result<()> {
    emit_daemon_event(
        &next_daemon_event(
            seq,
            "notification-runtime-error",
            project_root,
            serde_json::json!({
                "stage": stage,
                "message": error,
            }),
        ),
        json,
    )
}

fn emit_daemon_event_with_notifications(
    seq: &mut u64,
    event_type: &str,
    project_root: Option<String>,
    data: serde_json::Value,
    json: bool,
    notification_runtime: Option<&mut DaemonNotificationRuntime>,
) -> Result<()> {
    let record = next_daemon_event(seq, event_type, project_root, data);
    emit_daemon_event(&record, json)?;

    if let Some(runtime) = notification_runtime {
        match runtime.enqueue_for_event(&record) {
            Ok(lifecycle_events) => {
                emit_notification_lifecycle_events(seq, lifecycle_events, json)?
            }
            Err(error) => {
                let error_message = error.to_string();
                emit_notification_runtime_error(
                    seq,
                    record.project_root.clone(),
                    "enqueue",
                    error_message.as_str(),
                    json,
                )?
            }
        }
    }
    Ok(())
}

fn emit_project_tick_summary_events(
    seq: &mut u64,
    summary: &super::daemon_scheduler::ProjectTickSummary,
    json: bool,
    notification_runtime: &mut Option<DaemonNotificationRuntime>,
) -> Result<()> {
    emit_daemon_event_with_notifications(
        seq,
        "health",
        Some(summary.project_root.clone()),
        summary.health.clone(),
        json,
        notification_runtime.as_mut(),
    )?;
    emit_daemon_event_with_notifications(
        seq,
        "queue",
        Some(summary.project_root.clone()),
        serde_json::json!({
            "tasks_total": summary.tasks_total,
            "tasks_ready": summary.tasks_ready,
            "tasks_in_progress": summary.tasks_in_progress,
            "tasks_blocked": summary.tasks_blocked,
            "tasks_done": summary.tasks_done,
            "stale_in_progress_count": summary.stale_in_progress_count,
            "stale_in_progress_threshold_hours": summary.stale_in_progress_threshold_hours,
            "stale_in_progress_task_ids": summary.stale_in_progress_task_ids,
            "workflows_running": summary.workflows_running,
            "workflows_completed": summary.workflows_completed,
            "workflows_failed": summary.workflows_failed,
            "started_ready_workflows": summary.started_ready_workflows,
            "executed_workflow_phases": summary.executed_workflow_phases,
            "failed_workflow_phases": summary.failed_workflow_phases,
        }),
        json,
        notification_runtime.as_mut(),
    )?;

    emit_daemon_event_with_notifications(
        seq,
        "workflow",
        Some(summary.project_root.clone()),
        serde_json::json!({
            "resumed_workflows": summary.resumed_workflows,
            "cleaned_stale_workflows": summary.cleaned_stale_workflows,
            "reconciled_stale_tasks": summary.reconciled_stale_tasks,
            "executed_workflow_phases": summary.executed_workflow_phases,
            "failed_workflow_phases": summary.failed_workflow_phases,
        }),
        json,
        notification_runtime.as_mut(),
    )?;

    for transition in &summary.requirement_lifecycle_transitions {
        emit_daemon_event_with_notifications(
            seq,
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
            json,
            notification_runtime.as_mut(),
        )?;
    }

    for transition in &summary.task_state_transitions {
        emit_daemon_event_with_notifications(
            seq,
            "task-state-change",
            Some(summary.project_root.clone()),
            serde_json::json!({
                "task_id": transition.task_id,
                "from_status": transition.from_status,
                "to_status": transition.to_status,
                "changed_at": transition.changed_at,
                "workflow_id": transition.workflow_id,
                "phase_id": transition.phase_id,
                "selection_source": transition.selection_source,
            }),
            json,
            notification_runtime.as_mut(),
        )?;
    }

    for phase_event in &summary.phase_execution_events {
        emit_daemon_event_with_notifications(
            seq,
            &phase_event.event_type,
            Some(phase_event.project_root.clone()),
            serde_json::json!({
                "workflow_id": phase_event.workflow_id,
                "task_id": phase_event.task_id,
                "phase_id": phase_event.phase_id,
                "phase_mode": phase_event.phase_mode,
                "metadata": phase_event.metadata,
                "payload": phase_event.payload,
            }),
            json,
            notification_runtime.as_mut(),
        )?;
    }

    Ok(())
}

pub(super) async fn handle_daemon_run(
    args: DaemonRunArgs,
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    let auto_merge_override = args.auto_merge;
    let auto_pr_override = args.auto_pr;
    let auto_commit_before_merge_override = args.auto_commit_before_merge;
    let auto_prune_worktrees_after_merge_override = args.auto_prune_worktrees_after_merge;
    let phase_timeout_override = args.phase_timeout_secs;
    let idle_timeout_override = args.idle_timeout_secs;
    let auto_merge_original = auto_merge_override
        .map(|_| std::env::var("AO_AUTO_MERGE_ENABLED").ok())
        .flatten();
    let auto_pr_original = auto_pr_override
        .map(|_| std::env::var("AO_AUTO_PR_ENABLED").ok())
        .flatten();
    let auto_commit_before_merge_original = auto_commit_before_merge_override
        .map(|_| std::env::var("AO_AUTO_COMMIT_BEFORE_MERGE").ok())
        .flatten();
    let auto_prune_worktrees_after_merge_original = auto_prune_worktrees_after_merge_override
        .map(|_| std::env::var("AO_AUTO_PRUNE_WORKTREES_AFTER_MERGE").ok())
        .flatten();
    let phase_timeout_original = phase_timeout_override
        .map(|_| std::env::var("AO_PHASE_TIMEOUT_SECS").ok())
        .flatten();
    let idle_timeout_original = idle_timeout_override
        .map(|_| std::env::var("AO_RUN_IDLE_TIMEOUT_SECS").ok())
        .flatten();

    if let Some(enabled) = auto_merge_override {
        std::env::set_var("AO_AUTO_MERGE_ENABLED", if enabled { "1" } else { "0" });
    }
    if let Some(enabled) = auto_pr_override {
        std::env::set_var("AO_AUTO_PR_ENABLED", if enabled { "1" } else { "0" });
    }
    if let Some(enabled) = auto_commit_before_merge_override {
        std::env::set_var(
            "AO_AUTO_COMMIT_BEFORE_MERGE",
            if enabled { "1" } else { "0" },
        );
    }
    if let Some(enabled) = auto_prune_worktrees_after_merge_override {
        std::env::set_var(
            "AO_AUTO_PRUNE_WORKTREES_AFTER_MERGE",
            if enabled { "1" } else { "0" },
        );
    }
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
        let mut notification_startup_error = None;
        let mut notification_runtime = match DaemonNotificationRuntime::new(project_root) {
            Ok(runtime) => Some(runtime),
            Err(error) => {
                notification_startup_error = Some(error.to_string());
                None
            }
        };
        if let Some(error) = notification_startup_error.as_deref() {
            emit_notification_runtime_error(
                &mut seq,
                Some(primary_root.clone()),
                "startup",
                error,
                json,
            )?;
        }

        emit_daemon_event_with_notifications(
            &mut seq,
            "status",
            Some(primary_root.clone()),
            serde_json::json!({"status":"running"}),
            json,
            notification_runtime.as_mut(),
        )?;

        if args.startup_cleanup {
            let entries = sync_project_registry(project_root, args.include_registry)?;
            emit_daemon_event_with_notifications(
                &mut seq,
                "recovery",
                None,
                serde_json::json!({
                    "startup_cleanup": true,
                    "projects_discovered": entries.len(),
                }),
                json,
                notification_runtime.as_mut(),
            )?;
        }

        let interval = Duration::from_secs(args.interval_secs.max(1));
        let mut completion_wake_rx = subscribe_phase_completion_wake();
        loop {
            let entries = sync_project_registry(project_root, args.include_registry)?;
            for entry in &entries {
                if entry.runtime_paused {
                    emit_daemon_event_with_notifications(
                        &mut seq,
                        "project",
                        Some(canonicalize_lossy(&entry.path)),
                        serde_json::json!({"paused": true}),
                        json,
                        notification_runtime.as_mut(),
                    )?;
                    continue;
                }

                resume_running_workflow_phase_spawns(&entry.path);
                match project_tick(&entry.path, &args).await {
                    Ok(summary) => {
                        if summary.started_daemon {
                            started_daemon_roots.insert(summary.project_root.clone());
                        }
                        emit_project_tick_summary_events(
                            &mut seq,
                            &summary,
                            json,
                            &mut notification_runtime,
                        )?;
                    }
                    Err(error) => {
                        emit_daemon_event_with_notifications(
                            &mut seq,
                            "log",
                            Some(canonicalize_lossy(&entry.path)),
                            serde_json::json!({
                                "level": "error",
                                "message": error.to_string(),
                            }),
                            json,
                            notification_runtime.as_mut(),
                        )?;
                    }
                }
            }

            if let Some(runtime) = notification_runtime.as_mut() {
                match runtime.flush_due_deliveries().await {
                    Ok(lifecycle_events) => {
                        emit_notification_lifecycle_events(&mut seq, lifecycle_events, json)?
                    }
                    Err(error) => {
                        let error_message = error.to_string();
                        emit_notification_runtime_error(
                            &mut seq,
                            Some(primary_root.clone()),
                            "flush",
                            error_message.as_str(),
                            json,
                        )?
                    }
                }
            }

            if args.once {
                for entry in &entries {
                    if entry.runtime_paused {
                        clear_running_workflow_phase_pool(&entry.path);
                        continue;
                    }
                    pause_running_workflow_phase_spawns(&entry.path);
                    let project_hub: Arc<dyn ServiceHub> = match FileServiceHub::new(&entry.path) {
                        Ok(hub) => Arc::new(hub),
                        Err(error) => {
                            emit_daemon_event_with_notifications(
                                &mut seq,
                                "log",
                                Some(canonicalize_lossy(&entry.path)),
                                serde_json::json!({
                                    "level": "error",
                                    "message": format!(
                                        "failed to initialize project hub while draining phase pool: {}",
                                        error
                                    ),
                                }),
                                json,
                                notification_runtime.as_mut(),
                            )?;
                            clear_running_workflow_phase_pool(&entry.path);
                            continue;
                        }
                    };
                    if let Err(error) = drain_running_workflow_phases_for_project(
                        project_hub,
                        &entry.path,
                        args.max_tasks_per_tick,
                    )
                    .await
                    {
                        emit_daemon_event_with_notifications(
                            &mut seq,
                            "log",
                            Some(canonicalize_lossy(&entry.path)),
                            serde_json::json!({
                                "level": "error",
                                "message": format!(
                                    "failed draining in-flight workflow phases during once execution: {}",
                                    error
                                ),
                            }),
                            json,
                            notification_runtime.as_mut(),
                        )?;
                    }
                    clear_running_workflow_phase_pool(&entry.path);
                }
                break;
            }

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    for entry in &entries {
                        if entry.runtime_paused {
                            clear_running_workflow_phase_pool(&entry.path);
                            continue;
                        }
                        pause_running_workflow_phase_spawns(&entry.path);
                        let project_hub: Arc<dyn ServiceHub> = match FileServiceHub::new(&entry.path) {
                            Ok(hub) => Arc::new(hub),
                            Err(error) => {
                                emit_daemon_event_with_notifications(
                                    &mut seq,
                                    "log",
                                    Some(canonicalize_lossy(&entry.path)),
                                    serde_json::json!({
                                        "level": "error",
                                        "message": format!(
                                            "failed to initialize project hub while draining phase pool: {}",
                                            error
                                        ),
                                    }),
                                    json,
                                    notification_runtime.as_mut(),
                                )?;
                                clear_running_workflow_phase_pool(&entry.path);
                                continue;
                            }
                        };
                        if let Err(error) = drain_running_workflow_phases_for_project(
                            project_hub,
                            &entry.path,
                            args.max_tasks_per_tick,
                        )
                        .await
                        {
                            emit_daemon_event_with_notifications(
                                &mut seq,
                                "log",
                                Some(canonicalize_lossy(&entry.path)),
                                serde_json::json!({
                                    "level": "error",
                                    "message": format!(
                                        "failed draining in-flight workflow phases during shutdown: {}",
                                        error
                                    ),
                                }),
                                json,
                                notification_runtime.as_mut(),
                            )?;
                        }
                        clear_running_workflow_phase_pool(&entry.path);
                    }
                    break;
                }
                wake = completion_wake_rx.recv() => {
                    if wake.is_err() {
                        completion_wake_rx = subscribe_phase_completion_wake();
                    }
                }
                _ = sleep(interval) => {}
            }
        }

        for root in &started_daemon_roots {
            if let Ok(project_hub) = FileServiceHub::new(root) {
                let _ = project_hub.daemon().stop().await;
            }
        }
        emit_daemon_event_with_notifications(
            &mut seq,
            "status",
            Some(primary_root),
            serde_json::json!({"status":"stopped"}),
            json,
            notification_runtime.as_mut(),
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
    if auto_merge_override.is_some() {
        restore_env_override("AO_AUTO_MERGE_ENABLED", auto_merge_original);
    }
    if auto_pr_override.is_some() {
        restore_env_override("AO_AUTO_PR_ENABLED", auto_pr_original);
    }
    if auto_commit_before_merge_override.is_some() {
        restore_env_override(
            "AO_AUTO_COMMIT_BEFORE_MERGE",
            auto_commit_before_merge_original,
        );
    }
    if auto_prune_worktrees_after_merge_override.is_some() {
        restore_env_override(
            "AO_AUTO_PRUNE_WORKTREES_AFTER_MERGE",
            auto_prune_worktrees_after_merge_original,
        );
    }

    run_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::runtime::runtime_daemon::{daemon_events_log_path, DaemonEventRecord};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn lock_env() -> MutexGuard<'static, ()> {
        env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let _lock = lock_env();

        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(config_root.path().to_string_lossy().as_ref()),
        );
        let _home_guard =
            EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
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
            auto_merge: None,
            auto_pr: None,
            auto_commit_before_merge: None,
            auto_prune_worktrees_after_merge: None,
            startup_cleanup: true,
            resume_interrupted: false,
            reconcile_stale: false,
            stale_threshold_hours: 24,
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

        let queue_event = events
            .iter()
            .find(|event| {
                event.event_type == "queue"
                    && event.project_root.as_deref()
                        == Some(canonicalize_lossy(&primary_root).as_str())
            })
            .expect("queue event for primary project should exist");
        for field in [
            "stale_in_progress_count",
            "stale_in_progress_threshold_hours",
            "started_ready_workflows",
            "executed_workflow_phases",
            "failed_workflow_phases",
        ] {
            assert!(
                queue_event
                    .data
                    .get(field)
                    .and_then(serde_json::Value::as_u64)
                    .is_some(),
                "queue event field `{field}` should be present as an integer"
            );
        }
        assert!(
            queue_event
                .data
                .get("stale_in_progress_task_ids")
                .and_then(serde_json::Value::as_array)
                .is_some(),
            "queue event field `stale_in_progress_task_ids` should be present as an array"
        );
    }

    #[tokio::test]
    async fn daemon_run_emits_task_state_change_events() {
        let _lock = lock_env();

        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(config_root.path().to_string_lossy().as_ref()),
        );
        let _home_guard =
            EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
        let _skip_runner = EnvVarGuard::set("AO_SKIP_RUNNER_START", Some("1"));

        let primary = TempDir::new().expect("primary project dir");
        let primary_root = primary.path().to_string_lossy().to_string();
        let primary_hub = Arc::new(FileServiceHub::new(&primary_root).expect("primary hub"));

        let task = primary_hub
            .tasks()
            .create(orchestrator_core::TaskCreateInput {
                title: "transition task".to_string(),
                description: "verify task-state-change daemon events".to_string(),
                task_type: Some(orchestrator_core::TaskType::Feature),
                priority: Some(orchestrator_core::Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");

        let mut workflow = primary_hub
            .workflows()
            .run(orchestrator_core::WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should run");
        for _ in 0..12 {
            if workflow.status == orchestrator_core::WorkflowStatus::Completed {
                break;
            }
            workflow = primary_hub
                .workflows()
                .complete_current_phase(&workflow.id)
                .await
                .expect("phase should complete");
        }
        assert_eq!(
            workflow.status,
            orchestrator_core::WorkflowStatus::Completed
        );

        primary_hub
            .tasks()
            .set_status(&task.id, orchestrator_core::TaskStatus::InProgress)
            .await
            .expect("task should be stale in-progress");

        set_registry_runtime_paused(&primary_root, false).expect("primary registry entry");

        let args = DaemonRunArgs {
            interval_secs: 1,
            include_registry: false,
            ai_task_generation: false,
            auto_run_ready: false,
            auto_merge: None,
            auto_pr: None,
            auto_commit_before_merge: None,
            auto_prune_worktrees_after_merge: None,
            startup_cleanup: false,
            resume_interrupted: false,
            reconcile_stale: true,
            stale_threshold_hours: 24,
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
        .expect("daemon run should emit transition event");

        let events_path = daemon_events_log_path();
        let events_content =
            std::fs::read_to_string(events_path).expect("daemon events log should exist");
        let events: Vec<DaemonEventRecord> = events_content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<DaemonEventRecord>(line).expect("event json"))
            .collect();

        let transition_event = events
            .iter()
            .find(|event| {
                event.event_type == "task-state-change"
                    && event.project_root.as_deref()
                        == Some(canonicalize_lossy(&primary_root).as_str())
                    && event
                        .data
                        .get("task_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(task.id.as_str())
            })
            .expect("task-state-change event should be emitted");
        assert_eq!(
            transition_event
                .data
                .get("from_status")
                .and_then(serde_json::Value::as_str),
            Some("in-progress")
        );
        assert_eq!(
            transition_event
                .data
                .get("to_status")
                .and_then(serde_json::Value::as_str),
            Some("done")
        );
        assert!(transition_event
            .data
            .get("changed_at")
            .and_then(serde_json::Value::as_str)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn daemon_run_emits_selection_source_for_started_task_events() {
        let _lock = lock_env();

        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(config_root.path().to_string_lossy().as_ref()),
        );
        let _home_guard =
            EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
        let _skip_runner = EnvVarGuard::set("AO_SKIP_RUNNER_START", Some("1"));

        let primary = TempDir::new().expect("primary project dir");
        let primary_root = primary.path().to_string_lossy().to_string();
        let primary_hub = Arc::new(FileServiceHub::new(&primary_root).expect("primary hub"));

        let task = primary_hub
            .tasks()
            .create(orchestrator_core::TaskCreateInput {
                title: "start selection source task".to_string(),
                description: "verify daemon emits selection source on workflow start".to_string(),
                task_type: Some(orchestrator_core::TaskType::Feature),
                priority: Some(orchestrator_core::Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        primary_hub
            .tasks()
            .set_status(&task.id, orchestrator_core::TaskStatus::Ready)
            .await
            .expect("task should be ready");

        set_registry_runtime_paused(&primary_root, false).expect("primary registry entry");

        let args = DaemonRunArgs {
            interval_secs: 1,
            include_registry: false,
            ai_task_generation: false,
            auto_run_ready: true,
            auto_merge: None,
            auto_pr: None,
            auto_commit_before_merge: None,
            auto_prune_worktrees_after_merge: None,
            startup_cleanup: false,
            resume_interrupted: false,
            reconcile_stale: false,
            stale_threshold_hours: 24,
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
        .expect("daemon run should emit selection source transition");

        let events_path = daemon_events_log_path();
        let events_content =
            std::fs::read_to_string(events_path).expect("daemon events log should exist");
        let events: Vec<DaemonEventRecord> = events_content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<DaemonEventRecord>(line).expect("event json"))
            .collect();

        let selection_event = events
            .iter()
            .find(|event| {
                event.event_type == "task-state-change"
                    && event.project_root.as_deref()
                        == Some(canonicalize_lossy(&primary_root).as_str())
                    && event
                        .data
                        .get("task_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(task.id.as_str())
                    && event
                        .data
                        .get("selection_source")
                        .and_then(serde_json::Value::as_str)
                        .is_some()
            })
            .expect("task-state-change event with selection source should be emitted");

        assert_eq!(
            selection_event
                .data
                .get("selection_source")
                .and_then(serde_json::Value::as_str),
            Some("fallback_picker")
        );
    }

    #[tokio::test]
    async fn daemon_run_continues_when_notification_delivery_fails() {
        let _lock = lock_env();

        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(config_root.path().to_string_lossy().as_ref()),
        );
        let _home_guard =
            EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
        let _skip_runner = EnvVarGuard::set("AO_SKIP_RUNNER_START", Some("1"));
        let _missing_url = EnvVarGuard::set("AO_NOTIFY_MISSING_URL", None);

        let primary = TempDir::new().expect("primary project dir");
        let primary_root = primary.path().to_string_lossy().to_string();
        let primary_hub = Arc::new(FileServiceHub::new(&primary_root).expect("primary hub"));
        set_registry_runtime_paused(&primary_root, false).expect("primary registry entry");

        let pm_config_path = PathBuf::from(&primary_root)
            .join(".ao")
            .join("pm-config.json");
        let pm_config = serde_json::json!({
            "notification_config": {
                "schema": "ao.daemon-notification-config.v1",
                "version": 1,
                "connectors": [
                    {
                        "type": "webhook",
                        "id": "ops-webhook",
                        "enabled": true,
                        "url_env": "AO_NOTIFY_MISSING_URL"
                    }
                ],
                "subscriptions": [
                    {
                        "id": "all-events",
                        "enabled": true,
                        "connector_id": "ops-webhook",
                        "event_types": ["*"]
                    }
                ],
                "retry_policy": {
                    "max_attempts": 1,
                    "base_delay_secs": 1,
                    "max_delay_secs": 5
                },
                "max_deliveries_per_tick": 8
            }
        });
        std::fs::write(
            &pm_config_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&pm_config).expect("serialize config")
            ),
        )
        .expect("pm-config should be written");

        let args = DaemonRunArgs {
            interval_secs: 1,
            include_registry: false,
            ai_task_generation: false,
            auto_run_ready: false,
            auto_merge: None,
            auto_pr: None,
            auto_commit_before_merge: None,
            auto_prune_worktrees_after_merge: None,
            startup_cleanup: true,
            resume_interrupted: false,
            reconcile_stale: false,
            stale_threshold_hours: 24,
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
        .expect("daemon run should succeed even when notification delivery fails");

        let events_path = daemon_events_log_path();
        let events_content =
            std::fs::read_to_string(events_path).expect("daemon events log should exist");
        let events: Vec<DaemonEventRecord> = events_content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<DaemonEventRecord>(line).expect("event json"))
            .collect();

        assert!(events
            .iter()
            .any(|event| event.event_type == "notification-delivery-dead-lettered"));
    }

    #[tokio::test]
    async fn daemon_run_emits_log_error_event_for_project_tick_failure() {
        let _lock = lock_env();

        let config_root = TempDir::new().expect("config temp dir");
        let home_root = TempDir::new().expect("home temp dir");
        let _config_guard = EnvVarGuard::set(
            "AO_CONFIG_DIR",
            Some(config_root.path().to_string_lossy().as_ref()),
        );
        let _home_guard =
            EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);
        let _skip_runner = EnvVarGuard::set("AO_SKIP_RUNNER_START", Some("1"));

        let primary = TempDir::new().expect("primary project dir");
        let primary_root = primary.path().to_string_lossy().to_string();
        let primary_hub = Arc::new(FileServiceHub::new(&primary_root).expect("primary hub"));
        set_registry_runtime_paused(&primary_root, false).expect("primary registry entry");

        let invalid_root = config_root.path().join("invalid-project-root");
        std::fs::write(&invalid_root, "not a directory")
            .expect("invalid registry root file should be created");
        let registry_path = protocol::Config::global_config_dir().join("projects.json");
        if let Some(parent) = registry_path.parent() {
            std::fs::create_dir_all(parent).expect("registry directory should be created");
        }
        std::fs::write(
            &registry_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "entries": [
                    {
                        "name": "invalid-root",
                        "path": invalid_root.to_string_lossy().to_string(),
                        "runtime_paused": false,
                        "archived": false,
                        "pinned": false
                    }
                ]
            }))
            .expect("registry should serialize"),
        )
        .expect("registry should be written");

        let args = DaemonRunArgs {
            interval_secs: 1,
            include_registry: true,
            ai_task_generation: false,
            auto_run_ready: false,
            auto_merge: None,
            auto_pr: None,
            auto_commit_before_merge: None,
            auto_prune_worktrees_after_merge: None,
            startup_cleanup: false,
            resume_interrupted: false,
            reconcile_stale: false,
            stale_threshold_hours: 24,
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
        .expect("daemon run should succeed while recording tick errors");

        let events_path = daemon_events_log_path();
        let events_content =
            std::fs::read_to_string(events_path).expect("daemon events log should exist");
        let events: Vec<DaemonEventRecord> = events_content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<DaemonEventRecord>(line).expect("event json"))
            .collect();

        let invalid_root_canonical = canonicalize_lossy(invalid_root.to_string_lossy().as_ref());
        let error_event = events
            .iter()
            .find(|event| {
                event.event_type == "log"
                    && event.project_root.as_deref() == Some(invalid_root_canonical.as_str())
                    && event.data.get("level").and_then(serde_json::Value::as_str) == Some("error")
            })
            .expect("tick failure should be emitted as queryable log error event");
        assert!(error_event
            .data
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map(|message| !message.trim().is_empty())
            .unwrap_or(false));
    }
}
