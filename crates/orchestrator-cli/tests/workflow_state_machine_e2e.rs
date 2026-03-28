#[path = "support/test_harness.rs"]
pub mod test_harness;

use anyhow::{Context, Result};
use serde_json::Value;
use test_harness::CliHarness;

#[test]
fn e2e_workflow_state_machine_transition_cycle() -> Result<()> {
    let harness = CliHarness::new()?;

    let created = harness.run_json_ok(&[
        "task",
        "create",
        "--title",
        "Workflow transition target",
        "--description",
        "workflow transition e2e validation",
    ])?;
    let task_id =
        created.pointer("/data/id").and_then(Value::as_str).context("task create should return data.id")?.to_string();

    let started = harness.run_json_ok(&["workflow", "run", "--task-id", &task_id])?;
    let workflow_id =
        started.pointer("/data/id").and_then(Value::as_str).context("workflow run should return data.id")?.to_string();
    assert_eq!(started.pointer("/data/status").and_then(Value::as_str), Some("running"));
    assert_eq!(started.pointer("/data/machine_state").and_then(Value::as_str), Some("run-phase"));

    let paused = harness.run_json_ok(&["workflow", "pause", "--id", &workflow_id, "--confirm", &workflow_id])?;
    assert_eq!(paused.pointer("/data/status").and_then(Value::as_str), Some("paused"));
    assert_eq!(paused.pointer("/data/machine_state").and_then(Value::as_str), Some("paused"));

    let resumed = harness.run_json_ok(&["workflow", "resume", "--id", &workflow_id])?;
    assert_eq!(resumed.pointer("/data/status").and_then(Value::as_str), Some("running"));
    assert_eq!(resumed.pointer("/data/machine_state").and_then(Value::as_str), Some("run-phase"));

    let cancelled = harness.run_json_ok(&["workflow", "cancel", "--id", &workflow_id, "--confirm", &workflow_id])?;
    assert_eq!(cancelled.pointer("/data/status").and_then(Value::as_str), Some("cancelled"));
    assert_eq!(cancelled.pointer("/data/machine_state").and_then(Value::as_str), Some("cancelled"));

    let resume_status = harness.run_json_ok(&["workflow", "resume-status", "--id", &workflow_id])?;
    assert_eq!(resume_status.pointer("/data/machine_state").and_then(Value::as_str), Some("cancelled"));
    assert_eq!(resume_status.pointer("/data/resumability/kind").and_then(Value::as_str), Some("invalid_state"));

    Ok(())
}

#[test]
fn e2e_workflow_state_machine_json_contract_endpoints() -> Result<()> {
    let harness = CliHarness::new()?;

    let state_machine_get = harness.run_json_ok(&["workflow", "state-machine", "get"])?;
    assert_eq!(state_machine_get.pointer("/data/schema").and_then(Value::as_str), Some("ao.state-machines.v1"));
    let machine_path = state_machine_get
        .pointer("/data/path")
        .and_then(Value::as_str)
        .context("workflow state-machine get should return data.path")?;
    assert!(std::path::Path::new(machine_path).exists(), "state machine metadata path should exist: {machine_path}");
    let transitions = state_machine_get
        .pointer("/data/state_machines/workflow/transitions")
        .and_then(Value::as_array)
        .context("workflow state-machine get should include transitions array")?;
    assert!(!transitions.is_empty(), "workflow state machine transitions should not be empty");

    let state_machine_validate = harness.run_json_ok(&["workflow", "state-machine", "validate"])?;
    assert_eq!(state_machine_validate.pointer("/data/valid").and_then(Value::as_bool), Some(true));
    assert_eq!(
        state_machine_validate.pointer("/data/errors").and_then(Value::as_array).map(std::vec::Vec::len),
        Some(0)
    );

    Ok(())
}
