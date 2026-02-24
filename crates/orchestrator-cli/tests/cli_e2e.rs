#[path = "support/test_harness.rs"]
mod test_harness;

use anyhow::{Context, Result};
use serde_json::Value;
use test_harness::CliHarness;

#[test]
fn e2e_task_lifecycle_round_trip() -> Result<()> {
    let harness = CliHarness::new()?;

    let created = harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "E2E Task",
        "--description",
        "Created by e2e test",
    ])?;
    let task_id = created
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("task create should return data.id")?
        .to_string();
    assert_eq!(
        created.pointer("/data/title").and_then(Value::as_str),
        Some("E2E Task")
    );
    assert_eq!(
        created.pointer("/data/status").and_then(Value::as_str),
        Some("backlog")
    );

    harness.run_json_ok(&["task", "status", "--id", &task_id, "--status", "ready"])?;

    let fetched = harness.run_json_ok(&["task", "get", "--id", &task_id])?;
    assert_eq!(
        fetched.pointer("/data/id").and_then(Value::as_str),
        Some(task_id.as_str())
    );
    assert_eq!(
        fetched.pointer("/data/status").and_then(Value::as_str),
        Some("ready")
    );

    let stats = harness.run_json_ok(&["task", "stats"])?;
    assert_eq!(
        stats.pointer("/data/total").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        stats
            .pointer("/data/by_status/ready")
            .and_then(Value::as_u64),
        Some(1)
    );

    Ok(())
}

#[test]
fn e2e_requirements_create_update_and_list() -> Result<()> {
    let harness = CliHarness::new()?;

    let created = harness.run_json_ok(&[
        "requirements",
        "create",
        "--title",
        "E2E Requirement",
        "--description",
        "Requirement from integration test",
        "--acceptance-criterion",
        "criterion one",
    ])?;
    let requirement_id = created
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("requirements create should return data.id")?
        .to_string();
    assert_eq!(
        created.pointer("/data/status").and_then(Value::as_str),
        Some("draft")
    );

    harness.run_json_ok(&[
        "requirements",
        "update",
        "--id",
        &requirement_id,
        "--status",
        "done",
        "--acceptance-criterion",
        "criterion two",
    ])?;

    let listed = harness.run_json_ok(&["requirements", "list"])?;
    let requirements = listed
        .pointer("/data")
        .and_then(Value::as_array)
        .context("requirements list should return data as array")?;
    let requirement = requirements
        .iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(requirement_id.as_str()))
        .context("updated requirement should be present in list")?;

    assert_eq!(
        requirement.get("status").and_then(Value::as_str),
        Some("done")
    );
    let acceptance_criteria = requirement
        .get("acceptance_criteria")
        .and_then(Value::as_array)
        .context("requirement should include acceptance_criteria")?;
    assert!(
        acceptance_criteria
            .iter()
            .any(|value| value.as_str() == Some("criterion one")),
        "first criterion should be retained"
    );
    assert!(
        acceptance_criteria
            .iter()
            .any(|value| value.as_str() == Some("criterion two")),
        "second criterion should be appended"
    );

    let requirements_dir = harness.project_root().join(".ao/requirements/generated");
    assert!(
        requirements_dir.exists(),
        "requirements generated directory should exist"
    );

    Ok(())
}

#[test]
fn e2e_daemon_autonomous_start_idempotent_then_stop() -> Result<()> {
    let harness = CliHarness::new()?;

    let started = harness.run_json_ok(&[
        "daemon",
        "start",
        "--autonomous",
        "--interval-secs",
        "1",
        "--include-registry",
        "false",
        "--auto-run-ready",
        "false",
        "--startup-cleanup",
        "false",
        "--resume-interrupted",
        "false",
        "--reconcile-stale",
        "false",
        "--max-tasks-per-tick",
        "1",
    ])?;
    let daemon_pid = started
        .pointer("/data/daemon_pid")
        .and_then(Value::as_u64)
        .context("daemon start --autonomous should return data.daemon_pid")?;
    assert!(daemon_pid > 0, "daemon pid should be > 0");

    let already_running = harness.run_json_ok(&[
        "daemon",
        "start",
        "--autonomous",
        "--interval-secs",
        "1",
        "--include-registry",
        "false",
        "--auto-run-ready",
        "false",
        "--startup-cleanup",
        "false",
        "--resume-interrupted",
        "false",
        "--reconcile-stale",
        "false",
        "--max-tasks-per-tick",
        "1",
    ])?;
    assert_eq!(
        already_running
            .pointer("/data/daemon_pid")
            .and_then(Value::as_u64),
        Some(daemon_pid),
        "second autonomous start should report the same running daemon pid"
    );

    harness.run_json_ok(&["daemon", "stop"])?;
    Ok(())
}
