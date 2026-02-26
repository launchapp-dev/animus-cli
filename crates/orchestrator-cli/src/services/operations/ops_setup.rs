use std::io::{self, IsTerminal, Write};
use std::path::Path;

use anyhow::{anyhow, Result};
use orchestrator_core::{
    daemon_project_config_path, load_daemon_project_config, update_daemon_project_config,
    DaemonProjectConfig, DaemonProjectConfigPatch, DoctorCheckStatus, DoctorReport,
};
use serde::Serialize;

use super::ops_doctor::{apply_doctor_fixes, DoctorFixAction};
use crate::{print_value, SetupArgs};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SetupMode {
    Guided,
    NonInteractive,
}

#[derive(Debug, Clone, Serialize)]
struct SetupFieldPlan {
    field: String,
    before: bool,
    after: bool,
    changed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SetupBlockedItem {
    check_id: String,
    details: String,
    remediation: String,
}

#[derive(Debug, Clone)]
struct DesiredDaemonConfig {
    auto_merge_enabled: bool,
    auto_pr_enabled: bool,
    auto_commit_before_merge: bool,
}

pub(crate) async fn handle_setup(args: SetupArgs, project_root: &str, json: bool) -> Result<()> {
    let project_root_path = Path::new(project_root);
    let mode = if args.non_interactive {
        SetupMode::NonInteractive
    } else {
        SetupMode::Guided
    };

    let current_config = load_daemon_project_config(project_root_path)?;
    let desired = resolve_desired_config(&args, mode, &current_config)?;
    let daemon_plan = daemon_field_plan(&current_config, &desired);

    let doctor_before = DoctorReport::run_for_project(project_root_path);
    let blocked_items = collect_blocked_items(&doctor_before);
    let doctor_summary = serde_json::json!({
        "result": doctor_before.result,
        "ok": count_checks(&doctor_before, DoctorCheckStatus::Ok),
        "warn": count_checks(&doctor_before, DoctorCheckStatus::Warn),
        "fail": count_checks(&doctor_before, DoctorCheckStatus::Fail),
    });

    if args.plan {
        return print_value(
            serde_json::json!({
                "stage": "plan",
                "mode": mode,
                "environment": {
                    "project_root": project_root,
                    "doctor": doctor_summary,
                    "daemon_config_path": daemon_project_config_path(project_root_path).display().to_string(),
                },
                "required_changes": {
                    "daemon_config": daemon_plan,
                },
                "blocked_items": blocked_items,
                "apply": {
                    "applied": false,
                    "changed_domains": [],
                    "unchanged_domains": ["daemon_config"],
                },
            }),
            json,
        );
    }

    let patch = DaemonProjectConfigPatch {
        auto_merge_enabled: Some(desired.auto_merge_enabled),
        auto_pr_enabled: Some(desired.auto_pr_enabled),
        auto_commit_before_merge: Some(desired.auto_commit_before_merge),
    };
    let (_, daemon_config_updated) = update_daemon_project_config(project_root_path, &patch)?;

    let mut doctor_fix_actions: Vec<DoctorFixAction> = Vec::new();
    if args.doctor_fix {
        let report_for_fix = DoctorReport::run_for_project(project_root_path);
        doctor_fix_actions = apply_doctor_fixes(project_root, &report_for_fix);
    }

    let doctor_after = DoctorReport::run_for_project(project_root_path);
    let doctor_fix_applied = doctor_fix_actions
        .iter()
        .any(|action| action.status == "applied");

    let mut changed_domains = Vec::new();
    let mut unchanged_domains = Vec::new();
    if daemon_config_updated {
        changed_domains.push("daemon_config");
    } else {
        unchanged_domains.push("daemon_config");
    }
    if args.doctor_fix {
        if doctor_fix_applied {
            changed_domains.push("doctor_remediation");
        } else {
            unchanged_domains.push("doctor_remediation");
        }
    }

    print_value(
        serde_json::json!({
            "stage": "apply",
            "mode": mode,
            "environment": {
                "project_root": project_root,
                "daemon_config_path": daemon_project_config_path(project_root_path).display().to_string(),
            },
            "required_changes": {
                "daemon_config": daemon_plan,
            },
            "blocked_items": blocked_items,
            "apply": {
                "applied": true,
                "changed_domains": changed_domains,
                "unchanged_domains": unchanged_domains,
                "daemon_config_updated": daemon_config_updated,
            },
            "doctor_before": doctor_before,
            "doctor_after": doctor_after,
            "doctor_fix": {
                "requested": args.doctor_fix,
                "actions": doctor_fix_actions,
            },
        }),
        json,
    )
}

fn resolve_desired_config(
    args: &SetupArgs,
    mode: SetupMode,
    current: &DaemonProjectConfig,
) -> Result<DesiredDaemonConfig> {
    match mode {
        SetupMode::NonInteractive => resolve_non_interactive_desired_config(args),
        SetupMode::Guided => resolve_guided_desired_config(args, current),
    }
}

fn resolve_non_interactive_desired_config(args: &SetupArgs) -> Result<DesiredDaemonConfig> {
    let mut missing = Vec::new();
    if args.auto_merge.is_none() {
        missing.push("--auto-merge");
    }
    if args.auto_pr.is_none() {
        missing.push("--auto-pr");
    }
    if args.auto_commit_before_merge.is_none() {
        missing.push("--auto-commit-before-merge");
    }
    if !missing.is_empty() {
        return Err(anyhow!(
            "missing required non-interactive setup inputs: {}. rerun with explicit values or omit --non-interactive for guided setup",
            missing.join(", ")
        ));
    }

    Ok(DesiredDaemonConfig {
        auto_merge_enabled: args.auto_merge.unwrap_or(false),
        auto_pr_enabled: args.auto_pr.unwrap_or(false),
        auto_commit_before_merge: args.auto_commit_before_merge.unwrap_or(false),
    })
}

fn resolve_guided_desired_config(
    args: &SetupArgs,
    current: &DaemonProjectConfig,
) -> Result<DesiredDaemonConfig> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!(
            "guided setup must be run in an interactive terminal; rerun with --non-interactive and explicit --auto-* values"
        ));
    }

    let auto_merge_enabled = match args.auto_merge {
        Some(value) => value,
        None => prompt_bool(
            "Enable automatic merge after successful daemon workflow runs?",
            current.auto_merge_enabled,
        )?,
    };
    let auto_pr_enabled = match args.auto_pr {
        Some(value) => value,
        None => prompt_bool(
            "Enable automatic pull request creation for daemon workflow runs?",
            current.auto_pr_enabled,
        )?,
    };
    let auto_commit_before_merge = match args.auto_commit_before_merge {
        Some(value) => value,
        None => prompt_bool(
            "Enable automatic commit before merge when worktree is dirty?",
            current.auto_commit_before_merge,
        )?,
    };

    Ok(DesiredDaemonConfig {
        auto_merge_enabled,
        auto_pr_enabled,
        auto_commit_before_merge,
    })
}

fn prompt_bool(prompt: &str, default: bool) -> Result<bool> {
    let mut stdout = io::stdout();
    let mut input = String::new();

    loop {
        input.clear();
        let suffix = if default { "[Y/n]" } else { "[y/N]" };
        print!("{prompt} {suffix}: ");
        stdout.flush()?;

        io::stdin().read_line(&mut input)?;
        match input.trim().to_ascii_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" | "true" | "1" => return Ok(true),
            "n" | "no" | "false" | "0" => return Ok(false),
            _ => {
                println!("Please answer y or n.");
            }
        }
    }
}

fn daemon_field_plan(
    current: &DaemonProjectConfig,
    desired: &DesiredDaemonConfig,
) -> Vec<SetupFieldPlan> {
    vec![
        field_plan(
            "auto_merge_enabled",
            current.auto_merge_enabled,
            desired.auto_merge_enabled,
        ),
        field_plan(
            "auto_pr_enabled",
            current.auto_pr_enabled,
            desired.auto_pr_enabled,
        ),
        field_plan(
            "auto_commit_before_merge",
            current.auto_commit_before_merge,
            desired.auto_commit_before_merge,
        ),
    ]
}

fn field_plan(field: &str, before: bool, after: bool) -> SetupFieldPlan {
    SetupFieldPlan {
        field: field.to_string(),
        before,
        after,
        changed: before != after,
    }
}

fn count_checks(report: &DoctorReport, status: DoctorCheckStatus) -> usize {
    report
        .checks
        .iter()
        .filter(|check| check.status == status)
        .count()
}

fn collect_blocked_items(report: &DoctorReport) -> Vec<SetupBlockedItem> {
    report
        .checks
        .iter()
        .filter(|check| {
            check.status == DoctorCheckStatus::Fail
                || (check.status == DoctorCheckStatus::Warn && !check.remediation.available)
        })
        .map(|check| SetupBlockedItem {
            check_id: check.id.clone(),
            details: check.details.clone(),
            remediation: check.remediation.details.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{DoctorCheck, DoctorCheckResult, DoctorRemediation};

    fn non_interactive_args() -> SetupArgs {
        SetupArgs {
            non_interactive: true,
            plan: true,
            auto_merge: None,
            auto_pr: None,
            auto_commit_before_merge: None,
            doctor_fix: false,
        }
    }

    #[test]
    fn non_interactive_setup_requires_explicit_values() {
        let args = non_interactive_args();
        let error = resolve_non_interactive_desired_config(&args)
            .expect_err("missing non-interactive flags should fail");
        assert!(error
            .to_string()
            .contains("missing required non-interactive setup inputs"));
    }

    #[test]
    fn daemon_field_plan_marks_changed_and_unchanged_values() {
        let current = DaemonProjectConfig::default();
        let desired = DesiredDaemonConfig {
            auto_merge_enabled: false,
            auto_pr_enabled: true,
            auto_commit_before_merge: false,
        };

        let plan = daemon_field_plan(&current, &desired);
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].changed, false);
        assert_eq!(plan[1].changed, true);
        assert_eq!(plan[2].changed, false);
    }

    #[test]
    fn collect_blocked_items_only_returns_actionable_non_ok_checks() {
        let report = DoctorReport {
            result: DoctorCheckResult::Degraded,
            checks: vec![
                DoctorCheck {
                    id: "ok_without_fix".to_string(),
                    status: DoctorCheckStatus::Ok,
                    details: "ok check".to_string(),
                    remediation: DoctorRemediation {
                        id: "manual".to_string(),
                        available: false,
                        details: "manual remediation".to_string(),
                        command: None,
                    },
                },
                DoctorCheck {
                    id: "warn_without_fix".to_string(),
                    status: DoctorCheckStatus::Warn,
                    details: "warn check".to_string(),
                    remediation: DoctorRemediation {
                        id: "manual".to_string(),
                        available: false,
                        details: "manual remediation".to_string(),
                        command: None,
                    },
                },
                DoctorCheck {
                    id: "warn_with_fix".to_string(),
                    status: DoctorCheckStatus::Warn,
                    details: "warn check".to_string(),
                    remediation: DoctorRemediation {
                        id: "auto_fix".to_string(),
                        available: true,
                        details: "automatic remediation".to_string(),
                        command: Some("ao doctor --fix".to_string()),
                    },
                },
                DoctorCheck {
                    id: "fail_with_fix".to_string(),
                    status: DoctorCheckStatus::Fail,
                    details: "fail check".to_string(),
                    remediation: DoctorRemediation {
                        id: "auto_fix".to_string(),
                        available: true,
                        details: "automatic remediation".to_string(),
                        command: Some("ao doctor --fix".to_string()),
                    },
                },
            ],
        };

        let blocked_items = collect_blocked_items(&report);
        assert_eq!(blocked_items.len(), 2);
        assert_eq!(blocked_items[0].check_id, "warn_without_fix");
        assert_eq!(blocked_items[1].check_id, "fail_with_fix");
    }
}
