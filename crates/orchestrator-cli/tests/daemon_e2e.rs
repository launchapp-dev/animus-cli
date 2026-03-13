#[path = "support/test_harness.rs"]
mod test_harness;

use anyhow::Result;
use serde_json::Value;
use test_harness::CliHarness;

#[test]
fn daemon_run_once_completes_single_tick_with_no_work() -> Result<()> {
    let harness = CliHarness::new()?;

    let output = harness.run_json_output(&[
        "daemon",
        "run",
        "--once",
        "--auto-run-ready",
        "false",
        "--startup-cleanup",
        "false",
        "--reconcile-stale",
        "false",
    ])?;

    assert!(
        output.status.success(),
        "daemon run --once should exit cleanly with no work\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}

#[test]
fn daemon_run_once_sees_ready_task_but_skips_dispatch_when_auto_run_disabled() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "test task for daemon",
        "--description",
        "verify daemon tick sees but skips this task",
    ])?;
    harness.run_json_ok(&["task", "status", "--id", "TASK-001", "--status", "ready"])?;

    let output = harness.run_json_output(&[
        "daemon",
        "run",
        "--once",
        "--auto-run-ready",
        "false",
        "--startup-cleanup",
        "false",
        "--reconcile-stale",
        "false",
    ])?;

    assert!(
        output.status.success(),
        "daemon run --once should exit cleanly\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let task_payload = harness.run_json_ok(&["task", "get", "--id", "TASK-001"])?;
    let status = task_payload
        .pointer("/data/status")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert_eq!(
        status, "ready",
        "task should remain ready when auto-run is disabled"
    );

    Ok(())
}

#[test]
fn daemon_health_reports_stopped_when_no_daemon_running() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok(&["daemon", "health"])?;
    let status = payload
        .pointer("/data/status")
        .and_then(Value::as_str)
        .unwrap_or("");

    assert!(
        status == "stopped" || status == "crashed",
        "daemon health should report stopped or crashed when not running, got: {}",
        status
    );

    Ok(())
}

#[test]
fn daemon_status_reports_stopped_when_no_daemon_running() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok(&["daemon", "status"])?;
    let status = payload
        .pointer("/data")
        .and_then(Value::as_str)
        .unwrap_or("");

    assert!(
        status == "stopped" || status == "crashed",
        "daemon status should report stopped when not running, got: {}",
        status
    );

    Ok(())
}

#[test]
fn daemon_run_once_reconciles_dependency_gates() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "dependency task",
        "--description",
        "blocks the other task",
    ])?;
    harness.run_json_ok(&["task", "status", "--id", "TASK-001", "--status", "ready"])?;
    harness.run_json_ok(&["task", "status", "--id", "TASK-001", "--status", "in-progress"])?;
    harness.run_json_ok(&["task", "status", "--id", "TASK-001", "--status", "done"])?;

    harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "dependent task",
        "--description",
        "depends on TASK-001",
    ])?;
    harness.run_json_ok(&[
        "task",
        "dependency-add",
        "--id",
        "TASK-002",
        "--dependency-id",
        "TASK-001",
        "--dependency-type",
        "blocked-by",
    ])?;
    harness.run_json_ok(&["task", "status", "--id", "TASK-002", "--status", "ready"])?;

    let output = harness.run_json_output(&[
        "daemon",
        "run",
        "--once",
        "--auto-run-ready",
        "false",
        "--startup-cleanup",
        "false",
        "--reconcile-stale",
        "true",
    ])?;

    assert!(
        output.status.success(),
        "daemon run --once should exit cleanly\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let task_payload = harness.run_json_ok(&["task", "get", "--id", "TASK-002"])?;
    let status = task_payload
        .pointer("/data/status")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert_eq!(
        status, "ready",
        "dependent task should remain ready when dependency is done"
    );

    Ok(())
}

#[test]
fn daemon_events_returns_empty_when_no_events() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok(&["daemon", "events", "--limit", "10"])?;
    let events = payload
        .pointer("/data/events")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);

    assert_eq!(events, 0, "should have no daemon events initially");

    Ok(())
}

#[test]
fn daemon_run_once_with_stale_reconciliation_handles_stale_in_progress_tasks() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "stale task",
        "--description",
        "will become stale in-progress",
    ])?;
    harness.run_json_ok(&[
        "task",
        "status",
        "--id",
        "TASK-001",
        "--status",
        "in-progress",
    ])?;

    let output = harness.run_json_output(&[
        "daemon",
        "run",
        "--once",
        "--auto-run-ready",
        "false",
        "--startup-cleanup",
        "false",
        "--reconcile-stale",
        "true",
        "--stale-threshold-hours",
        "1",
    ])?;

    assert!(
        output.status.success(),
        "daemon run --once with stale reconciliation should exit cleanly\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}
