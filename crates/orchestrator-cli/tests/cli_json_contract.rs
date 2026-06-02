#[path = "support/test_harness.rs"]
pub mod test_harness;

use anyhow::Result;
use protocol::CLI_SCHEMA_ID;
use serde_json::Value;
use test_harness::CliHarness;

fn assert_success_envelope(payload: &Value) {
    assert_eq!(payload.get("schema").and_then(Value::as_str), Some(CLI_SCHEMA_ID));
    assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(true));
    assert!(payload.get("data").is_some(), "success envelope should include data");
}

fn assert_error_envelope(payload: &Value, expected_code: &str, expected_exit_code: i32) {
    assert_eq!(payload.get("schema").and_then(Value::as_str), Some(CLI_SCHEMA_ID));
    assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some(expected_code));
    assert_eq!(payload.pointer("/error/exit_code").and_then(Value::as_i64), Some(i64::from(expected_exit_code)));
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
fn status_command_json_payload_includes_dashboard_schema_and_slices() -> Result<()> {
    let harness = CliHarness::new()?;

    let status = harness.run_json_ok(&["status"])?;
    assert_success_envelope(&status);
    assert_eq!(status.pointer("/data/schema").and_then(Value::as_str), Some("animus.status.v1"));
    assert!(status.pointer("/data/daemon/status").is_some(), "status payload should include daemon.status");
    assert!(status.pointer("/data/active_agents/count").is_some(), "status payload should include active_agents.count");
    assert!(status.pointer("/data/task_summary/total").is_some(), "status payload should include task_summary.total");
    assert!(
        status.pointer("/data/recent_completions/entries").is_some(),
        "status payload should include recent_completions.entries"
    );
    assert!(
        status.pointer("/data/recent_failures/entries").is_some(),
        "status payload should include recent_failures.entries"
    );
    assert!(status.pointer("/data/ci/available").is_some(), "status payload should include ci.available");

    Ok(())
}

#[test]
fn json_success_envelope_wraps_print_ok_messages() -> Result<()> {
    let harness = CliHarness::new()?;

    let cleared = harness.run_json_ok(&["daemon", "clear-logs"])?;
    assert_success_envelope(&cleared);
    assert_eq!(cleared.pointer("/data/message").and_then(Value::as_str), Some("daemon logs cleared"));

    Ok(())
}

#[test]
fn json_error_envelope_maps_conflict() -> Result<()> {
    let harness = CliHarness::new()?;
    let pack = tempfile::tempdir()?;
    std::fs::write(
        pack.path().join("pack.toml"),
        r#"schema = "animus.pack.v1"
id = "animus.conflict-test"
version = "0.1.0"
kind = "capability-pack"
title = "Conflict Test"
description = "Fixture pack to exercise the duplicate-install conflict path."

[ownership]
mode = "installed"

[compatibility]
animus_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[skills]
root = "skills"
"#,
    )?;
    std::fs::create_dir_all(pack.path().join("skills"))?;
    let path = pack.path().to_string_lossy().into_owned();

    harness.run_json_ok(&["pack", "install", "--path", &path])?;
    let (payload, status) = harness.run_json_err_with_exit(&["pack", "install", "--path", &path])?;

    assert_eq!(status, 4, "conflict should exit with code 4");
    assert_error_envelope(&payload, "conflict", 4);

    Ok(())
}

#[test]
fn json_error_envelope_maps_unavailable() -> Result<()> {
    let harness = CliHarness::new()?;

    let (payload, status) =
        harness.run_json_err_with_exit(&["agent", "control", "--run-id", "fake-run", "--action", "terminate"])?;
    assert_eq!(status, 5, "runner connection failure should exit with code 5");
    assert_error_envelope(&payload, "unavailable", 5);
    let message = payload.pointer("/error/message").and_then(Value::as_str).unwrap_or_default();
    assert!(
        message.contains("connect") || message.contains("connection") || message.contains("timed out"),
        "unavailable message should mention runner connectivity: {message}"
    );

    Ok(())
}

#[test]
fn json_error_envelope_maps_invalid_output_run_id() -> Result<()> {
    let harness = CliHarness::new()?;

    let (payload, status) = harness.run_json_err_with_exit(&["output", "jsonl", "--run-id", "../escape"])?;
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

/// Regression for the audit's Fix 3: clap argparse failures must not bypass
/// `--json`. Before this fix, `animus --json nope` exited 2 with multi-line
/// human-readable clap text on stderr, breaking every scripted consumer that
/// expects the `animus.cli.v1` envelope on stderr.
///
/// We exercise the `Command` path directly (not `CliHarness::run_json_*`)
/// because the harness's run wrappers prepend `--json --project-root <tmp>`
/// to `args`; we still want that here, but we also want to assert against
/// raw stdout/stderr and the exit code.
#[test]
fn argparse_failure_emits_json_envelope_when_json_requested() -> Result<()> {
    use std::process::Command;
    let binary = assert_cmd::cargo::cargo_bin!("animus");
    let output =
        Command::new(binary).arg("--json").arg("nope").output().expect("animus binary should execute for argparse");

    assert!(!output.status.success(), "unknown subcommand must fail");
    assert_eq!(output.status.code(), Some(2), "argparse failure must keep clap's historical exit code of 2");

    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf-8");
    let envelope_line = stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('{'))
        .unwrap_or_else(|| panic!("expected JSON envelope on stderr, got:\n{stderr}"));
    let payload: Value =
        serde_json::from_str(envelope_line).unwrap_or_else(|e| panic!("envelope must be valid JSON ({e}): {stderr}"));
    assert_error_envelope(&payload, "invalid_input", 2);
    assert_eq!(
        payload.pointer("/error/details/stage").and_then(Value::as_str),
        Some("parse"),
        "argparse failures must mark stage=parse so consumers can distinguish them from runtime errors"
    );
    let message = payload.pointer("/error/message").and_then(Value::as_str).unwrap_or_default();
    assert!(
        message.contains("nope") || message.to_ascii_lowercase().contains("unrecognized"),
        "argparse error message should mention the offending token or be recognizably parse-shaped: {message}"
    );

    Ok(())
}

/// Companion check: without `--json`, the failure path stays exactly as
/// before — clap's pretty-printed human text on stderr, no JSON envelope.
/// This pins the non-regression for terminal users.
#[test]
fn argparse_failure_keeps_human_text_when_json_not_requested() -> Result<()> {
    use std::process::Command;
    let binary = assert_cmd::cargo::cargo_bin!("animus");
    let output = Command::new(binary).arg("nope").output().expect("animus binary should execute for argparse");

    assert!(!output.status.success(), "unknown subcommand must fail");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf-8");
    assert!(
        stderr.lines().map(str::trim).find(|line| line.starts_with('{')).is_none(),
        "no JSON envelope expected when --json was not requested: {stderr}"
    );
    assert!(stderr.contains("Usage") || stderr.to_ascii_lowercase().contains("unrecognized"));

    Ok(())
}

/// `animus --json --help` is a *successful* display, not a parse failure.
/// Pre-fix this path didn't exist (clap exited 0 directly); the post-fix
/// argparse-envelope code path must not lie about success/failure here.
/// Exit 0 is mandatory; stdout has clap's help text; no JSON error envelope
/// on stderr.
#[test]
fn help_flag_keeps_exit_zero_under_json_mode() -> Result<()> {
    use std::process::Command;
    let binary = assert_cmd::cargo::cargo_bin!("animus");
    let output =
        Command::new(binary).arg("--json").arg("--help").output().expect("animus binary should execute for --help");
    assert!(output.status.success(), "`--help` must exit 0 even with --json");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf-8");
    assert!(
        stderr.lines().map(str::trim).find(|line| line.starts_with('{')).is_none(),
        "--help must not emit a JSON error envelope on stderr: {stderr}"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Usage"), "clap help text expected on stdout, got: {stdout}");
    Ok(())
}
