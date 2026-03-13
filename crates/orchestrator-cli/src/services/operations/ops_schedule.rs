use std::path::Path;

use crate::{not_found_error, print_ok, print_value, ScheduleCommand};
use anyhow::{Context, Result};
use chrono::Utc;
use orchestrator_core::{
    load_schedule_state, load_workflow_config, project_schedule_dispatch_attempt,
    project_schedule_execution_fact,
};
use orchestrator_daemon_runtime::{
    build_completion_reconciliation_plan, build_runner_command_from_dispatch, CompletedProcess,
    RunnerEvent, ScheduleDispatch as RuntimeScheduleDispatch, SubjectDispatch,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct ScheduleListEntry {
    id: String,
    cron: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    enabled: bool,
    last_run: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScheduleHistoryEntry {
    id: String,
    last_run: Option<String>,
    last_status: String,
    run_count: u64,
}

pub(crate) async fn handle_schedule(
    command: ScheduleCommand,
    _hub: std::sync::Arc<dyn orchestrator_core::services::ServiceHub>,
    project_root: &str,
    json: bool,
) -> Result<()> {
    match command {
        ScheduleCommand::List => {
            let schedule_config = load_workflow_config(Path::new(project_root))
                .with_context(|| format!("failed to load workflow config from {}", project_root))?;
            let schedule_state = load_schedule_state(Path::new(project_root))
                .with_context(|| format!("failed to load schedule state from {}", project_root))?;
            let rows = schedule_config
                .schedules
                .iter()
                .map(|schedule| {
                    let run_state = schedule_state.schedules.get(&schedule.id);
                    ScheduleListEntry {
                        id: schedule.id.clone(),
                        cron: schedule.cron.clone(),
                        workflow_ref: schedule.workflow_ref.clone(),
                        command: schedule.command.clone(),
                        enabled: schedule.enabled,
                        last_run: run_state
                            .and_then(|state| state.last_run)
                            .map(|value| value.to_rfc3339()),
                    }
                })
                .collect::<Vec<_>>();

            if json {
                print_value(rows, json)
            } else {
                print_schedule_table(&rows);
                Ok(())
            }
        }
        ScheduleCommand::Fire { id } => {
            let schedule_config = load_workflow_config(Path::new(project_root))
                .with_context(|| format!("failed to load workflow config from {}", project_root))?;
            if !schedule_config
                .schedules
                .iter()
                .any(|schedule| schedule.id == id)
            {
                return Err(not_found_error(format!("schedule not found: {id}")));
            }

            fire_schedule(project_root, &id)?;
            print_ok(&format!("fired schedule: {id}"), json);
            Ok(())
        }
        ScheduleCommand::History { id } => {
            let schedule_config = load_workflow_config(Path::new(project_root))
                .with_context(|| format!("failed to load workflow config from {}", project_root))?;
            let schedule_state = load_schedule_state(Path::new(project_root))
                .with_context(|| format!("failed to load schedule state from {}", project_root))?;
            if !schedule_config
                .schedules
                .iter()
                .any(|schedule| schedule.id == id)
            {
                return Err(not_found_error(format!("schedule not found: {id}")));
            }

            let run_state = schedule_state
                .schedules
                .get(&id)
                .cloned()
                .unwrap_or_default();
            let history = ScheduleHistoryEntry {
                id,
                last_run: run_state.last_run.map(|value| value.to_rfc3339()),
                last_status: run_state.last_status,
                run_count: run_state.run_count,
            };
            print_value(history, json)
        }
    }
}

fn fire_schedule(project_root: &str, schedule_id: &str) -> Result<()> {
    use std::process::Stdio;

    let now = Utc::now();
    let status = RuntimeScheduleDispatch::fire_schedule(
        project_root,
        schedule_id,
        now,
        "manual-schedule-fire",
        |schedule_id, dispatch| {
            let mut cmd = build_runner_command_from_dispatch(dispatch, project_root);
            cmd.stdout(Stdio::null()).stderr(Stdio::piped());
            let child = cmd.spawn().with_context(|| {
                format!(
                    "failed to spawn ao-workflow-runner for schedule '{}'",
                    schedule_id
                )
            })?;
            spawn_completion_writeback(project_root, schedule_id, dispatch.clone(), child);
            Ok(())
        },
    )?;
    project_schedule_dispatch_attempt(project_root, schedule_id, now, &status);

    Ok(())
}

fn spawn_completion_writeback(
    project_root: &str,
    schedule_id: &str,
    dispatch: SubjectDispatch,
    mut child: std::process::Child,
) {
    let root = project_root.to_string();
    let sched_id = schedule_id.to_string();
    std::thread::spawn(move || {
        let mut stderr_lines = Vec::new();
        if let Some(stderr) = child.stderr.take() {
            use std::io::BufRead as _;
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("{line}");
                stderr_lines.push(line);
            }
        }

        let exit_code = child.wait().ok().and_then(|exit| exit.code());
        let events = parse_runner_events(&stderr_lines);
        let completed = CompletedProcess {
            subject_id: dispatch.subject_id().to_string(),
            task_id: dispatch.task_id().map(|value| value.to_string()),
            workflow_id: latest_runner_workflow_id(&events),
            workflow_ref: Some(dispatch.workflow_ref.clone()),
            workflow_status: latest_runner_workflow_status(&events),
            schedule_id: Some(sched_id.clone()),
            exit_code,
            success: exit_code == Some(0),
            failure_reason: None,
            events,
        };
        let plan = build_completion_reconciliation_plan(vec![completed]);
        if let Some(fact) = plan.execution_facts.first() {
            project_schedule_execution_fact(&root, fact);
        }
    });
}

fn parse_runner_events(lines: &[String]) -> Vec<RunnerEvent> {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<RunnerEvent>(line).ok())
        .collect()
}

fn latest_runner_workflow_id(events: &[RunnerEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .find_map(|event| event.workflow_id.clone())
}

fn latest_runner_workflow_status(
    events: &[RunnerEvent],
) -> Option<orchestrator_core::WorkflowStatus> {
    events.iter().rev().find_map(|event| event.workflow_status)
}

#[cfg(test)]
mod tests {
    use orchestrator_core::{builtin_workflow_config, write_workflow_config};
    use tempfile::tempdir;

    #[test]
    fn manual_fire_rejects_legacy_command_schedules_at_config_write() {
        let temp = tempdir().expect("tempdir should be created");
        let project_root = temp.path();
        let mut config = builtin_workflow_config();
        config.schedules.push(orchestrator_core::WorkflowSchedule {
            id: "nightly".to_string(),
            cron: "30 12 * * *".to_string(),
            workflow_ref: None,
            command: Some("exit 0".to_string()),
            input: None,
            enabled: true,
        });
        let error = write_workflow_config(project_root, &config)
            .expect_err("legacy command schedule should be rejected");
        assert!(
            error.to_string().contains("command is no longer supported"),
            "unexpected error: {error}"
        );
    }
}

fn print_schedule_table(rows: &[ScheduleListEntry]) {
    let headers = [
        "id",
        "cron",
        "workflow_ref",
        "command",
        "enabled",
        "last_run",
    ];
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();

    for row in rows {
        widths[0] = widths[0].max(row.id.len());
        widths[1] = widths[1].max(row.cron.len());
        widths[2] = widths[2].max(row.workflow_ref.as_deref().unwrap_or("-").len());
        widths[3] = widths[3].max(row.command.as_deref().unwrap_or("-").len());
        widths[4] = widths[4].max(row.enabled.to_string().len());
        widths[5] = widths[5].max(row.last_run.as_deref().unwrap_or("-").len());
    }

    let header = format!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}",
        headers[0],
        headers[1],
        headers[2],
        headers[3],
        headers[4],
        headers[5],
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
        w3 = widths[3],
        w4 = widths[4],
        w5 = widths[5]
    );
    println!("{}", header);
    println!("{}", "-".repeat(header.len()));

    for row in rows {
        println!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}",
            row.id,
            row.cron,
            row.workflow_ref.as_deref().unwrap_or("-"),
            row.command.as_deref().unwrap_or("-"),
            row.enabled,
            row.last_run.as_deref().unwrap_or("-"),
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
            w4 = widths[4],
            w5 = widths[5]
        );
    }
}
