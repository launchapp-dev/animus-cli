use crate::cli_types::DaemonRunArgs;
use anyhow::Result;
use orchestrator_core::DaemonStatus;
use orchestrator_core::FileServiceHub;
use orchestrator_core::ServiceHub;
use orchestrator_daemon_runtime::{
    recover_orphaned_running_workflows, run_daemon, DaemonRunEvent, DaemonRunHooks, ProcessManager,
};
use std::sync::Arc;

#[cfg(test)]
use super::canonicalize_lossy;
use super::daemon_run_host::DefaultDaemonRunHost;
use super::daemon_scheduler::{runtime_options_from_cli, slim_project_tick_driver};

fn restore_env_override(key: &str, original: Option<String>) {
    if let Some(value) = original {
        std::env::set_var(key, value);
    } else {
        std::env::remove_var(key);
    }
}

struct CliDaemonRunHost {
    inner: DefaultDaemonRunHost,
}

impl CliDaemonRunHost {
    fn new(project_root: &str, json: bool) -> Self {
        Self {
            inner: DefaultDaemonRunHost::new(project_root, json),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl DaemonRunHooks for CliDaemonRunHost {
    fn handle_event(&mut self, event: DaemonRunEvent) -> Result<()> {
        self.inner.handle_event(event)
    }

    async fn daemon_status(&mut self, project_root: &str) -> Result<DaemonStatus> {
        let hub = FileServiceHub::new(project_root)?;
        hub.daemon().status().await
    }

    async fn start_daemon(&mut self, project_root: &str) -> Result<()> {
        let hub = FileServiceHub::new(project_root)?;
        hub.daemon().start().await
    }

    async fn stop_daemon(&mut self, project_root: &str) -> Result<()> {
        let hub = FileServiceHub::new(project_root)?;
        hub.daemon().stop().await
    }

    async fn recover_startup_orphans(&mut self, project_root: &str) -> Result<usize> {
        let startup_hub = Arc::new(FileServiceHub::new(project_root)?);
        Ok(recover_orphaned_running_workflows(
            startup_hub as Arc<dyn ServiceHub>,
            project_root,
            &std::collections::HashSet::new(),
        )
        .await)
    }

    async fn flush_notifications(&mut self, project_root: &str) -> Result<()> {
        self.inner.flush_notifications(project_root).await
    }
}

pub(super) async fn handle_daemon_run(
    args: DaemonRunArgs,
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

    let runtime_options = runtime_options_from_cli(&args);
    let mut process_manager = ProcessManager::new();
    let mut driver = slim_project_tick_driver(&mut process_manager);
    let mut host = CliDaemonRunHost::new(project_root, json);

    let run_result = run_daemon(
        project_root,
        &runtime_options,
        &mut driver,
        &mut host,
        |driver| driver.active_process_count(),
    )
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
    use std::path::PathBuf;
    use std::sync::MutexGuard;
    use tempfile::TempDir;

    fn lock_env() -> MutexGuard<'static, ()> {
        crate::shared::test_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    use protocol::test_utils::EnvVarGuard;

    #[tokio::test]
    async fn daemon_run_once_processes_current_project_root() {
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

        let args = DaemonRunArgs {
            pool_size: None,
            max_agents: None,
            interval_secs: 1,
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
        handle_daemon_run(args, &primary_root, true)
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
            .run(orchestrator_core::WorkflowRunInput::for_task(
                task.id.clone(),
                None,
            ))
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
            .set_status(&task.id, orchestrator_core::TaskStatus::InProgress, false)
            .await
            .expect("task should be stale in-progress");

        let args = DaemonRunArgs {
            pool_size: None,
            max_agents: None,
            interval_secs: 1,
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
        handle_daemon_run(args, &primary_root, true)
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

        let test_bin_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .expect("test binary directory");
        let release_bin_dir = test_bin_dir.parent().unwrap_or(&test_bin_dir);
        let path_with_bin_dir = format!(
            "{}:{}:{}",
            release_bin_dir.display(),
            test_bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let _path_guard = EnvVarGuard::set("PATH", Some(&path_with_bin_dir));

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
            .set_status(&task.id, orchestrator_core::TaskStatus::Ready, false)
            .await
            .expect("task should be ready");

        let args = DaemonRunArgs {
            pool_size: None,
            max_agents: None,
            interval_secs: 1,
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
        handle_daemon_run(args, &primary_root, true)
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
            pool_size: None,
            max_agents: None,
            interval_secs: 1,
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
        handle_daemon_run(args, &primary_root, true)
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
}
