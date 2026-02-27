#[path = "support/test_harness.rs"]
mod test_harness;

use anyhow::Result;
use serde_json::Value;
use test_harness::CliHarness;

fn assert_success_envelope(payload: &Value) {
    assert_eq!(
        payload.get("schema").and_then(Value::as_str),
        Some("ao.cli.v1")
    );
    assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        payload.get("data").is_some(),
        "success envelope should include data"
    );
}

fn assert_error_envelope(payload: &Value, expected_code: &str, expected_exit_code: i32) {
    assert_eq!(
        payload.get("schema").and_then(Value::as_str),
        Some("ao.cli.v1")
    );
    assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        payload.pointer("/error/code").and_then(Value::as_str),
        Some(expected_code)
    );
    assert_eq!(
        payload.pointer("/error/exit_code").and_then(Value::as_i64),
        Some(i64::from(expected_exit_code))
    );
    assert!(
        payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|message| !message.is_empty()),
        "error envelope should include non-empty error.message"
    );
}

#[test]
fn json_success_envelope_contract_is_stable() -> Result<()> {
    let harness = CliHarness::new()?;

    let version = harness.run_json_ok(&["version"])?;
    assert_success_envelope(&version);
    assert_eq!(
        version.pointer("/data/binary").and_then(Value::as_str),
        Some("ao")
    );

    let stats = harness.run_json_ok(&["task", "stats"])?;
    assert_success_envelope(&stats);
    assert!(
        stats.pointer("/data/total").is_some(),
        "task stats should include data.total"
    );

    Ok(())
}

#[test]
fn status_command_json_payload_includes_dashboard_schema_and_slices() -> Result<()> {
    let harness = CliHarness::new()?;

    let status = harness.run_json_ok(&["status"])?;
    assert_success_envelope(&status);
    assert_eq!(
        status.pointer("/data/schema").and_then(Value::as_str),
        Some("ao.status.v1")
    );
    assert!(
        status.pointer("/data/daemon/status").is_some(),
        "status payload should include daemon.status"
    );
    assert!(
        status.pointer("/data/active_agents/count").is_some(),
        "status payload should include active_agents.count"
    );
    assert!(
        status.pointer("/data/task_summary/total").is_some(),
        "status payload should include task_summary.total"
    );
    assert!(
        status.pointer("/data/recent_completions/entries").is_some(),
        "status payload should include recent_completions.entries"
    );
    assert!(
        status.pointer("/data/recent_failures/entries").is_some(),
        "status payload should include recent_failures.entries"
    );
    assert!(
        status.pointer("/data/ci/available").is_some(),
        "status payload should include ci.available"
    );

    Ok(())
}

#[test]
fn json_success_envelope_wraps_print_ok_messages() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&[
        "architecture",
        "entity",
        "create",
        "--id",
        "api",
        "--name",
        "API",
    ])?;

    let deleted = harness.run_json_ok(&["architecture", "entity", "delete", "--id", "api"])?;
    assert_success_envelope(&deleted);
    assert_eq!(
        deleted.pointer("/data/message").and_then(Value::as_str),
        Some("architecture entity deleted")
    );

    Ok(())
}

#[test]
fn json_error_envelope_maps_invalid_input_and_not_found() -> Result<()> {
    let harness = CliHarness::new()?;

    let (invalid_payload, invalid_status) = harness
        .run_json_err_with_exit(&["task", "status", "--id", "TASK-404", "--status", "invalid"])?;
    assert_eq!(invalid_status, 2, "invalid input should exit with code 2");
    assert_error_envelope(&invalid_payload, "invalid_input", 2);

    let (missing_payload, missing_status) =
        harness.run_json_err_with_exit(&["task", "get", "--id", "TASK-404"])?;
    assert_eq!(missing_status, 3, "not found should exit with code 3");
    assert_error_envelope(&missing_payload, "not_found", 3);

    Ok(())
}

#[test]
fn json_error_envelope_maps_conflict() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&[
        "architecture",
        "entity",
        "create",
        "--id",
        "api",
        "--name",
        "API",
    ])?;

    let (payload, status) = harness.run_json_err_with_exit(&[
        "architecture",
        "entity",
        "create",
        "--id",
        "api",
        "--name",
        "Duplicate API",
    ])?;

    assert_eq!(status, 4, "conflict should exit with code 4");
    assert_error_envelope(&payload, "conflict", 4);

    Ok(())
}

#[test]
fn json_error_envelope_maps_unavailable() -> Result<()> {
    let harness = CliHarness::new()?;

    let (payload, status) = harness.run_json_err_with_exit(&["agent", "runner-status"])?;
    assert_eq!(
        status, 5,
        "runner connection failure should exit with code 5"
    );
    assert_error_envelope(&payload, "unavailable", 5);
    let message = payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains("connect")
            || message.contains("connection")
            || message.contains("timed out"),
        "unavailable message should mention runner connectivity: {message}"
    );

    Ok(())
}

#[test]
fn json_error_envelope_maps_invalid_output_run_id() -> Result<()> {
    let harness = CliHarness::new()?;

    let (payload, status) =
        harness.run_json_err_with_exit(&["output", "jsonl", "--run-id", "../escape"])?;
    assert_eq!(status, 2, "invalid run_id should exit with code 2");
    assert_error_envelope(&payload, "invalid_input", 2);
    assert!(
        payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("invalid run_id")),
        "invalid run_id message should be preserved in error envelope"
    );

    Ok(())
}
