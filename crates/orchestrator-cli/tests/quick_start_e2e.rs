#[path = "support/test_harness.rs"]
mod test_harness;

use anyhow::Result;
use serde_json::Value;
use test_harness::CliHarness;

/// Smoke test for the documented quick-start path.
/// Exercises the sequence from docs/getting-started/quick-start.md:
/// 1. ao doctor
/// 2. ao setup
/// 3. ao task create
/// 4. ao task stats
/// 5. ao status
/// 6. ao workflow run --sync (dry-run)
///
/// This test fails visibly with a message pointing to the docs if any command fails.
#[test]
fn quick_start_happy_path() -> Result<()> {
    let harness = CliHarness::new().map_err(|e| {
        eprintln!("Failed to initialize test harness. See docs/getting-started/quick-start.md for the documented quick-start path.");
        e
    })?;

    // Step 1: ao doctor
    let doctor_result = harness.run_json_ok(&["doctor"]).map_err(|e| {
        eprintln!(
            "QUICK-START FAILURE: 'ao doctor' failed. This is the first documented step.\n\
             See docs/getting-started/quick-start.md for the documented quick-start path.\n\
             Error: {}", e
        );
        e
    })?;
    assert_eq!(
        doctor_result.get("ok").and_then(Value::as_bool),
        Some(true),
        "ao doctor should return ok=true"
    );

    // Step 2: ao setup
    let setup_result = harness
        .run_json_ok(&[
            "setup",
            "--non-interactive",
            "--auto-merge",
            "true",
            "--auto-pr",
            "false",
            "--auto-commit-before-merge",
            "true",
        ])
        .map_err(|e| {
            eprintln!(
                "QUICK-START FAILURE: 'ao setup' failed. This is the second documented step.\n\
                 See docs/getting-started/quick-start.md for the documented quick-start path.\n\
                 Error: {}", e
            );
            e
        })?;
    assert_eq!(
        setup_result.get("ok").and_then(Value::as_bool),
        Some(true),
        "ao setup should return ok=true"
    );
    assert_eq!(
        setup_result.pointer("/data/stage").and_then(Value::as_str),
        Some("apply"),
        "ao setup should reach apply stage"
    );

    // Verify .ao structure was created
    assert!(
        harness.project_root().join(".ao").join("config.json").exists(),
        ".ao/config.json should be created by setup"
    );
    assert!(
        harness.project_root().join(".ao").join("workflows").exists(),
        ".ao/workflows directory should be created by setup"
    );

    // Step 3: ao task create
    let task_create_result = harness
        .run_json_ok(&[
            "task",
            "create",
            "--title",
            "Add rate limiting",
            "--description",
            "Throttle API requests before they hit the upstream provider",
            "--task-type",
            "feature",
            "--priority",
            "high",
        ])
        .map_err(|e| {
            eprintln!(
                "QUICK-START FAILURE: 'ao task create' failed. This is the third documented step.\n\
                 See docs/getting-started/quick-start.md for the documented quick-start path.\n\
                 Error: {}", e
            );
            e
        })?;
    assert_eq!(
        task_create_result.get("ok").and_then(Value::as_bool),
        Some(true),
        "ao task create should return ok=true"
    );

    let task_id = task_create_result
        .pointer("/data/id")
        .and_then(Value::as_str)
        .expect("task_create should return data.id");
    assert!(task_id.starts_with("TASK-"), "task_id should start with TASK-");

    // Step 4: ao task stats
    let task_stats_result = harness.run_json_ok(&["task", "stats"]).map_err(|e| {
        eprintln!(
            "QUICK-START FAILURE: 'ao task stats' failed. This is part of the inspection flow.\n\
             See docs/getting-started/quick-start.md for the documented quick-start path.\n\
             Error: {}", e
        );
        e
    })?;
    assert_eq!(
        task_stats_result.get("ok").and_then(Value::as_bool),
        Some(true),
        "ao task stats should return ok=true"
    );
    assert!(
        task_stats_result.pointer("/data/total").is_some(),
        "ao task stats should include data.total"
    );
    assert_eq!(
        task_stats_result.pointer("/data/total").and_then(Value::as_u64),
        Some(1),
        "ao task stats should report 1 total task after creation"
    );

    // Step 5: ao status
    let status_result = harness.run_json_ok(&["status"]).map_err(|e| {
        eprintln!(
            "QUICK-START FAILURE: 'ao status' failed. This is part of the inspection flow.\n\
             See docs/getting-started/quick-start.md for the documented quick-start path.\n\
             Error: {}", e
        );
        e
    })?;
    assert_eq!(
        status_result.get("ok").and_then(Value::as_bool),
        Some(true),
        "ao status should return ok=true"
    );

    // Step 6: ao workflow run --sync
    // This tests the workflow invocation synchronously as documented
    let workflow_run_result = harness
        .run_json_ok(&["workflow", "run", "--task-id", task_id, "--sync"])
        .map_err(|e| {
            eprintln!(
                "QUICK-START FAILURE: 'ao workflow run --sync' failed. This is the fourth documented step.\n\
                 See docs/getting-started/quick-start.md for the documented quick-start path.\n\
                 Error: {}", e
            );
            e
        })?;
    assert_eq!(
        workflow_run_result.get("ok").and_then(Value::as_bool),
        Some(true),
        "ao workflow run should return ok=true"
    );
    assert!(
        workflow_run_result.pointer("/data/workflow_id").is_some(),
        "ao workflow run should return a workflow_id"
    );

    Ok(())
}
