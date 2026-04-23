#[path = "support/test_harness.rs"]
pub mod test_harness;

use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use test_harness::CliHarness;

#[test]
fn setup_guided_plan_is_available_without_a_tty() -> Result<()> {
    let harness = CliHarness::new()?;

    let payload = harness.run_json_ok(&["setup", "--plan"])?;
    assert_eq!(payload.pointer("/data/stage").and_then(Value::as_str), Some("plan"));
    assert_eq!(payload.pointer("/data/mode").and_then(Value::as_str), Some("guided"));
    assert_eq!(payload.pointer("/data/apply/applied").and_then(Value::as_bool), Some(false));

    Ok(())
}

#[test]
fn setup_guided_apply_still_requires_interactive_terminal() -> Result<()> {
    let harness = CliHarness::new()?;

    let (payload, status) = harness.run_json_err_with_exit(&["setup"])?;
    assert_eq!(status, 2);
    assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"));
    assert!(payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("guided setup must be run in an interactive terminal"));

    Ok(())
}

#[test]
fn setup_non_interactive_requires_explicit_inputs() -> Result<()> {
    let harness = CliHarness::new()?;

    let (payload, status) = harness.run_json_err_with_exit(&["setup", "--non-interactive", "--plan"])?;
    assert_eq!(status, 2);
    assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"));
    assert!(payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("missing required non-interactive setup inputs"));

    Ok(())
}

#[test]
fn setup_plan_apply_and_idempotent_rerun_are_stable() -> Result<()> {
    let harness = CliHarness::new()?;
    let setup_flags = [
        "setup",
        "--non-interactive",
        "--auto-merge",
        "true",
        "--auto-pr",
        "false",
        "--auto-commit-before-merge",
        "true",
    ];

    let plan = harness.run_json_ok(&[
        "setup",
        "--non-interactive",
        "--plan",
        "--auto-merge",
        "true",
        "--auto-pr",
        "false",
        "--auto-commit-before-merge",
        "true",
    ])?;
    assert_eq!(plan.pointer("/data/stage").and_then(Value::as_str), Some("plan"));
    assert_eq!(plan.pointer("/data/mode").and_then(Value::as_str), Some("non_interactive"));
    assert_eq!(plan.pointer("/data/apply/applied").and_then(Value::as_bool), Some(false));

    let first_apply = harness.run_json_ok(&setup_flags)?;
    assert_eq!(first_apply.pointer("/data/stage").and_then(Value::as_str), Some("apply"));
    assert_eq!(first_apply.pointer("/data/apply/daemon_config_updated").and_then(Value::as_bool), Some(true));
    assert!(first_apply
        .pointer("/data/apply/changed_domains")
        .and_then(Value::as_array)
        .is_some_and(|domains| domains.iter().any(|value| value.as_str() == Some("project_bootstrap"))));

    let second_apply = harness.run_json_ok(&setup_flags)?;
    assert_eq!(second_apply.pointer("/data/apply/daemon_config_updated").and_then(Value::as_bool), Some(false));
    assert!(second_apply
        .pointer("/data/apply/unchanged_domains")
        .and_then(Value::as_array)
        .is_some_and(|domains| domains.iter().any(|value| value.as_str() == Some("project_bootstrap"))));

    let config_json_path = harness.project_root().join(".ao").join("config.json");
    assert!(config_json_path.exists(), "setup apply should bootstrap project config");
    let workflows_dir = harness.project_root().join(".ao").join("workflows");
    assert!(workflows_dir.join("custom.yaml").exists(), "setup apply should scaffold custom workflow YAML");
    assert!(
        workflows_dir.join("standard-workflow.yaml").exists(),
        "setup apply should scaffold standard workflow YAML"
    );
    let pm_config_path = harness.scoped_root().join("daemon").join("pm-config.json");
    assert!(pm_config_path.exists(), "setup apply should persist pm-config");
    let pm_config_content = std::fs::read_to_string(pm_config_path)?;
    let pm_config: Value = serde_json::from_str(&pm_config_content)?;
    assert_eq!(pm_config.get("auto_merge_enabled").and_then(Value::as_bool), Some(true));
    assert_eq!(pm_config.get("auto_pr_enabled").and_then(Value::as_bool), Some(false));
    assert_eq!(pm_config.get("auto_commit_before_merge").and_then(Value::as_bool), Some(true));

    Ok(())
}

#[test]
fn setup_plan_blocked_items_match_actionable_doctor_checks() -> Result<()> {
    let harness = CliHarness::new()?;

    let doctor = harness.run_json_ok(&["doctor"])?;
    let check_index: HashMap<String, (String, bool)> = doctor
        .pointer("/data/doctor/checks")
        .and_then(Value::as_array)
        .expect("doctor checks array should exist")
        .iter()
        .filter_map(|check| {
            let id = check.get("id").and_then(Value::as_str)?;
            let status = check.get("status").and_then(Value::as_str)?;
            let remediation_available = check.pointer("/remediation/available")?.as_bool()?;
            Some((id.to_string(), (status.to_string(), remediation_available)))
        })
        .collect();

    let plan = harness.run_json_ok(&[
        "setup",
        "--non-interactive",
        "--plan",
        "--auto-merge",
        "true",
        "--auto-pr",
        "false",
        "--auto-commit-before-merge",
        "true",
    ])?;
    let blocked_items =
        plan.pointer("/data/blocked_items").and_then(Value::as_array).expect("blocked_items should be an array");

    for blocked in blocked_items {
        let check_id = blocked.get("check_id").and_then(Value::as_str).expect("blocked item should include check_id");
        let (status, remediation_available) =
            check_index.get(check_id).expect("blocked item check_id should exist in doctor report");
        let actionable = status == "fail" || (status == "warn" && !*remediation_available);
        assert!(
            actionable,
            "blocked item should be actionable: id={check_id}, status={status}, remediation_available={remediation_available}"
        );
    }

    Ok(())
}

#[test]
fn doctor_reports_stable_checks_and_fix_actions() -> Result<()> {
    let harness = CliHarness::new()?;

    let doctor = harness.run_json_ok(&["doctor"])?;
    let checks =
        doctor.pointer("/data/doctor/checks").and_then(Value::as_array).expect("doctor checks array should exist");
    assert!(!checks.is_empty(), "doctor checks should not be empty");
    for check in checks {
        assert!(check.get("id").and_then(Value::as_str).is_some());
        assert!(check.get("status").and_then(Value::as_str).is_some());
        assert!(check.pointer("/remediation/id").and_then(Value::as_str).is_some());
        assert!(
            check.pointer("/remediation/available").and_then(Value::as_bool).is_some(),
            "remediation availability should be included"
        );
    }

    let fixed = harness.run_json_ok(&["doctor", "--fix"])?;
    assert_eq!(fixed.pointer("/data/fix/requested").and_then(Value::as_bool), Some(true));
    let actions = fixed.pointer("/data/fix/actions").and_then(Value::as_array).expect("fix actions should be an array");
    assert!(!actions.is_empty(), "doctor --fix should report action results");
    assert!(actions.iter().all(|action| {
        action.get("id").and_then(Value::as_str).is_some()
            && action.get("status").and_then(Value::as_str).is_some()
            && action.get("details").and_then(Value::as_str).is_some()
    }));

    Ok(())
}

#[test]
fn doctor_fix_skips_manual_ao_directory_repair() -> Result<()> {
    let harness = CliHarness::new()?;
    std::fs::write(harness.project_root().join(".ao"), "not a directory")?;

    let doctor = harness.run_json_ok(&["doctor"])?;
    let checks =
        doctor.pointer("/data/doctor/checks").and_then(Value::as_array).expect("doctor checks array should exist");

    for id in ["ao_directory_present", "daemon_config_valid_json"] {
        let check = checks
            .iter()
            .find(|check| check.get("id").and_then(Value::as_str) == Some(id))
            .expect("check should exist");
        assert_eq!(check.get("status").and_then(Value::as_str), Some("fail"));
        assert_eq!(check.pointer("/remediation/id").and_then(Value::as_str), Some("manual_ao_directory_repair"));
        assert_eq!(check.pointer("/remediation/available").and_then(Value::as_bool), Some(false));
        assert_eq!(check.pointer("/remediation/command").and_then(Value::as_str), None);
    }

    let fixed = harness.run_json_ok(&["doctor", "--fix"])?;
    assert_eq!(fixed.pointer("/data/fix/applied").and_then(Value::as_bool), Some(false));
    let actions = fixed.pointer("/data/fix/actions").and_then(Value::as_array).expect("fix actions should be an array");
    assert_eq!(actions.len(), 3);
    assert!(actions.iter().any(|action| {
        matches!(
            (action.get("id").and_then(Value::as_str), action.get("status").and_then(Value::as_str)),
            (Some("bootstrap_project_state"), Some("failed"))
        )
    }));
    assert!(actions.iter().any(|action| {
        matches!(
            (action.get("id").and_then(Value::as_str), action.get("status").and_then(Value::as_str)),
            (Some("create_default_daemon_config"), Some("skipped"))
        )
    }));
    assert!(actions.iter().any(|action| {
        matches!(
            (action.get("id").and_then(Value::as_str), action.get("status").and_then(Value::as_str)),
            (Some("start_runner"), Some("skipped"))
        )
    }));
    assert!(harness.project_root().join(".ao").is_file());

    Ok(())
}
