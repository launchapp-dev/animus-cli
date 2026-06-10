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
        "--skip-preflight",
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
fn daemon_events_returns_empty_when_no_events() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok(&["daemon", "events", "--limit", "10"])?;
    let events = payload.pointer("/data/events").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);

    assert_eq!(events, 0, "should have no daemon events initially");

    Ok(())
}

#[test]
fn daemon_events_exits_after_printing_when_events_exist() -> Result<()> {
    let harness = CliHarness::new()?;

    let event = serde_json::json!({
        "schema": "animus.daemon.event.v1",
        "id": "event-regression-1",
        "seq": 1,
        "timestamp": "2026-01-01T00:00:00Z",
        "event_type": "queue",
        "project_root": null,
        "data": {},
    });
    std::fs::write(harness.config_root().join("daemon-events.jsonl"), format!("{event}\n"))?;

    // Regression guard: `daemon events` used to default to follow mode and
    // never exit once an events file existed. Without `--follow` it must
    // print the batch and return promptly.
    let output =
        harness.run_json_output_within(&["daemon", "events", "--limit", "10"], std::time::Duration::from_secs(30))?;

    assert!(
        output.status.success(),
        "daemon events should exit cleanly\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("event-regression-1"), "printed events should include the stored record, got: {stdout}");

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
