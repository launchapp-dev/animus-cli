#[path = "support/test_harness.rs"]
pub mod test_harness;

use anyhow::Result;
use serde_json::Value;
use test_harness::CliHarness;

#[test]
fn doctor_reports_stable_checks_and_fix_actions() -> Result<()> {
    let harness = CliHarness::new()?;

    // A fresh harness project has no plugins installed, so the required-roles
    // check fails and the gateable exit contract returns exit 5 — but the
    // doctor data envelope is still emitted on stdout.
    let (doctor, exit) = harness.run_json_stdout_with_exit(&["doctor"])?;
    assert_eq!(exit, 5, "doctor with failing checks should exit non-zero so `doctor && next` can gate");
    let checks =
        doctor.pointer("/data/doctor/checks").and_then(Value::as_array).expect("doctor checks array should exist");
    assert!(!checks.is_empty(), "doctor checks should not be empty");
    assert_eq!(
        doctor.pointer("/data/summary/overall").and_then(Value::as_str),
        Some("unhealthy"),
        "fresh project with no plugins should be unhealthy"
    );
    for check in checks {
        assert!(check.get("id").and_then(Value::as_str).is_some());
        assert!(check.get("status").and_then(Value::as_str).is_some());
        assert!(check.pointer("/remediation/id").and_then(Value::as_str).is_some());
        assert!(
            check.pointer("/remediation/available").and_then(Value::as_bool).is_some(),
            "remediation availability should be included"
        );
    }

    let (fixed, _fixed_exit) = harness.run_json_stdout_with_exit(&["doctor", "--fix"])?;
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
    std::fs::write(harness.project_root().join(".animus"), "not a directory")?;

    let (doctor, _exit) = harness.run_json_stdout_with_exit(&["doctor"])?;
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

    let (fixed, _fixed_exit) = harness.run_json_stdout_with_exit(&["doctor", "--fix"])?;
    assert_eq!(fixed.pointer("/data/fix/applied").and_then(Value::as_bool), Some(false));
    let actions = fixed.pointer("/data/fix/actions").and_then(Value::as_array).expect("fix actions should be an array");
    // v0.4.13 D2: doctor emits more action ids now (chmod_plugin_binaries,
    // remove_stale_locks, …) — assert the legacy actions are still present
    // without pinning the total count.
    assert!(actions.len() >= 2, "fix actions should include legacy actions plus new safe fixes");
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
    assert!(harness.project_root().join(".animus").is_file());

    Ok(())
}
