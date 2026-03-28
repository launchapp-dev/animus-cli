#[path = "support/test_harness.rs"]
pub mod test_harness;

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
    let status = task_payload.pointer("/data/status").and_then(Value::as_str).unwrap_or("");
    assert_eq!(status, "ready", "task should remain ready when auto-run is disabled");

    Ok(())
}

#[test]
fn daemon_health_reports_stopped_when_no_daemon_running() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok(&["daemon", "health"])?;
    let status = payload.pointer("/data/status").and_then(Value::as_str).unwrap_or("");

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
    let status = payload.pointer("/data").and_then(Value::as_str).unwrap_or("");

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

    harness.run_json_ok(&["task", "create", "--title", "dependency task", "--description", "blocks the other task"])?;
    harness.run_json_ok(&["task", "status", "--id", "TASK-001", "--status", "ready"])?;
    harness.run_json_ok(&["task", "status", "--id", "TASK-001", "--status", "in-progress"])?;
    harness.run_json_ok(&["task", "status", "--id", "TASK-001", "--status", "done"])?;

    harness.run_json_ok(&["task", "create", "--title", "dependent task", "--description", "depends on TASK-001"])?;
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
    let status = task_payload.pointer("/data/status").and_then(Value::as_str).unwrap_or("");
    assert_eq!(status, "ready", "dependent task should remain ready when dependency is done");

    Ok(())
}

#[test]
fn daemon_events_returns_empty_when_no_events() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok(&["daemon", "events", "--limit", "10"])?;
    let events = payload.pointer("/data/events").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);

    assert_eq!(events, 0, "should have no daemon events initially");

    Ok(())
}

#[test]
fn queue_enqueue_dispatch_round_trip() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "queue dispatch task",
        "--description",
        "verify queue-driven dispatch lifecycle",
    ])?;
    harness.run_json_ok(&["task", "status", "--id", "TASK-001", "--status", "ready"])?;

    harness.run_json_ok(&["queue", "enqueue", "--task-id", "TASK-001"])?;

    let queue_payload = harness.run_json_ok(&["queue", "list"])?;
    let entries =
        queue_payload.pointer("/data/entries").and_then(Value::as_array).expect("queue list should have entries array");
    assert_eq!(entries.len(), 1, "queue should have exactly one entry");
    let entry_status = entries[0].get("status").and_then(Value::as_str).unwrap_or("");
    assert_eq!(entry_status, "pending", "enqueued entry should be pending before daemon tick");
    let entry_task_id = entries[0].get("task_id").and_then(Value::as_str).unwrap_or("");
    assert_eq!(entry_task_id, "TASK-001", "enqueued entry should reference the correct task");

    let output = harness.run_json_output(&[
        "daemon",
        "run",
        "--once",
        "--auto-run-ready",
        "true",
        "--startup-cleanup",
        "false",
        "--reconcile-stale",
        "false",
    ])?;
    assert!(
        output.status.success(),
        "daemon run --once should exit cleanly\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let queue_after = harness.run_json_ok(&["queue", "list"])?;
    let entries_after = queue_after
        .pointer("/data/entries")
        .and_then(Value::as_array)
        .expect("queue list should have entries array after daemon tick");
    assert!(!entries_after.is_empty(), "queue entry should still exist after daemon tick");
    let post_status = entries_after[0].get("status").and_then(Value::as_str).unwrap_or("");
    assert!(
        post_status == "assigned" || post_status == "pending",
        "queue entry should be assigned or remain pending (if runner binary unavailable), got: {}",
        post_status
    );

    Ok(())
}

#[test]
fn workflow_config_validate_passes() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok(&["workflow", "config", "validate"])?;
    let ok = payload.get("ok").and_then(Value::as_bool).unwrap_or(false);
    assert!(ok, "workflow config validate should pass");

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
    harness.run_json_ok(&["task", "status", "--id", "TASK-001", "--status", "in-progress"])?;

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
