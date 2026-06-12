#[path = "support/test_harness.rs"]
pub mod test_harness;

use anyhow::{Context, Result};
use fs2::FileExt;
use serde_json::Value;
use std::fs::OpenOptions;
use test_harness::CliHarness;

#[test]
fn e2e_daemon_start_detaches_by_default_idempotent_then_stop() -> Result<()> {
    let harness = CliHarness::new()?;

    // `daemon start` without any detach flag must spawn a detached
    // background daemon (v0.6: detached is the default and only behavior).
    let started = harness.run_json_ok(&[
        "daemon",
        "start",
        "--skip-runner",
        "--interval-secs",
        "1",
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
        "--skip-preflight",
    ])?;
    let daemon_pid = started
        .pointer("/data/daemon_pid")
        .and_then(Value::as_u64)
        .context("daemon start should return data.daemon_pid")?;
    assert!(daemon_pid > 0, "daemon pid should be > 0");
    assert_eq!(
        started.pointer("/data/detached").and_then(Value::as_bool),
        Some(true),
        "daemon start should report detached mode"
    );
    assert!(
        started.pointer("/data/log_path").and_then(Value::as_str).is_some_and(|path| !path.is_empty()),
        "daemon start should report the background log path"
    );

    // The deprecated `--autonomous` flag stays accepted as a hidden no-op
    // and behaves identically (idempotent against the running daemon).
    let already_running = harness.run_json_ok(&[
        "daemon",
        "start",
        "--autonomous",
        "--skip-runner",
        "--interval-secs",
        "1",
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
        "--skip-preflight",
    ])?;
    assert_eq!(
        already_running.pointer("/data/daemon_pid").and_then(Value::as_u64),
        Some(daemon_pid),
        "second start (with deprecated --autonomous) should report the same running daemon pid"
    );

    harness.run_json_ok(&["daemon", "stop"])?;
    Ok(())
}

#[test]
fn e2e_daemon_start_reports_early_exit_failure() -> Result<()> {
    let harness = CliHarness::new()?;

    let lock_path = harness.scoped_root().join("daemon").join("daemon.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).context("daemon lock parent should be created")?;
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .context("daemon lock should be opened")?;
    lock_file.try_lock_exclusive().context("daemon lock should be acquired in test")?;

    let (failure, exit_code) = harness.run_json_err_with_exit(&[
        "daemon",
        "start",
        "--skip-runner",
        "--interval-secs",
        "1",
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
    assert_ne!(exit_code, 0, "daemon start should fail when the detached child exits");
    let message = failure
        .pointer("/error/message")
        .and_then(Value::as_str)
        .context("daemon start error should include /error/message")?;
    assert!(
        message.contains("autonomous daemon failed startup validation"),
        "daemon start failure should indicate startup validation failure"
    );
    assert!(message.contains("startup log path"), "daemon start failure should include startup log path diagnostics");
    assert!(message.contains("startup log tail"), "daemon start failure should include startup log tail diagnostics");

    drop(lock_file);
    Ok(())
}

#[test]
fn e2e_daemon_preflight_subcommand_reports_missing_plugins() -> Result<()> {
    let harness = CliHarness::new()?;

    // Preflight must exit non-zero (code 2 = invalid_input) when any
    // required role is missing, so CI scripts and `&&` chains can
    // detect the failure. The inner JSON error body still carries the
    // actionable `plugin install` hint that operators rely on.
    let (failure, exit_code) = harness.run_json_err_with_exit(&["daemon", "preflight"])?;
    assert_eq!(exit_code, 2, "preflight should exit 2 (invalid_input) when required roles missing");
    let message = failure
        .pointer("/error/message")
        .and_then(Value::as_str)
        .context("preflight error envelope should include /error/message")?;
    assert!(
        message.contains("plugin preflight failed"),
        "preflight error message should describe the gap; got: {message}"
    );
    assert!(
        message.contains("animus plugin install"),
        "preflight error message should surface the actionable install command; got: {message}"
    );
    // The structured preflight payload (schema / missing / fix_message)
    // must survive into /error/details so machine consumers can still
    // iterate the missing-roles list without re-parsing the textual
    // message. Without this the JSON-mode error path is strictly
    // poorer than the legacy `"ok": true` success-with-failure shape
    // it replaces.
    assert_eq!(
        failure.pointer("/error/details/schema").and_then(Value::as_str),
        Some("animus.daemon.preflight.v1"),
        "preflight error details should carry the structured schema"
    );
    let missing = failure
        .pointer("/error/details/missing")
        .and_then(Value::as_array)
        .context("preflight error details should expose the missing-roles array")?;
    assert!(!missing.is_empty(), "missing-roles array should be non-empty in fresh project");
    Ok(())
}

#[test]
fn e2e_daemon_start_without_preflight_refuses_when_no_plugins() -> Result<()> {
    let harness = CliHarness::new()?;

    let (failure, exit_code) = harness.run_json_err_with_exit(&[
        "daemon",
        "start",
        "--skip-runner",
        "--interval-secs",
        "1",
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
    assert_ne!(exit_code, 0, "daemon start should fail without --skip-preflight when no plugins installed");
    let message = failure
        .pointer("/error/message")
        .and_then(Value::as_str)
        .context("daemon start error should include /error/message")?;
    assert!(
        message.contains("plugin preflight failed") || message.contains("at_least_one_provider"),
        "daemon start failure should mention preflight result; got: {message}"
    );
    Ok(())
}

#[test]
fn e2e_daemon_config_rejects_removed_auto_prune_flag() -> Result<()> {
    let harness = CliHarness::new()?;

    // The daemon git/merge policy flags were removed in v0.5.x; the CLI
    // should reject them as unknown arguments.
    let output = harness.run_json_output(&["daemon", "config", "--auto-prune-worktrees-after-merge", "true"])?;
    assert!(!output.status.success(), "removed daemon config flag should be rejected");

    Ok(())
}

#[test]
fn e2e_daemon_config_persists_pool_size() -> Result<()> {
    let harness = CliHarness::new()?;

    let configured = harness.run_json_ok(&["daemon", "config", "--pool-size", "8"])?;
    assert_eq!(configured.pointer("/data/pool_size").and_then(Value::as_u64), Some(8));
    assert!(configured.pointer("/data/updated").and_then(Value::as_bool).unwrap_or(false));

    // Verify persisted in pm-config.json
    let pm_config_path = harness.scoped_root().join("daemon").join("pm-config.json");
    let pm_config: Value =
        serde_json::from_str(&std::fs::read_to_string(&pm_config_path).context("pm-config readable")?)?;
    assert_eq!(pm_config.get("pool_size").and_then(Value::as_u64), Some(8));

    Ok(())
}

#[test]
fn e2e_daemon_config_persists_interval_secs() -> Result<()> {
    let harness = CliHarness::new()?;

    let configured = harness.run_json_ok(&["daemon", "config", "--interval-secs", "15"])?;
    assert_eq!(configured.pointer("/data/interval_secs").and_then(Value::as_u64), Some(15));

    let pm_config_path = harness.scoped_root().join("daemon").join("pm-config.json");
    let pm_config: Value =
        serde_json::from_str(&std::fs::read_to_string(&pm_config_path).context("pm-config readable")?)?;
    assert_eq!(pm_config.get("interval_secs").and_then(Value::as_u64), Some(15));

    Ok(())
}

#[test]
fn e2e_daemon_config_persists_max_tasks_per_tick() -> Result<()> {
    let harness = CliHarness::new()?;

    let configured = harness.run_json_ok(&["daemon", "config", "--max-tasks-per-tick", "10"])?;
    assert_eq!(configured.pointer("/data/max_tasks_per_tick").and_then(Value::as_u64), Some(10));

    let pm_config_path = harness.scoped_root().join("daemon").join("pm-config.json");
    let pm_config: Value =
        serde_json::from_str(&std::fs::read_to_string(&pm_config_path).context("pm-config readable")?)?;
    assert_eq!(pm_config.get("max_tasks_per_tick").and_then(Value::as_u64), Some(10));

    Ok(())
}

#[test]
fn e2e_daemon_config_persists_auto_run_ready() -> Result<()> {
    let harness = CliHarness::new()?;

    let configured = harness.run_json_ok(&["daemon", "config", "--auto-run-ready", "false"])?;
    assert_eq!(configured.pointer("/data/auto_run_ready").and_then(Value::as_bool), Some(false));

    let pm_config_path = harness.scoped_root().join("daemon").join("pm-config.json");
    let pm_config: Value =
        serde_json::from_str(&std::fs::read_to_string(&pm_config_path).context("pm-config readable")?)?;
    assert_eq!(pm_config.get("auto_run_ready").and_then(Value::as_bool), Some(false));

    Ok(())
}

#[test]
fn e2e_daemon_config_shows_runtime_settings() -> Result<()> {
    let harness = CliHarness::new()?;

    // Set multiple settings then read back
    harness.run_json_ok(&["daemon", "config", "--pool-size", "4", "--interval-secs", "20"])?;
    let result = harness.run_json_ok(&["daemon", "config"])?;
    assert_eq!(result.pointer("/data/pool_size").and_then(Value::as_u64), Some(4));
    assert_eq!(result.pointer("/data/interval_secs").and_then(Value::as_u64), Some(20));
    // auto_run_ready should show default true when not explicitly set
    assert_eq!(result.pointer("/data/auto_run_ready").and_then(Value::as_bool), Some(true));

    Ok(())
}

#[test]
fn e2e_daemon_config_multiple_runtime_settings_at_once() -> Result<()> {
    let harness = CliHarness::new()?;

    let configured = harness.run_json_ok(&[
        "daemon",
        "config",
        "--pool-size",
        "6",
        "--interval-secs",
        "12",
        "--max-tasks-per-tick",
        "8",
        "--stale-threshold-hours",
        "48",
        "--phase-timeout-secs",
        "300",
    ])?;

    assert_eq!(configured.pointer("/data/pool_size").and_then(Value::as_u64), Some(6));
    assert_eq!(configured.pointer("/data/interval_secs").and_then(Value::as_u64), Some(12));
    assert_eq!(configured.pointer("/data/max_tasks_per_tick").and_then(Value::as_u64), Some(8));
    assert_eq!(configured.pointer("/data/stale_threshold_hours").and_then(Value::as_u64), Some(48));
    assert_eq!(configured.pointer("/data/phase_timeout_secs").and_then(Value::as_u64), Some(300));

    Ok(())
}
