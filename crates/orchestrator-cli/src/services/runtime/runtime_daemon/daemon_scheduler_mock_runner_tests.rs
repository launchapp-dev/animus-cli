#![cfg(unix)]

use super::*;
use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use orchestrator_core::{
    builtin_workflow_config, load_schedule_state, write_workflow_config, FileServiceHub,
    TaskCreateInput, TaskStatus, TaskType, WorkflowSchedule,
};
use protocol::test_utils::EnvVarGuard;
use serde_json::json;
use std::{env, fs, os::unix::fs::PermissionsExt, path::Path};
use tempfile::TempDir;
use tokio::time::{sleep, Duration};

fn daemon_args(auto_run_ready: bool) -> DaemonRunArgs {
    DaemonRunArgs {
        pool_size: Some(2),
        max_agents: Some(2),
        interval_secs: 1,
        ai_task_generation: false,
        auto_run_ready,
        auto_merge: None,
        auto_pr: None,
        auto_commit_before_merge: None,
        auto_prune_worktrees_after_merge: None,
        startup_cleanup: false,
        resume_interrupted: false,
        reconcile_stale: false,
        stale_threshold_hours: 24,
        max_tasks_per_tick: 2,
        phase_timeout_secs: None,
        idle_timeout_secs: None,
        once: true,
    }
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn install_mock_runner(temp: &TempDir, exit_code: i32) -> Result<()> {
    let runner_path = temp.path().join("ao-workflow-runner");
    let args_log = temp.path().join("runner-args.log");
    let input_log = temp.path().join("runner-input.log");
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > {args_log}\nprintf '%s' \"${{AO_SCHEDULE_INPUT:-}}\" > {input_log}\nprintf '%s\\n' '{{\"event\":\"runner_start\"}}' >&2\nprintf '%s\\n' '{{\"event\":\"runner_complete\",\"exit_code\":{exit_code}}}' >&2\nexit {exit_code}\n",
        args_log = shell_quote(&args_log),
        input_log = shell_quote(&input_log),
        exit_code = exit_code,
    );

    fs::write(&runner_path, script).context("mock runner should be written")?;
    let mut permissions = fs::metadata(&runner_path)
        .context("mock runner metadata should be available")?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&runner_path, permissions).context("mock runner should be executable")?;
    Ok(())
}

fn prepend_runner_to_path(temp: &TempDir) -> Result<EnvVarGuard> {
    let original_path = env::var_os("PATH").unwrap_or_default();
    let mut paths = env::split_paths(&original_path).collect::<Vec<_>>();
    paths.insert(0, temp.path().to_path_buf());
    let joined = env::join_paths(paths).context("path list should join")?;
    let joined = joined.to_string_lossy().into_owned();
    Ok(EnvVarGuard::set("PATH", Some(joined.as_str())))
}

fn read_with_retry(path: &Path) -> Result<String> {
    for _ in 0..50 {
        if let Ok(value) = fs::read_to_string(path) {
            return Ok(value);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    fs::read_to_string(path).with_context(|| format!("{} should be readable", path.display()))
}

async fn run_until_schedule_completed(
    project_root: &str,
    args: &DaemonRunArgs,
    process_manager: &mut ProcessManager,
    now: chrono::DateTime<Utc>,
    schedule_id: &str,
) -> Result<ProjectTickSummary> {
    for _ in 0..100 {
        let summary = slim_project_tick_at(project_root, args, process_manager, false, now).await?;
        let state = load_schedule_state(Path::new(project_root))?;
        if state
            .schedules
            .get(schedule_id)
            .is_some_and(|entry| entry.last_status == "completed")
        {
            return Ok(summary);
        }
        sleep(Duration::from_millis(25)).await;
    }

    anyhow::bail!("schedule {schedule_id} did not reach completed state in time")
}

async fn run_until_task_done(
    project_root: &str,
    args: &DaemonRunArgs,
    process_manager: &mut ProcessManager,
    now: chrono::DateTime<Utc>,
    task_id: &str,
) -> Result<ProjectTickSummary> {
    for _ in 0..100 {
        let summary = slim_project_tick_at(project_root, args, process_manager, false, now).await?;
        let refreshed_hub = Arc::new(FileServiceHub::new(project_root)?);
        if refreshed_hub.tasks().get(task_id).await?.status == TaskStatus::Done {
            return Ok(summary);
        }
        sleep(Duration::from_millis(25)).await;
    }

    anyhow::bail!("task {task_id} did not reach done state in time")
}

#[tokio::test]
async fn slim_project_tick_processes_due_schedule_via_mock_runner() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
    let _skip_runner = EnvVarGuard::set("AO_SKIP_RUNNER_START", Some("1"));
    let runner_dir = TempDir::new().expect("runner temp dir should be created");
    install_mock_runner(&runner_dir, 0).expect("mock runner should be installed");
    let _path_guard = prepend_runner_to_path(&runner_dir).expect("path should be overridden");

    let project_root = TempDir::new().expect("project temp dir should be created");
    let project_root_str = project_root.path().to_string_lossy().to_string();
    let _hub = FileServiceHub::new(&project_root_str).expect("file service hub should initialize");

    let mut config = builtin_workflow_config();
    config.schedules.push(WorkflowSchedule {
        id: "nightly-review".to_string(),
        cron: "30 12 * * *".to_string(),
        workflow_ref: Some(orchestrator_core::STANDARD_WORKFLOW_REF.to_string()),
        command: None,
        input: Some(json!({"source":"schedule","count":1})),
        enabled: true,
    });
    write_workflow_config(project_root.path(), &config).expect("workflow config should be written");

    let now = Utc
        .with_ymd_and_hms(2026, 3, 7, 12, 30, 0)
        .single()
        .expect("timestamp should be valid");
    let args = daemon_args(false);
    let mut process_manager = ProcessManager::new();

    let first_summary =
        slim_project_tick_at(&project_root_str, &args, &mut process_manager, false, now)
            .await
            .expect("first daemon tick should succeed");
    let second_summary = run_until_schedule_completed(
        &project_root_str,
        &args,
        &mut process_manager,
        now,
        "nightly-review",
    )
    .await
    .expect("schedule should reconcile to completed");

    let args_log = read_with_retry(&runner_dir.path().join("runner-args.log"))
        .expect("mock runner args should be captured");
    assert!(args_log.contains("execute"));
    assert!(args_log.contains("--title"));
    assert!(args_log.contains("schedule:nightly-review"));
    assert!(args_log.contains("--workflow-ref"));
    assert!(args_log.contains(orchestrator_core::STANDARD_WORKFLOW_REF));

    let input_log = read_with_retry(&runner_dir.path().join("runner-input.log"))
        .expect("mock runner input should be captured");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&input_log).expect("schedule input should parse"),
        json!({"source":"schedule","count":1})
    );

    let schedule_state =
        load_schedule_state(project_root.path()).expect("schedule state should load");
    let run_state = schedule_state
        .schedules
        .get("nightly-review")
        .expect("schedule state entry should exist");
    assert_eq!(run_state.last_status, "completed");
    assert_eq!(run_state.run_count, 1);
    assert_eq!(second_summary.executed_workflow_phases, 1);
    assert_eq!(first_summary.started_ready_workflows, 0);
}

#[tokio::test]
async fn slim_project_tick_dispatches_and_reconciles_ready_task_via_mock_runner() {
    let _lock = crate::shared::test_env_lock()
        .lock()
        .expect("env lock should be available");
    let _skip_runner = EnvVarGuard::set("AO_SKIP_RUNNER_START", Some("1"));
    let runner_dir = TempDir::new().expect("runner temp dir should be created");
    install_mock_runner(&runner_dir, 0).expect("mock runner should be installed");
    let _path_guard = prepend_runner_to_path(&runner_dir).expect("path should be overridden");

    let project_root = TempDir::new().expect("project temp dir should be created");
    let project_root_str = project_root.path().to_string_lossy().to_string();
    let hub = Arc::new(FileServiceHub::new(&project_root_str).expect("file service hub"));
    let task = hub
        .tasks()
        .create(TaskCreateInput {
            title: "mock runner ready task".to_string(),
            description: "prove ready queue dispatch and reconciliation".to_string(),
            task_type: Some(TaskType::Feature),
            priority: None,
            created_by: Some("test".to_string()),
            tags: Vec::new(),
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
        })
        .await
        .expect("task should be created");
    hub.tasks()
        .set_status(&task.id, TaskStatus::Ready, false)
        .await
        .expect("task should be ready");

    let now = Utc
        .with_ymd_and_hms(2026, 3, 7, 12, 30, 0)
        .single()
        .expect("timestamp should be valid");
    let args = daemon_args(true);
    let mut process_manager = ProcessManager::new();

    let dispatch_summary =
        slim_project_tick_at(&project_root_str, &args, &mut process_manager, false, now)
            .await
            .expect("dispatch tick should succeed");
    assert_eq!(dispatch_summary.started_ready_workflows, 1);

    let reconcile_summary = run_until_task_done(
        &project_root_str,
        &args,
        &mut process_manager,
        now,
        &task.id,
    )
    .await
    .expect("task should reconcile to done");
    assert_eq!(reconcile_summary.executed_workflow_phases, 1);

    let args_log = read_with_retry(&runner_dir.path().join("runner-args.log"))
        .expect("mock runner args should be captured");
    assert!(args_log.contains("execute"));
    assert!(args_log.contains("--task-id"));
    assert!(args_log.contains(task.id.as_str()));

    let refreshed_hub =
        Arc::new(FileServiceHub::new(&project_root_str).expect("refreshed file service hub"));
    let updated_task = refreshed_hub
        .tasks()
        .get(&task.id)
        .await
        .expect("task should load");
    assert_eq!(updated_task.status, TaskStatus::Done);
}
