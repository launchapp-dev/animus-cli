#[path = "support/test_harness.rs"]
pub mod test_harness;

use anyhow::Result;
use serde_json::Value;
use test_harness::CliHarness;

#[test]
fn workflow_candidate_validation_does_not_bootstrap_project_state() -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("absent-project");
    for (raw, valid) in [("schema: animus.workflow-config.v2\nversion: 2\n", true), ("schema: invalid", false)] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_animus"))
            .args(["--json", "--project-root"])
            .arg(&project)
            .args(["workflow", "config", "validate", "--file", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child.stdin.take().unwrap().write_all(raw.as_bytes())?;
        let output = child.wait_with_output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let payload: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(payload["data"]["valid"], valid);
        assert!(!project.exists());
    }
    let candidate = temp.path().join("candidate.json");
    std::fs::write(&candidate, r#"{"schema":"animus.workflow-config.v2","version":2}"#)?;
    let output = Command::new(env!("CARGO_BIN_EXE_animus"))
        .args(["--json", "--project-root"])
        .arg(&project)
        .args(["workflow", "config", "validate", "--file"])
        .arg(&candidate)
        .output()?;
    assert!(output.status.success());
    let payload: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(payload["data"]["valid"], true);
    assert!(!project.exists());
    Ok(())
}

#[test]
fn workflow_candidate_validation_preserves_existing_global_and_metrics_state() -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let isolated_home = temp.path().join("isolated-home");
    let global = isolated_home.join(".animus");
    std::fs::create_dir_all(project.join(".animus"))?;
    let metrics = global.join(protocol::repository_scope_for_path(&project)).join("metrics");
    std::fs::create_dir_all(&metrics)?;
    let mut config: protocol::Config = serde_json::from_value(serde_json::json!({}))?;
    config.metrics = Some(protocol::MetricsConfig {
        enabled: Some(true),
        install_id: Some("candidate-validation-test".to_string()),
        ..Default::default()
    });
    let global_bytes = serde_json::to_vec(&config)?;
    std::fs::write(global.join("config.json"), &global_bytes)?;
    let live_config = b"invalid live configuration must not be read";
    std::fs::write(project.join(".animus/config.json"), live_config)?;
    let pending_bytes = b"unparseable-pending-event-sentinel\n";
    std::fs::write(metrics.join("pending.jsonl"), pending_bytes)?;
    for json in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_animus"));
        command
            .env("HOME", &isolated_home)
            .env("ANIMUS_CONFIG_DIR", &global)
            .env("ANIMUS_AUTO_UPDATE_DISABLE", "0")
            .env("ANIMUS_AUTO_UPDATE_MODE", "notify")
            .env("ANIMUS_METRICS_DISABLE", "0")
            .arg("--project-root")
            .arg(&project)
            .args(["workflow", "config", "validate", "--file", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if json {
            command.arg("--json");
        }
        let mut child = command.spawn()?;
        child.stdin.take().unwrap().write_all(b"schema: animus.workflow-config.v2\nversion: 2\n")?;
        let output = child.wait_with_output()?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        if json {
            let payload: Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(payload["data"]["valid"], true);
        } else {
            assert!(String::from_utf8_lossy(&output.stdout).contains("valid"));
        }
        assert_eq!(std::fs::read(global.join("config.json"))?, global_bytes);
        assert_eq!(std::fs::read(project.join(".animus/config.json"))?, live_config);
        assert_eq!(std::fs::read(metrics.join("pending.jsonl"))?, pending_bytes);
        assert_eq!(std::fs::read_dir(&metrics)?.count(), 1);
        assert!(!global.join("auto-update-state.json").exists());
    }
    Ok(())
}

#[test]
fn daemon_run_once_completes_single_tick_with_no_work() -> Result<()> {
    let harness = CliHarness::new()?;

    let output = harness.run_json_output(&[
        "daemon",
        "run",
        "--once",
        "--skip-preflight",
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
    // print the batch and return promptly. The fixture record carries a
    // null project_root, so `--all-projects` is required for it to clear the
    // default current-project scope filter.
    let output = harness.run_json_output_within(
        &["daemon", "events", "--limit", "10", "--all-projects"],
        std::time::Duration::from_secs(30),
    )?;

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
fn daemon_events_scopes_to_current_project_by_default() -> Result<()> {
    let harness = CliHarness::new()?;

    let foreign = serde_json::json!({
        "schema": "animus.daemon.event.v1",
        "id": "event-foreign-1",
        "seq": 1,
        "timestamp": "2026-01-01T00:00:00Z",
        "event_type": "queue",
        "project_root": "/some/other/project",
        "data": {},
    });
    std::fs::write(harness.config_root().join("daemon-events.jsonl"), format!("{foreign}\n"))?;

    // Default scope is the current project root, so a record tagged with a
    // different project must be hidden.
    let scoped =
        harness.run_json_output_within(&["daemon", "events", "--limit", "10"], std::time::Duration::from_secs(30))?;
    assert!(scoped.status.success(), "scoped daemon events should exit cleanly");
    let scoped_stdout = String::from_utf8_lossy(&scoped.stdout);
    assert!(
        !scoped_stdout.contains("event-foreign-1"),
        "default-scoped daemon events must hide other projects, got: {scoped_stdout}"
    );

    // --all-projects opens the fleet-wide view.
    let fleet = harness.run_json_output_within(
        &["daemon", "events", "--limit", "10", "--all-projects"],
        std::time::Duration::from_secs(30),
    )?;
    assert!(fleet.status.success(), "fleet daemon events should exit cleanly");
    let fleet_stdout = String::from_utf8_lossy(&fleet.stdout);
    assert!(
        fleet_stdout.contains("event-foreign-1"),
        "--all-projects must surface other projects' events, got: {fleet_stdout}"
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
