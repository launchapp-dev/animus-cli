#[path = "support/test_harness.rs"]
mod test_harness;

use anyhow::{Context, Result};
use serde_json::Value;
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
    let data = payload
        .pointer("/data")
        .expect("envelope should include /data payload");

    for key in SHARED_DESTRUCTIVE_DRY_RUN_KEYS {
        assert!(
            data.get(key).is_some(),
            "dry-run payload should include shared key '{}'",
            key
        );
    }

    assert_eq!(
        data.get("operation").and_then(Value::as_str),
        Some(expected_operation)
    );
    assert_eq!(
        data.get("action").and_then(Value::as_str),
        Some(expected_operation)
    );
    assert_eq!(data.get("dry_run").and_then(Value::as_bool), Some(true));
    assert_eq!(
        data.get("requires_confirmation").and_then(Value::as_bool),
        Some(expected_requires_confirmation)
    );
    assert!(
        data.get("target").and_then(Value::as_object).is_some(),
        "dry-run payload target should be a JSON object"
    );
    assert!(
        data.get("planned_effects")
            .and_then(Value::as_array)
            .map(|effects| !effects.is_empty())
            .unwrap_or(false),
        "dry-run payload planned_effects should be a non-empty array"
    );
    assert!(
        data.get("next_step").and_then(Value::as_str).is_some(),
        "dry-run payload next_step should be a string"
    );
}

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
    assert_eq!(
        stats
            .pointer("/data/stale_in_progress/count")
            .and_then(Value::as_u64),
        Some(0)
    );

    Ok(())
}

#[test]
fn e2e_task_create_warns_for_unlinked_non_chore_in_json_mode() -> Result<()> {
    let harness = CliHarness::new()?;
    let output = harness.run_json_output(&[
        "task",
        "create",
        "--title",
        "Warn on missing requirement",
        "--description",
        "integration test",
    ])?;

    let stdout = String::from_utf8(output.stdout).context("stdout should be utf-8")?;
    let stderr = String::from_utf8(output.stderr).context("stderr should be utf-8")?;
    assert!(
        output.status.success(),
        "task create should succeed; stdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );

    let payload: Value =
        serde_json::from_str(&stdout).context("stdout should remain valid JSON envelope")?;
    assert_eq!(
        payload.get("schema").and_then(Value::as_str),
        Some("ao.cli.v1")
    );
    assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        payload
            .pointer("/data/id")
            .and_then(Value::as_str)
            .is_some(),
        "task create should still return a task id"
    );
    assert!(
        stderr.contains("creating non-chore task without linked requirements"),
        "stderr should include missing linked requirement warning"
    );
    assert!(
        stderr.contains("--linked-requirement"),
        "warning should include actionable flag guidance"
    );

    Ok(())
}

#[test]
fn e2e_task_create_does_not_warn_for_unlinked_chore() -> Result<()> {
    let harness = CliHarness::new()?;
    let output = harness.run_json_output(&[
        "task",
        "create",
        "--title",
        "Chore without linkage",
        "--description",
        "integration test",
        "--task-type",
        "chore",
    ])?;

    let stdout = String::from_utf8(output.stdout).context("stdout should be utf-8")?;
    let stderr = String::from_utf8(output.stderr).context("stderr should be utf-8")?;
    assert!(
        output.status.success(),
        "task create should succeed; stdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );
    assert!(
        !stderr.contains("creating non-chore task without linked requirements"),
        "chore tasks should not emit missing linked requirement warning"
    );

    Ok(())
}

#[test]
fn e2e_task_create_does_not_warn_when_linked_requirement_is_provided() -> Result<()> {
    let harness = CliHarness::new()?;
    let output = harness.run_json_output(&[
        "task",
        "create",
        "--title",
        "Linked task",
        "--description",
        "integration test",
        "--linked-requirement",
        "REQ-123",
    ])?;

    let stdout = String::from_utf8(output.stdout).context("stdout should be utf-8")?;
    let stderr = String::from_utf8(output.stderr).context("stderr should be utf-8")?;
    assert!(
        output.status.success(),
        "task create should succeed; stdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );

    let payload: Value =
        serde_json::from_str(&stdout).context("stdout should remain valid JSON envelope")?;
    assert_eq!(
        payload
            .pointer("/data/linked_requirements/0")
            .and_then(Value::as_str),
        Some("REQ-123")
    );
    assert!(
        !stderr.contains("creating non-chore task without linked requirements"),
        "linked tasks should not emit missing linked requirement warning"
    );

    Ok(())
}

#[test]
fn e2e_task_create_warns_when_linked_requirements_are_blank() -> Result<()> {
    let harness = CliHarness::new()?;
    let output = harness.run_json_output(&[
        "task",
        "create",
        "--title",
        "Blank linked requirements",
        "--description",
        "integration test",
        "--linked-requirement",
        "",
        "--linked-requirement",
        "   ",
    ])?;

    let stdout = String::from_utf8(output.stdout).context("stdout should be utf-8")?;
    let stderr = String::from_utf8(output.stderr).context("stderr should be utf-8")?;
    assert!(
        output.status.success(),
        "task create should succeed; stdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );

    let payload: Value =
        serde_json::from_str(&stdout).context("stdout should remain valid JSON envelope")?;
    assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        stderr.contains("creating non-chore task without linked requirements"),
        "blank linked requirements should still emit warning"
    );

    Ok(())
}

#[test]
fn e2e_task_create_does_not_warn_when_any_linked_requirement_is_non_blank() -> Result<()> {
    let harness = CliHarness::new()?;
    let output = harness.run_json_output(&[
        "task",
        "create",
        "--title",
        "Mixed linked requirements",
        "--description",
        "integration test",
        "--linked-requirement",
        "",
        "--linked-requirement",
        "REQ-123",
    ])?;

    let stdout = String::from_utf8(output.stdout).context("stdout should be utf-8")?;
    let stderr = String::from_utf8(output.stderr).context("stderr should be utf-8")?;
    assert!(
        output.status.success(),
        "task create should succeed; stdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );

    let payload: Value =
        serde_json::from_str(&stdout).context("stdout should remain valid JSON envelope")?;
    assert_eq!(
        payload
            .pointer("/data/linked_requirements/1")
            .and_then(Value::as_str),
        Some("REQ-123")
    );
    assert!(
        !stderr.contains("creating non-chore task without linked requirements"),
        "warning should not be emitted when at least one linked requirement is non-blank"
    );

    Ok(())
}

#[test]
fn e2e_task_create_warning_uses_resolved_input_json_payload() -> Result<()> {
    let harness = CliHarness::new()?;
    let input_json = r#"{"title":"input-json task","description":"integration test","task_type":"feature","linked_requirements":[]}"#;
    let output = harness.run_json_output(&[
        "task",
        "create",
        "--title",
        "cli title ignored",
        "--task-type",
        "chore",
        "--linked-requirement",
        "REQ-123",
        "--input-json",
        input_json,
    ])?;

    let stdout = String::from_utf8(output.stdout).context("stdout should be utf-8")?;
    let stderr = String::from_utf8(output.stderr).context("stderr should be utf-8")?;
    assert!(
        output.status.success(),
        "task create should succeed; stdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );

    let payload: Value =
        serde_json::from_str(&stdout).context("stdout should remain valid JSON envelope")?;
    assert_eq!(
        payload.pointer("/data/title").and_then(Value::as_str),
        Some("input-json task")
    );
    assert_eq!(
        payload.pointer("/data/type").and_then(Value::as_str),
        Some("feature")
    );
    assert_eq!(
        payload
            .pointer("/data/linked_requirements")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    assert!(
        stderr.contains("creating non-chore task without linked requirements"),
        "warning should be based on resolved --input-json payload"
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

    let requirements_docs = harness.project_root().join(".ao/docs/requirements.json");
    assert!(
        requirements_docs.exists(),
        "requirements docs file should exist"
    );
    let requirements_docs_payload: Value = serde_json::from_str(
        &std::fs::read_to_string(&requirements_docs)
            .context("requirements docs should be readable")?,
    )
    .context("requirements docs should contain valid JSON")?;
    let docs_items = requirements_docs_payload
        .as_array()
        .context("requirements docs should contain an array")?;
    assert!(
        docs_items
            .iter()
            .any(|item| item.get("id").and_then(Value::as_str) == Some(requirement_id.as_str())),
        "requirements docs should contain the created requirement"
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

#[test]
fn e2e_daemon_config_persists_auto_prune_worktrees_after_merge() -> Result<()> {
    let harness = CliHarness::new()?;

    let configured = harness.run_json_ok(&[
        "daemon",
        "config",
        "--auto-prune-worktrees-after-merge",
        "true",
    ])?;
    assert_eq!(
        configured
            .pointer("/data/auto_prune_worktrees_after_merge")
            .and_then(Value::as_bool),
        Some(true)
    );

    let pm_config_path = harness.project_root().join(".ao").join("pm-config.json");
    let pm_config_content =
        std::fs::read_to_string(pm_config_path).context("pm-config should be readable")?;
    let pm_config: Value =
        serde_json::from_str(&pm_config_content).context("pm-config should parse as JSON")?;
    assert_eq!(
        pm_config
            .get("auto_prune_worktrees_after_merge")
            .and_then(Value::as_bool),
        Some(true)
    );

    Ok(())
}

#[test]
fn e2e_task_delete_requires_confirmation_and_supports_dry_run() -> Result<()> {
    let harness = CliHarness::new()?;

    let created = harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "Delete me",
        "--description",
        "Task deletion confirmation test",
    ])?;
    let task_id = created
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("task create should return data.id")?
        .to_string();

    let confirmation_error = harness.run_json_err(&["task", "delete", "--id", &task_id])?;
    let confirmation_message = confirmation_error
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        confirmation_message,
        format!(
            "CONFIRMATION_REQUIRED: rerun 'ao task delete --id {} --confirm {}'; use --dry-run to preview changes",
            task_id, task_id
        ),
        "task delete confirmation message should use canonical token order"
    );

    let preview = harness.run_json_ok(&["task", "delete", "--id", &task_id, "--dry-run"])?;
    assert_shared_destructive_dry_run_contract(&preview, "task.delete", true);

    harness.run_json_ok(&["task", "get", "--id", &task_id])?;
    harness.run_json_ok(&["task", "delete", "--id", &task_id, "--confirm", &task_id])?;

    let not_found = harness.run_json_err(&["task", "get", "--id", &task_id])?;
    assert_eq!(
        not_found.pointer("/error/code").and_then(Value::as_str),
        Some("not_found")
    );

    Ok(())
}

#[test]
fn e2e_task_control_cancel_requires_confirmation_and_supports_dry_run() -> Result<()> {
    let harness = CliHarness::new()?;

    let created = harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "Cancelable task",
        "--description",
        "Task control cancellation confirmation test",
    ])?;
    let task_id = created
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("task create should return data.id")?
        .to_string();

    let confirmation_error =
        harness.run_json_err(&["task-control", "cancel", "--task-id", &task_id])?;
    let confirmation_message = confirmation_error
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        confirmation_message,
        format!(
            "CONFIRMATION_REQUIRED: rerun 'ao task-control cancel --task-id {} --confirm {}'; use --dry-run to preview changes",
            task_id, task_id
        ),
        "task-control cancel confirmation message should use canonical token order"
    );

    let preview =
        harness.run_json_ok(&["task-control", "cancel", "--task-id", &task_id, "--dry-run"])?;
    assert_shared_destructive_dry_run_contract(&preview, "task-control.cancel", true);

    let before_cancel = harness.run_json_ok(&["task", "get", "--id", &task_id])?;
    assert_eq!(
        before_cancel
            .pointer("/data/cancelled")
            .and_then(Value::as_bool),
        Some(false)
    );

    let cancelled = harness.run_json_ok(&[
        "task-control",
        "cancel",
        "--task-id",
        &task_id,
        "--confirm",
        &task_id,
    ])?;
    assert_eq!(
        cancelled.pointer("/data/success").and_then(Value::as_bool),
        Some(true)
    );

    let after_cancel = harness.run_json_ok(&["task", "get", "--id", &task_id])?;
    assert_eq!(
        after_cancel
            .pointer("/data/cancelled")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        after_cancel.pointer("/data/status").and_then(Value::as_str),
        Some("cancelled")
    );

    Ok(())
}

#[test]
fn e2e_task_control_rebalance_priority_dry_run_and_apply() -> Result<()> {
    let harness = CliHarness::new()?;

    let blocked = harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "Blocked candidate",
        "--description",
        "Should become critical",
        "--priority",
        "high",
    ])?;
    let blocked_id = blocked
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("blocked task should return id")?
        .to_string();
    harness.run_json_ok(&["task", "status", "--id", &blocked_id, "--status", "blocked"])?;

    let early = harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "Early in progress",
        "--description",
        "Should become high from medium",
        "--priority",
        "medium",
    ])?;
    let early_id = early
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("early task should return id")?
        .to_string();
    harness.run_json_ok(&[
        "task",
        "status",
        "--id",
        &early_id,
        "--status",
        "in-progress",
    ])?;
    harness.run_json_ok(&[
        "task-control",
        "set-deadline",
        "--task-id",
        &early_id,
        "--deadline",
        "2026-03-01T09:00:00Z",
    ])?;

    let late = harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "Late in progress",
        "--description",
        "Should be demoted to medium",
        "--priority",
        "high",
    ])?;
    let late_id = late
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("late task should return id")?
        .to_string();
    harness.run_json_ok(&[
        "task",
        "status",
        "--id",
        &late_id,
        "--status",
        "in-progress",
    ])?;
    harness.run_json_ok(&[
        "task-control",
        "set-deadline",
        "--task-id",
        &late_id,
        "--deadline",
        "2026-03-10T09:00:00Z",
    ])?;

    let dry_run = harness.run_json_ok(&[
        "task-control",
        "rebalance-priority",
        "--high-budget-percent",
        "34",
    ])?;
    assert_eq!(
        dry_run.pointer("/data/dry_run").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        dry_run.pointer("/data/operation").and_then(Value::as_str),
        Some("task-control.rebalance-priority")
    );
    assert_eq!(
        dry_run
            .pointer("/data/plan/high_budget_percent")
            .and_then(Value::as_u64),
        Some(34)
    );
    assert_eq!(
        dry_run
            .pointer("/data/plan/after/active_by_priority/high")
            .and_then(Value::as_u64),
        Some(1)
    );

    let confirmation_error = harness.run_json_err(&[
        "task-control",
        "rebalance-priority",
        "--high-budget-percent",
        "34",
        "--apply",
    ])?;
    let confirmation_message = confirmation_error
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        confirmation_message.contains("CONFIRMATION_REQUIRED"),
        "apply mode should require confirmation"
    );

    let applied = harness.run_json_ok(&[
        "task-control",
        "rebalance-priority",
        "--high-budget-percent",
        "34",
        "--apply",
        "--confirm",
        "apply",
    ])?;
    assert_eq!(
        applied.pointer("/data/success").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        applied.pointer("/data/dry_run").and_then(Value::as_bool),
        Some(false)
    );

    let blocked_after = harness.run_json_ok(&["task", "get", "--id", &blocked_id])?;
    assert_eq!(
        blocked_after
            .pointer("/data/priority")
            .and_then(Value::as_str),
        Some("critical")
    );

    let early_after = harness.run_json_ok(&["task", "get", "--id", &early_id])?;
    assert_eq!(
        early_after
            .pointer("/data/priority")
            .and_then(Value::as_str),
        Some("high")
    );

    let late_after = harness.run_json_ok(&["task", "get", "--id", &late_id])?;
    assert_eq!(
        late_after.pointer("/data/priority").and_then(Value::as_str),
        Some("medium")
    );

    Ok(())
}

#[test]
fn e2e_task_control_rebalance_priority_rejects_conflicting_overrides() -> Result<()> {
    let harness = CliHarness::new()?;

    let created = harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "Conflicting override",
        "--description",
        "Should fail validation",
    ])?;
    let task_id = created
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("task create should return data.id")?
        .to_string();
    harness.run_json_ok(&["task", "status", "--id", &task_id, "--status", "ready"])?;

    let payload = harness.run_json_err(&[
        "task-control",
        "rebalance-priority",
        "--essential-task-id",
        &task_id,
        "--nice-to-have-task-id",
        &task_id,
    ])?;
    let message = payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(message.contains("conflicting task ids provided in overrides"));
    assert!(message.contains(task_id.as_str()));

    Ok(())
}

#[test]
fn e2e_workflow_destructive_commands_require_confirmation_and_dry_run_support() -> Result<()> {
    let harness = CliHarness::new()?;

    let created = harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "Workflow target",
        "--description",
        "workflow cancellation test",
    ])?;
    let task_id = created
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("task create should return data.id")?
        .to_string();
    let workflow = harness.run_json_ok(&["workflow", "run", "--task-id", &task_id])?;
    let workflow_id = workflow
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("workflow run should return data.id")?
        .to_string();

    let pause_error = harness.run_json_err(&["workflow", "pause", "--id", &workflow_id])?;
    let pause_confirmation_message = pause_error
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        pause_confirmation_message,
        format!(
            "CONFIRMATION_REQUIRED: rerun 'ao workflow pause --id {} --confirm {}'; use --dry-run to preview changes",
            workflow_id, workflow_id
        ),
        "workflow pause confirmation message should use canonical token order"
    );

    let pause_preview =
        harness.run_json_ok(&["workflow", "pause", "--id", &workflow_id, "--dry-run"])?;
    assert_shared_destructive_dry_run_contract(&pause_preview, "workflow.pause", true);

    let cancel_error = harness.run_json_err(&["workflow", "cancel", "--id", &workflow_id])?;
    let cancel_confirmation_message = cancel_error
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        cancel_confirmation_message,
        format!(
            "CONFIRMATION_REQUIRED: rerun 'ao workflow cancel --id {} --confirm {}'; use --dry-run to preview changes",
            workflow_id, workflow_id
        ),
        "workflow cancel confirmation message should use canonical token order"
    );

    let cancel_preview =
        harness.run_json_ok(&["workflow", "cancel", "--id", &workflow_id, "--dry-run"])?;
    assert_shared_destructive_dry_run_contract(&cancel_preview, "workflow.cancel", true);

    let cancelled = harness.run_json_ok(&[
        "workflow",
        "cancel",
        "--id",
        &workflow_id,
        "--confirm",
        &workflow_id,
    ])?;
    assert_eq!(
        cancelled.pointer("/data/id").and_then(Value::as_str),
        Some(workflow_id.as_str())
    );
    assert_eq!(
        cancelled.pointer("/data/status").and_then(Value::as_str),
        Some("cancelled")
    );

    let phase_id = "tmp-removable-phase";
    let phase_definition = "{\"mode\":\"agent\",\"agent_id\":\"default\",\"directive\":null,\"runtime\":null,\"output_contract\":null,\"output_json_schema\":null,\"command\":null,\"manual\":null}";
    harness.run_json_ok(&[
        "workflow",
        "phases",
        "upsert",
        "--phase",
        phase_id,
        "--input-json",
        phase_definition,
    ])?;

    let remove_error =
        harness.run_json_err(&["workflow", "phases", "remove", "--phase", phase_id])?;
    let remove_confirmation_message = remove_error
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        remove_confirmation_message,
        format!(
            "CONFIRMATION_REQUIRED: rerun 'ao workflow phases remove --phase {} --confirm {}'; use --dry-run to preview changes",
            phase_id, phase_id
        ),
        "workflow phases remove confirmation message should use canonical token order"
    );

    let remove_preview = harness.run_json_ok(&[
        "workflow",
        "phases",
        "remove",
        "--phase",
        phase_id,
        "--dry-run",
    ])?;
    assert_shared_destructive_dry_run_contract(&remove_preview, "workflow.phases.remove", true);
    assert_eq!(
        remove_preview
            .pointer("/data/can_remove")
            .and_then(Value::as_bool),
        Some(true)
    );

    let removed = harness.run_json_ok(&[
        "workflow",
        "phases",
        "remove",
        "--phase",
        phase_id,
        "--confirm",
        phase_id,
    ])?;
    assert_eq!(
        removed.pointer("/data/removed").and_then(Value::as_str),
        Some(phase_id)
    );

    Ok(())
}

#[test]
fn e2e_git_worktree_remove_requires_confirmation_and_supports_dry_run() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&["git", "repo", "init", "--name", "demo"])?;
    let repo = harness.run_json_ok(&["git", "repo", "get", "--repo", "demo"])?;
    let repo_path = repo
        .pointer("/data/path")
        .and_then(Value::as_str)
        .context("git repo get should return data.path")?;

    let seed_file = std::path::Path::new(repo_path).join("README.md");
    std::fs::write(&seed_file, "seed\n").context("failed to seed git repo")?;

    let git_add = Command::new("git")
        .args(["-C", repo_path, "add", "."])
        .output()
        .context("failed to run git add")?;
    assert!(
        git_add.status.success(),
        "git add failed: {}",
        String::from_utf8_lossy(&git_add.stderr)
    );

    let git_commit = Command::new("git")
        .args([
            "-C",
            repo_path,
            "-c",
            "user.name=AO Test",
            "-c",
            "user.email=ao@example.com",
            "commit",
            "-m",
            "seed",
        ])
        .output()
        .context("failed to run git commit")?;
    assert!(
        git_commit.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&git_commit.stderr)
    );

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

    let remove_error = harness.run_json_err(&[
        "git",
        "worktree",
        "remove",
        "--repo",
        "demo",
        "--worktree-name",
        worktree_name,
    ])?;
    let remove_confirmation_message = remove_error
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
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

    let push_preview =
        harness.run_json_ok(&["git", "push", "--repo", "demo", "--force", "--dry-run"])?;
    assert_shared_destructive_dry_run_contract(&push_preview, "git.push", true);
    assert_eq!(
        push_preview.pointer("/data/next_step").and_then(Value::as_str),
        Some(
            "request and approve a git confirmation for 'force_push' on 'demo', then rerun with --confirmation-id <id>"
        )
    );

    assert!(
        worktree_path.exists(),
        "dry-run should not remove worktree path"
    );

    Ok(())
}

#[test]
fn e2e_git_worktree_prune_cleans_done_task_worktrees() -> Result<()> {
    let harness = CliHarness::new()?;

    harness.run_json_ok(&["git", "repo", "init", "--name", "demo"])?;
    let repo = harness.run_json_ok(&["git", "repo", "get", "--repo", "demo"])?;
    let repo_path = repo
        .pointer("/data/path")
        .and_then(Value::as_str)
        .context("git repo get should return data.path")?;

    let seed_file = std::path::Path::new(repo_path).join("README.md");
    std::fs::write(&seed_file, "seed\n").context("failed to seed git repo")?;

    let git_add = Command::new("git")
        .args(["-C", repo_path, "add", "."])
        .output()
        .context("failed to run git add")?;
    assert!(
        git_add.status.success(),
        "git add failed: {}",
        String::from_utf8_lossy(&git_add.stderr)
    );

    let git_commit = Command::new("git")
        .args([
            "-C",
            repo_path,
            "-c",
            "user.name=AO Test",
            "-c",
            "user.email=ao@example.com",
            "commit",
            "-m",
            "seed",
        ])
        .output()
        .context("failed to run git commit")?;
    assert!(
        git_commit.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&git_commit.stderr)
    );

    let created_task = harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "Prune worktree candidate",
        "--description",
        "done task should be pruned",
    ])?;
    let task_id = created_task
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("task create should return data.id")?
        .to_string();
    harness.run_json_ok(&["task", "status", "--id", &task_id, "--status", "done"])?;

    let task_token = task_id.to_ascii_lowercase();
    let branch_name = format!("ao/{task_token}");
    let worktree_name = format!("task-{task_token}");
    let managed_root = harness
        .config_root()
        .join(".ao")
        .join(protocol::repository_scope_for_path(harness.project_root()))
        .join("worktrees");
    let worktree_path = managed_root.join(&worktree_name);
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).context("failed to create worktree parent directory")?;
    }
    let worktree_path_string = worktree_path.to_string_lossy().to_string();
    harness.run_json_ok(&[
        "git",
        "worktree",
        "create",
        "--repo",
        "demo",
        "--worktree-name",
        &worktree_name,
        "--worktree-path",
        &worktree_path_string,
        "--branch",
        &branch_name,
        "--create-branch",
    ])?;

    let preview = harness.run_json_ok(&[
        "git",
        "worktree",
        "prune",
        "--repo",
        "demo",
        "--delete-remote-branch",
        "--dry-run",
    ])?;
    assert_shared_destructive_dry_run_contract(&preview, "git.worktree.prune", true);
    assert_eq!(
        preview
            .pointer("/data/candidate_count")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        preview
            .pointer("/data/candidates/0/task_id")
            .and_then(Value::as_str),
        Some(task_id.as_str())
    );
    assert!(
        worktree_path.exists(),
        "dry-run should not remove candidate worktree path"
    );

    let prune_confirmation_error =
        harness.run_json_err(&["git", "worktree", "prune", "--repo", "demo"])?;
    let prune_confirmation_message = prune_confirmation_error
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        prune_confirmation_message,
        "CONFIRMATION_REQUIRED: request and approve a git confirmation for 'prune_worktrees' on 'demo', then rerun with --confirmation-id <id>; use --dry-run to preview changes",
        "git worktree prune confirmation message should use canonical token order"
    );

    let confirmation_request = harness.run_json_ok(&[
        "git",
        "confirm",
        "request",
        "--operation-type",
        "prune_worktrees",
        "--repo-name",
        "demo",
    ])?;
    let confirmation_id = confirmation_request
        .pointer("/data/id")
        .and_then(Value::as_str)
        .context("git confirm request should return data.id")?
        .to_string();
    harness.run_json_ok(&[
        "git",
        "confirm",
        "respond",
        "--request-id",
        &confirmation_id,
        "--approved",
    ])?;

    let pruned = harness.run_json_ok(&[
        "git",
        "worktree",
        "prune",
        "--repo",
        "demo",
        "--confirmation-id",
        &confirmation_id,
    ])?;
    assert_eq!(
        pruned.pointer("/data/pruned_count").and_then(Value::as_u64),
        Some(1)
    );
    assert!(
        !worktree_path.exists(),
        "prune should remove candidate worktree path"
    );

    let task_after_prune = harness.run_json_ok(&["task", "get", "--id", &task_id])?;
    assert!(
        task_after_prune
            .pointer("/data/worktree_path")
            .map(Value::is_null)
            .unwrap_or(true),
        "prune should clear stale task worktree_path metadata"
    );

    let listed = harness.run_json_ok(&["git", "worktree", "list", "--repo", "demo"])?;
    let entries = listed
        .pointer("/data")
        .and_then(Value::as_array)
        .context("git worktree list should return data array")?;
    assert!(
        !entries.iter().any(|entry| {
            entry
                .get("path")
                .and_then(Value::as_str)
                .map(|path| path == worktree_path_string)
                .unwrap_or(false)
        }),
        "pruned worktree should not appear in git worktree list"
    );

    Ok(())
}

#[test]
fn e2e_git_repo_init_failure_is_reported_and_not_registered() -> Result<()> {
    let harness = CliHarness::new()?;
    let occupied_path = harness.project_root().join("occupied-path");
    std::fs::write(&occupied_path, "blocking file\n")
        .context("failed to create occupied path file")?;
    let occupied_path_string = occupied_path.to_string_lossy().to_string();

    let failed_init = harness.run_json_err(&[
        "git",
        "repo",
        "init",
        "--name",
        "broken",
        "--path",
        &occupied_path_string,
    ])?;
    let error_message = failed_init
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        error_message.contains("git init failed"),
        "expected git init failure message, got: {error_message}"
    );

    let listed = harness.run_json_ok(&["git", "repo", "list"])?;
    let repos = listed
        .pointer("/data")
        .and_then(Value::as_array)
        .context("git repo list should return data array")?;
    assert!(
        !repos.iter().any(|repo| {
            repo.get("name")
                .and_then(Value::as_str)
                .map(|name| name == "broken")
                .unwrap_or(false)
        }),
        "failed git init should not register repo entry"
    );

    Ok(())
}
