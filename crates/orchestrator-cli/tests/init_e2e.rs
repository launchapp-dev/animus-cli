#[path = "support/test_harness.rs"]
pub mod test_harness;

use anyhow::Result;
use serde_json::Value;
use std::fs;
use test_harness::CliHarness;

#[test]
fn init_non_interactive_requires_template_or_path() -> Result<()> {
    let harness = CliHarness::new()?;

    let (payload, status) = harness.run_json_err_with_exit(&["init", "--non-interactive", "--plan"])?;
    assert_eq!(status, 2);
    assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"));
    assert!(payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("non-interactive init requires --template or --path"));

    Ok(())
}

#[test]
fn init_plan_reports_selected_template_and_required_changes() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok(&["init", "--template", "task-queue", "--non-interactive", "--plan"])?;
    assert_eq!(payload.pointer("/data/stage").and_then(Value::as_str), Some("plan"));
    assert_eq!(payload.pointer("/data/mode").and_then(Value::as_str), Some("non_interactive"));
    assert_eq!(payload.pointer("/data/template/id").and_then(Value::as_str), Some("task-queue"));
    assert_eq!(payload.pointer("/data/template/source_kind").and_then(Value::as_str), Some("bundled"));
    assert_eq!(payload.pointer("/data/apply/applied").and_then(Value::as_bool), Some(false));
    assert!(payload
        .pointer("/data/required_changes/template_files")
        .and_then(Value::as_array)
        .is_some_and(|files| {
            files.iter().any(|file| {
                matches!(
                    (
                        file.get("path").and_then(Value::as_str),
                        file.get("action").and_then(Value::as_str)
                    ),
                    (Some(".ao/workflows/standard-workflow.yaml"), Some("create"))
                )
            })
        }));
    assert!(payload
        .pointer("/data/required_changes/daemon_config")
        .and_then(Value::as_array)
        .is_some_and(|fields| {
            fields.iter().any(|field| {
                matches!(
                    (
                        field.get("field").and_then(Value::as_str),
                        field.get("after").and_then(Value::as_bool)
                    ),
                    (Some("auto_merge_enabled"), Some(true))
                )
            })
        }));

    Ok(())
}

#[test]
fn init_apply_writes_template_files_and_daemon_defaults() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok(&["init", "--template", "conductor", "--non-interactive"])?;
    assert_eq!(payload.pointer("/data/stage").and_then(Value::as_str), Some("apply"));
    assert_eq!(payload.pointer("/data/template/id").and_then(Value::as_str), Some("conductor"));
    assert_eq!(payload.pointer("/data/apply/applied").and_then(Value::as_bool), Some(true));
    assert!(payload
        .pointer("/data/apply/changed_domains")
        .and_then(Value::as_array)
        .is_some_and(|domains| domains.iter().any(|value| value.as_str() == Some("template_files"))));
    assert!(payload
        .pointer("/data/apply/written_files")
        .and_then(Value::as_array)
        .is_some_and(|files| files.iter().any(|value| value.as_str() == Some(".ao/workflows/conductor-workflow.yaml"))));

    let conductor_path = harness.project_root().join(".ao/workflows/conductor-workflow.yaml");
    assert!(conductor_path.exists(), "conductor template should write its workflow wrapper");
    let conductor_contents = fs::read_to_string(&conductor_path)?;
    assert!(conductor_contents.contains("ao.requirement/plan"));

    let pm_config_path = harness.scoped_root().join("daemon").join("pm-config.json");
    let pm_config: Value = serde_json::from_str(&fs::read_to_string(pm_config_path)?)?;
    assert_eq!(pm_config.get("auto_merge_enabled").and_then(Value::as_bool), Some(false));
    assert_eq!(pm_config.get("auto_pr_enabled").and_then(Value::as_bool), Some(true));
    assert_eq!(pm_config.get("auto_commit_before_merge").and_then(Value::as_bool), Some(false));

    Ok(())
}

#[test]
fn init_rejects_conflicting_project_files_without_force() -> Result<()> {
    let harness = CliHarness::new()?;
    let custom_workflow_path = harness.project_root().join(".ao/workflows/custom.yaml");
    fs::create_dir_all(custom_workflow_path.parent().expect("workflow path should have a parent"))?;
    fs::write(&custom_workflow_path, "user-owned workflow")?;

    let (payload, status) = harness.run_json_err_with_exit(&["init", "--template", "task-queue", "--non-interactive"])?;
    assert_eq!(status, 4);
    assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("conflict"));
    assert!(payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains(".ao/workflows/custom.yaml"));
    assert_eq!(fs::read_to_string(custom_workflow_path)?, "user-owned workflow");

    Ok(())
}

#[test]
fn init_supports_local_template_directories() -> Result<()> {
    let harness = CliHarness::new()?;
    let template_root = tempfile::tempdir()?;
    let source_root = template_root.path().join("skeleton/.ao/workflows");
    fs::create_dir_all(&source_root)?;
    fs::write(
        template_root.path().join("template.toml"),
        r#"schema = "animus.template.v1"
id = "local-copy"
version = "0.1.0"
title = "Local Copy Template"
description = "Local template fixture for init e2e coverage."
pattern = "local-copy"
next_steps = ["animus workflow list"]

[source]
mode = "copy"
root = "skeleton"

[daemon]
auto_merge = true
auto_pr = false
auto_commit_before_merge = true
"#,
    )?;
    fs::write(
        source_root.join("local-template.yaml"),
        "workflows:\n  - id: local-template\n    name: Local Template\n    phases:\n      - workflow_ref: ao.task/standard\n",
    )?;

    let template_path = template_root.path().to_string_lossy().into_owned();
    let payload = harness.run_json_ok(&["init", "--path", &template_path, "--non-interactive"])?;
    assert_eq!(payload.pointer("/data/template/id").and_then(Value::as_str), Some("local-copy"));
    assert_eq!(payload.pointer("/data/template/source_kind").and_then(Value::as_str), Some("local"));

    let local_workflow_path = harness.project_root().join(".ao/workflows/local-template.yaml");
    assert!(local_workflow_path.exists(), "local template file should be copied into the project");
    assert!(fs::read_to_string(&local_workflow_path)?.contains("local-template"));

    let pm_config_path = harness.scoped_root().join("daemon").join("pm-config.json");
    let pm_config: Value = serde_json::from_str(&fs::read_to_string(pm_config_path)?)?;
    assert_eq!(pm_config.get("auto_merge_enabled").and_then(Value::as_bool), Some(true));
    assert_eq!(pm_config.get("auto_pr_enabled").and_then(Value::as_bool), Some(false));
    assert_eq!(pm_config.get("auto_commit_before_merge").and_then(Value::as_bool), Some(true));

    Ok(())
}
