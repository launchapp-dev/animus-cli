#[path = "support/test_harness.rs"]
pub mod test_harness;

use anyhow::{Context, Result};
use fs2::FileExt;
use serde_json::Value;
use std::fs::OpenOptions;
use std::process::Command;
use test_harness::CliHarness;

const SHARED_DESTRUCTIVE_DRY_RUN_KEYS: [&str; 8] = [
    "operation",
    "target",
    "action",
    "destructive",
    "dry_run",
    "requires_confirmation",
    "planned_effects",
    "next_step",
];

fn assert_shared_destructive_dry_run_contract(
    payload: &Value,
    expected_operation: &str,
    expected_requires_confirmation: bool,
) {
    let data = payload.pointer("/data").expect("envelope should include /data payload");

    for key in SHARED_DESTRUCTIVE_DRY_RUN_KEYS {
        assert!(data.get(key).is_some(), "dry-run payload should include shared key '{}'", key);
    }

    assert_eq!(data.get("operation").and_then(Value::as_str), Some(expected_operation));
    assert_eq!(data.get("action").and_then(Value::as_str), Some(expected_operation));
    assert_eq!(data.get("dry_run").and_then(Value::as_bool), Some(true));
    assert_eq!(data.get("requires_confirmation").and_then(Value::as_bool), Some(expected_requires_confirmation));
    assert!(data.get("target").and_then(Value::as_object).is_some(), "dry-run payload target should be a JSON object");
    assert!(
        data.get("planned_effects").and_then(Value::as_array).map(|effects| !effects.is_empty()).unwrap_or(false),
        "dry-run payload planned_effects should be a non-empty array"
    );
    assert!(data.get("next_step").and_then(Value::as_str).is_some(), "dry-run payload next_step should be a string");
}

#[test]
fn e2e_daemon_autonomous_start_idempotent_then_stop() -> Result<()> {
    let harness = CliHarness::new()?;

    let started = harness.run_json_ok(&[
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
    let daemon_pid = started
        .pointer("/data/daemon_pid")
        .and_then(Value::as_u64)
        .context("daemon start --autonomous should return data.daemon_pid")?;
    assert!(daemon_pid > 0, "daemon pid should be > 0");

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
        "second autonomous start should report the same running daemon pid"
    );

    harness.run_json_ok(&["daemon", "stop"])?;
    Ok(())
}

#[test]
fn e2e_daemon_autonomous_start_reports_early_exit_failure() -> Result<()> {
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
    ])?;
    assert_ne!(exit_code, 0, "daemon start should fail when autonomous child exits");
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

#[test]
fn e2e_git_worktree_remove_requires_confirmation_and_supports_dry_run() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&["git", "repo", "init", "--name", "demo"])?;
    let repo = harness.run_json_ok(&["git", "repo", "get", "--repo", "demo"])?;
    let repo_path =
        repo.pointer("/data/path").and_then(Value::as_str).context("git repo get should return data.path")?;

    let seed_file = std::path::Path::new(repo_path).join("README.md");
    std::fs::write(&seed_file, "seed\n").context("failed to seed git repo")?;

    let git_add = Command::new("git").args(["-C", repo_path, "add", "."]).output().context("failed to run git add")?;
    assert!(git_add.status.success(), "git add failed: {}", String::from_utf8_lossy(&git_add.stderr));

    let git_commit = Command::new("git")
        .args([
            "-C",
            repo_path,
            "-c",
            "user.name=Animus Test",
            "-c",
            "user.email=animus@example.com",
            "commit",
            "-m",
            "seed",
        ])
        .output()
        .context("failed to run git commit")?;
    assert!(git_commit.status.success(), "git commit failed: {}", String::from_utf8_lossy(&git_commit.stderr));

    let worktree_name = "wt-preview";
    let worktree_path = harness.project_root().join(worktree_name);
    let worktree_path_string = worktree_path.to_string_lossy().to_string();
    harness.run_json_ok(&[
        "git",
        "worktree",
        "create",
        "--repo",
        "demo",
        "--worktree-name",
        worktree_name,
        "--worktree-path",
        &worktree_path_string,
        "--branch",
        worktree_name,
        "--create-branch",
    ])?;

    let remove_error =
        harness.run_json_err(&["git", "worktree", "remove", "--repo", "demo", "--worktree-name", worktree_name])?;
    let remove_confirmation_message =
        remove_error.pointer("/error/message").and_then(Value::as_str).unwrap_or_default();
    assert_eq!(
        remove_confirmation_message,
        "CONFIRMATION_REQUIRED: request and approve a git confirmation for 'remove_worktree' on 'demo', then rerun with --confirmation-id <id>; use --dry-run to preview changes",
        "git worktree remove confirmation message should use canonical token order"
    );

    let remove_preview = harness.run_json_ok(&[
        "git",
        "worktree",
        "remove",
        "--repo",
        "demo",
        "--worktree-name",
        worktree_name,
        "--dry-run",
    ])?;
    assert_shared_destructive_dry_run_contract(&remove_preview, "git.worktree.remove", true);
    assert_eq!(
        remove_preview.pointer("/data/next_step").and_then(Value::as_str),
        Some(
            "request and approve a git confirmation for 'remove_worktree' on 'demo', then rerun with --confirmation-id <id>"
        )
    );

    let push_preview = harness.run_json_ok(&["git", "push", "--repo", "demo", "--force", "--dry-run"])?;
    assert_shared_destructive_dry_run_contract(&push_preview, "git.push", true);
    assert_eq!(
        push_preview.pointer("/data/next_step").and_then(Value::as_str),
        Some(
            "request and approve a git confirmation for 'force_push' on 'demo', then rerun with --confirmation-id <id>"
        )
    );

    assert!(worktree_path.exists(), "dry-run should not remove worktree path");

    Ok(())
}

#[test]
fn e2e_git_repo_init_failure_is_reported_and_not_registered() -> Result<()> {
    let harness = CliHarness::new()?;
    let occupied_path = harness.project_root().join("occupied-path");
    std::fs::write(&occupied_path, "blocking file\n").context("failed to create occupied path file")?;
    let occupied_path_string = occupied_path.to_string_lossy().to_string();

    let failed_init =
        harness.run_json_err(&["git", "repo", "init", "--name", "broken", "--path", &occupied_path_string])?;
    let error_message = failed_init.pointer("/error/message").and_then(Value::as_str).unwrap_or_default();
    assert!(error_message.contains("git init failed"), "expected git init failure message, got: {error_message}");

    let listed = harness.run_json_ok(&["git", "repo", "list"])?;
    let repos = listed.pointer("/data").and_then(Value::as_array).context("git repo list should return data array")?;
    assert!(
        !repos
            .iter()
            .any(|repo| { repo.get("name").and_then(Value::as_str).map(|name| name == "broken").unwrap_or(false) }),
        "failed git init should not register repo entry"
    );

    Ok(())
}
