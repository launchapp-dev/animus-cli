use anyhow::{Context, Result};
use orchestrator_daemon_runtime::ScheduleDispatch as RuntimeScheduleDispatch;

use crate::services::runtime::runtime_daemon::daemon_process_manager::{
    ProcessManager, WorkflowSubjectArgs,
};

pub(super) struct ScheduleDispatch;

impl ScheduleDispatch {
    pub(super) fn allows_proactive_dispatch(
        active_hours: Option<&str>,
        now: chrono::NaiveTime,
    ) -> bool {
        RuntimeScheduleDispatch::allows_proactive_dispatch(active_hours, now)
    }

    pub(super) fn process_due_schedules(
        process_manager: &mut ProcessManager,
        project_root: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        RuntimeScheduleDispatch::process_due_schedules(
            project_root,
            now,
            |schedule_id, pipeline_id, input_json| {
                spawn_schedule_pipeline(
                    process_manager,
                    project_root,
                    schedule_id,
                    pipeline_id,
                    input_json,
                )
            },
            |schedule_id, command| spawn_schedule_command(project_root, schedule_id, command),
        );
    }

    pub(super) fn update_completion_state(project_root: &str, schedule_id: &str, status: &str) {
        RuntimeScheduleDispatch::update_completion_state(project_root, schedule_id, status);
    }
}

fn spawn_schedule_pipeline(
    process_manager: &mut ProcessManager,
    project_root: &str,
    schedule_id: &str,
    pipeline_id: &str,
    input_json: Option<&str>,
) -> Result<()> {
    let subject = WorkflowSubjectArgs::Custom {
        title: format!("schedule:{schedule_id}"),
        description: Some(format!("Triggered by schedule '{schedule_id}'")),
        input_json: input_json.map(String::from),
    };
    process_manager.spawn_workflow_runner(&subject, pipeline_id, project_root)?;

    eprintln!(
        "{}: schedule '{}' fired pipeline '{}'",
        protocol::ACTOR_DAEMON,
        schedule_id,
        pipeline_id
    );
    Ok(())
}

fn spawn_schedule_command(project_root: &str, schedule_id: &str, command: &str) -> Result<()> {
    use tokio::process::Command as TokioCommand;

    let mut child = TokioCommand::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(project_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to spawn schedule command for '{schedule_id}'"))?;

    eprintln!(
        "{}: schedule '{}' fired command: {}",
        protocol::ACTOR_DAEMON,
        schedule_id,
        command
    );

    let root = project_root.to_string();
    let sched_id = schedule_id.to_string();
    tokio::spawn(async move {
        let status = match child.wait().await {
            Ok(exit) if exit.success() => "completed",
            Ok(_) => "failed",
            Err(_) => "failed",
        };
        ScheduleDispatch::update_completion_state(&root, &sched_id, status);
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use orchestrator_core::{
        load_schedule_state, save_schedule_state, ScheduleRunState, ScheduleState,
    };
    use tempfile::tempdir;

    use super::{spawn_schedule_command, ScheduleDispatch};

    async fn wait_for_schedule_status(project_root: &std::path::Path, schedule_id: &str) -> String {
        for _ in 0..50 {
            let state = load_schedule_state(project_root).expect("schedule state should load");
            if let Some(entry) = state.schedules.get(schedule_id) {
                if entry.last_status != "dispatched" {
                    return entry.last_status.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        load_schedule_state(project_root)
            .expect("schedule state should load")
            .schedules
            .get(schedule_id)
            .map(|entry| entry.last_status.clone())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn schedule_command_updates_completion_state_on_success() {
        let temp = tempdir().expect("tempdir should be created");
        let project_root = temp.path();
        let mut state = ScheduleState::default();
        state.schedules.insert(
            "nightly".to_string(),
            ScheduleRunState {
                last_run: Some(chrono::Utc::now()),
                last_status: "dispatched".to_string(),
                run_count: 1,
            },
        );
        save_schedule_state(project_root, &state).expect("initial schedule state should save");

        spawn_schedule_command(project_root.to_string_lossy().as_ref(), "nightly", "exit 0")
            .expect("command schedule should spawn");

        let status = wait_for_schedule_status(project_root, "nightly").await;
        assert_eq!(status, "completed");
    }

    #[tokio::test]
    async fn schedule_command_updates_completion_state_on_failure() {
        let temp = tempdir().expect("tempdir should be created");
        let project_root = temp.path();
        let mut state = ScheduleState::default();
        state.schedules.insert(
            "nightly".to_string(),
            ScheduleRunState {
                last_run: Some(chrono::Utc::now()),
                last_status: "dispatched".to_string(),
                run_count: 1,
            },
        );
        save_schedule_state(project_root, &state).expect("initial schedule state should save");

        spawn_schedule_command(project_root.to_string_lossy().as_ref(), "nightly", "exit 1")
            .expect("command schedule should spawn");

        let status = wait_for_schedule_status(project_root, "nightly").await;
        assert_eq!(status, "failed");
    }

    #[test]
    fn update_completion_state_overwrites_existing_status() {
        let temp = tempdir().expect("tempdir should be created");
        let project_root = temp.path();
        let mut state = ScheduleState::default();
        state.schedules.insert(
            "nightly".to_string(),
            ScheduleRunState {
                last_run: Some(chrono::Utc::now()),
                last_status: "dispatched".to_string(),
                run_count: 1,
            },
        );
        save_schedule_state(project_root, &state).expect("initial schedule state should save");

        ScheduleDispatch::update_completion_state(
            project_root.to_string_lossy().as_ref(),
            "nightly",
            "completed",
        );

        let loaded = load_schedule_state(project_root).expect("schedule state should load");
        assert_eq!(
            loaded
                .schedules
                .get("nightly")
                .expect("nightly state should exist")
                .last_status,
            "completed"
        );
    }
}
