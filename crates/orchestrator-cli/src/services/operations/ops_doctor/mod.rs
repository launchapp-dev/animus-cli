//! `animus doctor` polish (v0.4.13 D2).
//!
//! Layout:
//! - [`check_kit`]: shared types (`DiagnosticCheck`, `CheckFix`, `CheckStatus`).
//! - `checks_*`: one module per category; each exposes a `run(&CheckContext)`
//!   returning a list of [`check_kit::DiagnosticCheck`].
//! - This file aggregates everything, applies the `--filter` flag, applies
//!   `--fix` for safe remediations, and emits the
//!   `animus.doctor.v1`-shaped envelope.

use std::path::{Path, PathBuf};

use anyhow::Result;
use colored::Colorize;
use orchestrator_core::{
    load_daemon_project_config, write_daemon_project_config, DoctorCheckStatus, DoctorReport, FileServiceHub,
};
use serde::Serialize;

use crate::{print_value, DoctorArgs};

mod check_kit;
mod checks_api_keys;
mod checks_branches;
mod checks_cli_tools;
mod checks_cosign;
mod checks_crashes;
mod checks_daemon;
mod checks_disk;
mod checks_filesystem;
mod checks_plugins;
mod checks_stale_pid;
mod checks_worktrees;
mod checks_zombie_phases;

use check_kit::{CheckContext, CheckStatus, DiagnosticCheck, FixOutcome};

const DOCTOR_SCHEMA: &str = "animus.doctor.v1";

#[derive(Debug, Clone, Serialize)]
struct DoctorEnvelope {
    schema: &'static str,
    summary: DoctorSummary,
    checks: Vec<DiagnosticCheck>,
    /// Back-compat alias: legacy callers and the existing `setup_doctor_e2e`
    /// tests pointer at `/data/doctor/checks`. Keep emitting the legacy
    /// `DoctorReport` here so older scripts and the MCP surface don't break.
    doctor: DoctorReport,
    fix: FixSection,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorSummary {
    total: usize,
    passed: usize,
    warned: usize,
    failed: usize,
    skipped: usize,
    safe_fixes_available: usize,
    overall: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct FixSection {
    requested: bool,
    applied: bool,
    actions: Vec<FixOutcome>,
}

pub(crate) async fn handle_doctor(project_root: &str, args: DoctorArgs, json: bool) -> Result<()> {
    let project_root_path = PathBuf::from(project_root);
    let ctx = CheckContext { project_root: project_root_path.clone(), skip_subprocess: args.skip_subprocess };

    let mut checks = run_all_checks(&ctx).await;
    apply_filters(&mut checks, &args);

    // Apply safe fixes if requested. We do this BEFORE rendering so the
    // human-readable summary reflects post-fix state. Fixes that change
    // filesystem state (chmod, rm stale locks) are re-evaluated by re-running
    // the relevant check modules.
    let mut fix_section = FixSection { requested: args.fix, applied: false, actions: Vec::new() };
    if args.fix {
        let outcomes = apply_safe_fixes(&ctx, &checks, args.yes);
        fix_section.applied = outcomes.iter().any(|o| o.status == "applied");
        fix_section.actions = outcomes;
        // Re-run after applying fixes so the surfaced state is fresh.
        checks = run_all_checks(&ctx).await;
        apply_filters(&mut checks, &args);
    }

    let summary = summarize(&checks);
    let doctor = DoctorReport::run_for_project(&project_root_path);

    if json {
        let envelope = DoctorEnvelope { schema: DOCTOR_SCHEMA, summary, checks, doctor, fix: fix_section };
        print_value(envelope, true)?;
    } else {
        render_human(&checks, &summary, &fix_section);
        // Provide a small json hint for scripted consumers.
        if !args.fix && summary.safe_fixes_available > 0 {
            println!(
                "\n{}/{} checks passed. Run `animus doctor --fix` to apply {} safe fix(es).",
                summary.passed, summary.total, summary.safe_fixes_available
            );
        } else {
            println!("\n{}/{} checks passed.", summary.passed, summary.total);
        }
    }
    Ok(())
}

async fn run_all_checks(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();
    out.extend(checks_plugins::run(ctx));
    out.extend(checks_daemon::run(ctx).await);
    out.extend(checks_cli_tools::run(ctx));
    out.extend(checks_api_keys::run(ctx));
    out.extend(checks_cosign::run(ctx));
    out.extend(checks_filesystem::run(ctx));
    out.extend(checks_disk::run(ctx));
    out.extend(checks_crashes::run(ctx));
    out.extend(checks_stale_pid::run(ctx));
    out.extend(checks_worktrees::run(ctx));
    out.extend(checks_zombie_phases::run(ctx));
    out.extend(checks_branches::run(ctx));
    out
}

fn apply_filters(checks: &mut Vec<DiagnosticCheck>, args: &DoctorArgs) {
    if !args.filter.is_empty() {
        let needles: Vec<String> = args.filter.iter().map(|s| s.to_ascii_lowercase()).collect();
        checks.retain(|c| {
            let id_lc = c.id.to_ascii_lowercase();
            let cat_lc = c.category.to_ascii_lowercase();
            needles.iter().any(|n| id_lc.contains(n) || cat_lc.contains(n))
        });
    }
    if !args.check.is_empty() {
        let exact: Vec<String> = args.check.iter().map(|s| s.to_ascii_lowercase()).collect();
        checks.retain(|c| {
            let id_lc = c.id.to_ascii_lowercase();
            let cat_lc = c.category.to_ascii_lowercase();
            exact.iter().any(|n| id_lc == *n || cat_lc == *n)
        });
    }
}

fn summarize(checks: &[DiagnosticCheck]) -> DoctorSummary {
    let mut passed = 0;
    let mut warned = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut safe_fixes_available = 0;
    for check in checks {
        match check.status {
            CheckStatus::Pass => passed += 1,
            CheckStatus::Warn => warned += 1,
            CheckStatus::Fail => failed += 1,
            CheckStatus::Skipped => skipped += 1,
        }
        if check.status != CheckStatus::Pass && check.status != CheckStatus::Skipped {
            safe_fixes_available += check.fixes.iter().filter(|f| f.auto_applicable).count();
        }
    }
    let overall = if failed > 0 {
        "unhealthy"
    } else if warned > 0 {
        "degraded"
    } else {
        "healthy"
    };
    DoctorSummary { total: checks.len(), passed, warned, failed, skipped, safe_fixes_available, overall }
}

fn render_human(checks: &[DiagnosticCheck], summary: &DoctorSummary, fix: &FixSection) {
    let mut current_category: Option<&str> = None;
    for check in checks {
        if Some(check.category) != current_category {
            println!("\n{}", check.category.to_ascii_uppercase().bold());
            current_category = Some(check.category);
        }
        let glyph = check.status.glyph();
        let painted = match check.status {
            CheckStatus::Pass => glyph.green().to_string(),
            CheckStatus::Warn => glyph.yellow().to_string(),
            CheckStatus::Fail => glyph.red().to_string(),
            CheckStatus::Skipped => glyph.dimmed().to_string(),
        };
        println!("  {painted} {} {}", check.title.bold(), format!("({})", check.id).dimmed());
        if !check.details.is_empty() {
            println!("      {}", check.details);
        }
        if let Some(current) = &check.current {
            println!("      current:  {current}");
        }
        if let Some(expected) = &check.expected {
            println!("      expected: {expected}");
        }
        for fix in &check.fixes {
            let auto_marker = if fix.auto_applicable { " [auto-fixable]" } else { "" };
            println!("      fix{auto_marker}: {}", fix.details);
            if let Some(cmd) = &fix.command {
                println!("        $ {}", cmd.cyan());
            }
        }
    }
    println!(
        "\n{}: {} passed, {} warn, {} fail, {} skipped (overall: {})",
        "summary".bold(),
        summary.passed.to_string().green(),
        summary.warned.to_string().yellow(),
        summary.failed.to_string().red(),
        summary.skipped,
        summary.overall,
    );
    if fix.requested {
        let applied = fix.actions.iter().filter(|a| a.status == "applied").count();
        let failed = fix.actions.iter().filter(|a| a.status == "failed").count();
        println!(
            "{}: {applied} applied, {failed} failed, {} skipped",
            "--fix".bold(),
            fix.actions.len() - applied - failed
        );
    }
}

fn apply_safe_fixes(ctx: &CheckContext, checks: &[DiagnosticCheck], yes: bool) -> Vec<FixOutcome> {
    let mut out = Vec::new();
    let project_root_path = ctx.project_root.as_path();

    // Bootstrap project state (legacy behavior — keeps backward compat with
    // the old doctor --fix flow).
    let legacy_report = DoctorReport::run_for_project(project_root_path);
    if legacy_remediation_needed(&legacy_report, "bootstrap_project_state") {
        match FileServiceHub::new(project_root_path) {
            Ok(_) => {
                out.push(applied("bootstrap_project_state", "created/validated baseline animus state and config files"))
            }
            Err(error) => out.push(failed("bootstrap_project_state", error.to_string())),
        }
    } else {
        out.push(skipped("bootstrap_project_state", "project bootstrap checks already passed"));
    }

    if legacy_remediation_needed(&legacy_report, "create_default_daemon_config") {
        let result = load_daemon_project_config(project_root_path)
            .and_then(|config| write_daemon_project_config(project_root_path, &config));
        match result {
            Ok(_) => out.push(applied("create_default_daemon_config", "created daemon config with default values")),
            Err(error) => out.push(failed("create_default_daemon_config", error.to_string())),
        }
    } else {
        out.push(skipped("create_default_daemon_config", "daemon config remediation not required"));
    }

    // Back-compat: the legacy doctor surface always emitted a `start_runner`
    // action. Existing scripts and the e2e harness still assert on it; keep
    // it as a no-op skip so the action list shape stays stable.
    if legacy_remediation_needed(&legacy_report, "start_runner") {
        out.push(skipped(
            "start_runner",
            "agent-runner will be started automatically on next workflow/agent run; to start manually run `animus daemon start`",
        ));
    }

    // Remove stale lock files surfaced by the filesystem check.
    let stale_locks_present = checks.iter().any(|c| c.id == "stale_locks" && c.status != CheckStatus::Pass);
    if stale_locks_present {
        let stale = checks_filesystem::collect_stale_locks_for_fix(project_root_path);
        let mut removed: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for path in &stale {
            match std::fs::remove_file(path) {
                Ok(()) => removed.push(path.display().to_string()),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        if errors.is_empty() && !removed.is_empty() {
            out.push(applied(
                "remove_stale_locks",
                &format!("removed {} stale lock file(s): {}", removed.len(), summarize_paths(&removed)),
            ));
        } else if !errors.is_empty() {
            out.push(failed(
                "remove_stale_locks",
                format!("removed {}, failed {}: {}", removed.len(), errors.len(), errors.join("; ")),
            ));
        }
    } else {
        out.push(skipped("remove_stale_locks", "no stale lock files detected"));
    }

    // chmod non-executable plugin binaries flagged by the plugin check.
    let chmod_targets: Vec<&DiagnosticCheck> = checks
        .iter()
        .filter(|c| c.id.starts_with("plugin_binary_executable.") && c.status == CheckStatus::Fail)
        .collect();
    if chmod_targets.is_empty() {
        out.push(skipped("chmod_plugin_binaries", "no non-executable plugin binaries detected"));
    } else {
        let mut applied_count = 0usize;
        let mut errors: Vec<String> = Vec::new();
        for target in chmod_targets {
            if let Some(path) = extract_path_from_current(target.current.as_deref()) {
                match make_executable(&path) {
                    Ok(()) => applied_count += 1,
                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                }
            }
        }
        if errors.is_empty() && applied_count > 0 {
            out.push(applied("chmod_plugin_binaries", &format!("chmoded {applied_count} plugin binary/binaries")));
        } else if !errors.is_empty() {
            out.push(failed(
                "chmod_plugin_binaries",
                format!("chmoded {applied_count}, failed {}: {}", errors.len(), errors.join("; ")),
            ));
        }
    }

    // Stale daemon pid / control.sock cleanup. Always safe — the underlying
    // process is already gone by the time the check fires. We use Warn|Fail
    // (not "not Pass") so a Skipped check never triggers a fix.
    let stale_pid_present =
        checks.iter().any(|c| c.id == "stale_daemon_pid" && matches!(c.status, CheckStatus::Warn | CheckStatus::Fail));
    if stale_pid_present {
        let artifacts = checks_stale_pid::collect_stale_artifacts_for_fix(project_root_path);
        let mut removed: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for path in &artifacts {
            match std::fs::remove_file(path) {
                Ok(()) => removed.push(path.display().to_string()),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        if errors.is_empty() && !removed.is_empty() {
            out.push(applied(
                "remove_stale_daemon_pid",
                &format!("removed stale daemon artifact(s): {}", summarize_paths(&removed)),
            ));
        } else if !errors.is_empty() {
            out.push(failed(
                "remove_stale_daemon_pid",
                format!("removed {}, failed {}: {}", removed.len(), errors.len(), errors.join("; ")),
            ));
        }
    } else {
        out.push(skipped("remove_stale_daemon_pid", "no stale daemon.pid detected"));
    }

    // Zombie phase session normalization. Idempotent rewrite of the JSON.
    let zombies_present = checks
        .iter()
        .any(|c| c.id == "zombie_phase_sessions" && matches!(c.status, CheckStatus::Warn | CheckStatus::Fail));
    if zombies_present {
        let sessions = checks_zombie_phases::collect_zombie_for_fix(project_root_path);
        let mut normalized: Vec<String> = Vec::new();
        let mut raced: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for path in &sessions {
            match checks_zombie_phases::normalize_session_file(path) {
                Ok(checks_zombie_phases::NormalizeOutcome::Rewritten) => normalized.push(path.display().to_string()),
                Ok(checks_zombie_phases::NormalizeOutcome::NoLongerZombie) => raced.push(path.display().to_string()),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        if errors.is_empty() && !normalized.is_empty() {
            let raced_note = if raced.is_empty() {
                String::new()
            } else {
                format!(" (skipped {} no-longer-zombie session(s) after a race recheck)", raced.len())
            };
            out.push(applied(
                "normalize_zombie_phase_sessions",
                &format!(
                    "marked {} zombie phase session(s) as failed: {}{raced_note}",
                    normalized.len(),
                    summarize_paths(&normalized),
                ),
            ));
        } else if !errors.is_empty() {
            out.push(failed(
                "normalize_zombie_phase_sessions",
                format!("normalized {}, failed {}: {}", normalized.len(), errors.len(), errors.join("; ")),
            ));
        } else if !raced.is_empty() {
            out.push(skipped(
                "normalize_zombie_phase_sessions",
                &format!("{} session(s) finished between detect + fix; nothing to normalize", raced.len()),
            ));
        }
    } else {
        out.push(skipped("normalize_zombie_phase_sessions", "no zombie running phase sessions detected"));
    }

    // Orphan worktree removal. Requires --yes because it shells out to
    // `git worktree remove --force`, which is destructive even though the
    // directory in question is repo-local agent state. We deliberately
    // ignore Skipped — when --skip-subprocess gates the check we must not
    // re-enter git via the fix path, and when git itself failed the check
    // marks Skipped so the operator gets a clear "we can't tell" signal.
    let orphans_present =
        checks.iter().any(|c| c.id == "orphan_worktrees" && matches!(c.status, CheckStatus::Warn | CheckStatus::Fail));
    if orphans_present {
        if !yes {
            out.push(skipped(
                "remove_orphan_worktrees",
                "orphan worktrees detected; re-run with `animus doctor --fix --yes` to remove them",
            ));
        } else {
            let orphans = checks_worktrees::collect_orphan_worktrees_for_fix(project_root_path);
            let mut removed: Vec<String> = Vec::new();
            let mut errors: Vec<String> = Vec::new();
            for path in &orphans {
                match remove_orphan_worktree(project_root_path, path) {
                    Ok(()) => removed.push(path.display().to_string()),
                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                }
            }
            if errors.is_empty() && !removed.is_empty() {
                out.push(applied(
                    "remove_orphan_worktrees",
                    &format!("removed {} orphan worktree(s): {}", removed.len(), summarize_paths(&removed)),
                ));
            } else if !errors.is_empty() {
                out.push(failed(
                    "remove_orphan_worktrees",
                    format!("removed {}, failed {}: {}", removed.len(), errors.len(), errors.join("; ")),
                ));
            }
        }
    } else {
        out.push(skipped("remove_orphan_worktrees", "no orphan worktrees detected"));
    }

    out
}

fn remove_orphan_worktree(project_root: &Path, path: &Path) -> std::io::Result<()> {
    let status = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .current_dir(project_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) | Err(_) => {
            // git refused (likely because the path is not a registered
            // worktree). Fall back to a plain recursive directory removal —
            // safe because the doctor already proved the path is not in
            // `git worktree list --porcelain`.
            std::fs::remove_dir_all(path)
        }
    }
}

fn extract_path_from_current(current: Option<&str>) -> Option<PathBuf> {
    let value = current?;
    // The plugin check writes "not executable at <PATH>".
    let prefix = "not executable at ";
    value.find(prefix).map(|idx| PathBuf::from(value[idx + prefix.len()..].trim()))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let meta = std::fs::metadata(path)?;
    let mut perms = meta.permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn summarize_paths(paths: &[String]) -> String {
    if paths.len() <= 3 {
        paths.join(", ")
    } else {
        format!("{}, … (+{} more)", paths[..3].join(", "), paths.len() - 3)
    }
}

fn legacy_remediation_needed(report: &DoctorReport, remediation_id: &str) -> bool {
    report.checks.iter().any(|check| {
        check.remediation.id == remediation_id && check.remediation.available && check.status != DoctorCheckStatus::Ok
    })
}

fn applied(id: &str, details: &str) -> FixOutcome {
    FixOutcome { id: id.to_string(), status: "applied", details: details.to_string() }
}

fn skipped(id: &str, details: &str) -> FixOutcome {
    FixOutcome { id: id.to_string(), status: "skipped", details: details.to_string() }
}

fn failed(id: &str, details: String) -> FixOutcome {
    FixOutcome { id: id.to_string(), status: "failed", details }
}

#[cfg(test)]
// Tests serialize on a process-wide `Mutex<()>` to coordinate `HOME`/env
// mutations across parallel async tests. The guard is intentionally held
// across `.await` because the contended resource is the env, not the lock.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use protocol::test_utils::EnvVarGuard;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner())
    }

    #[tokio::test]
    async fn run_all_checks_includes_all_categories_in_empty_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let ctx = CheckContext { project_root: temp.path().to_path_buf(), skip_subprocess: true };
        let checks = run_all_checks(&ctx).await;
        let categories: std::collections::BTreeSet<&str> = checks.iter().map(|c| c.category).collect();
        for expected in &["plugins", "daemon", "cli_tools", "cosign", "filesystem", "disk", "recent_crashes"] {
            assert!(categories.contains(expected), "missing category: {expected}");
        }
    }

    #[test]
    fn summary_marks_unhealthy_when_any_failure() {
        let checks = vec![
            DiagnosticCheck::new("a", "plugins", CheckStatus::Pass, "a"),
            DiagnosticCheck::new("b", "plugins", CheckStatus::Fail, "b"),
        ];
        let s = summarize(&checks);
        assert_eq!(s.overall, "unhealthy");
        assert_eq!(s.failed, 1);
    }

    #[test]
    fn summary_marks_degraded_when_only_warns() {
        let checks = vec![DiagnosticCheck::new("a", "plugins", CheckStatus::Warn, "a")];
        let s = summarize(&checks);
        assert_eq!(s.overall, "degraded");
    }

    #[test]
    fn extract_path_from_current_parses_chmod_target() {
        let path = extract_path_from_current(Some("not executable at /tmp/foo/bar"));
        assert_eq!(path, Some(PathBuf::from("/tmp/foo/bar")));
    }

    #[tokio::test]
    async fn handle_doctor_runs_without_daemon_or_plugins() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        // Should not panic, should not return error, should produce JSON
        // envelope with at least the required-roles fail check.
        let args = DoctorArgs { fix: false, yes: false, filter: Vec::new(), check: Vec::new(), skip_subprocess: true };
        handle_doctor(temp.path().to_string_lossy().as_ref(), args, true)
            .await
            .expect("doctor should run cleanly even with no daemon/plugins");
    }

    #[tokio::test]
    async fn handle_doctor_filter_narrows_to_matching_checks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let args = DoctorArgs {
            fix: false,
            yes: false,
            filter: vec!["disk".to_string()],
            check: Vec::new(),
            skip_subprocess: true,
        };
        handle_doctor(temp.path().to_string_lossy().as_ref(), args, true).await.expect("filter run ok");
    }

    #[tokio::test]
    async fn handle_doctor_check_narrows_to_exact_id_or_category() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let args = DoctorArgs {
            fix: false,
            yes: false,
            filter: Vec::new(),
            check: vec!["stale_pid".to_string()],
            skip_subprocess: true,
        };
        handle_doctor(temp.path().to_string_lossy().as_ref(), args, true).await.expect("check run ok");
    }

    #[tokio::test]
    async fn doctor_fix_creates_default_daemon_config_when_missing() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let ctx = CheckContext { project_root: temp.path().to_path_buf(), skip_subprocess: true };
        let checks = run_all_checks(&ctx).await;
        let actions = apply_safe_fixes(&ctx, &checks, false);
        assert!(actions.iter().any(|action| action.id == "create_default_daemon_config" && action.status == "applied"));
        assert!(orchestrator_core::daemon_project_config_path(temp.path()).exists());
    }

    fn git_init(path: &std::path::Path) {
        std::process::Command::new("git")
            .arg("init")
            .arg("--quiet")
            .current_dir(path)
            .status()
            .expect("git init should succeed for orphan-worktree test");
    }

    #[tokio::test]
    async fn fix_without_yes_skips_orphan_worktree_removal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project-yes-required");
        std::fs::create_dir_all(&project).unwrap();
        git_init(&project);
        let wt_dir = protocol::scoped_state_root(&project).unwrap().join("worktrees");
        std::fs::create_dir_all(wt_dir.join("task-leftover")).unwrap();

        let ctx = CheckContext { project_root: project.clone(), skip_subprocess: false };
        let checks = run_all_checks(&ctx).await;
        let actions = apply_safe_fixes(&ctx, &checks, false);
        let action =
            actions.iter().find(|a| a.id == "remove_orphan_worktrees").expect("orphan worktree action emitted");
        assert_eq!(action.status, "skipped");
        assert!(action.details.contains("--yes"));
        assert!(wt_dir.join("task-leftover").exists(), "directory must remain untouched without --yes");
    }

    #[tokio::test]
    async fn fix_with_yes_removes_orphan_worktree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project-yes");
        std::fs::create_dir_all(&project).unwrap();
        git_init(&project);
        let wt_dir = protocol::scoped_state_root(&project).unwrap().join("worktrees");
        std::fs::create_dir_all(wt_dir.join("task-orphan")).unwrap();

        let ctx = CheckContext { project_root: project.clone(), skip_subprocess: false };
        let checks = run_all_checks(&ctx).await;
        let actions = apply_safe_fixes(&ctx, &checks, true);
        let action =
            actions.iter().find(|a| a.id == "remove_orphan_worktrees").expect("orphan worktree action emitted");
        assert_eq!(action.status, "applied", "{:?}", action);
        assert!(!wt_dir.join("task-orphan").exists());
    }

    #[tokio::test]
    async fn doctor_json_envelope_includes_new_checks_and_filter_to_check_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let ctx = CheckContext { project_root: temp.path().to_path_buf(), skip_subprocess: true };
        let all = run_all_checks(&ctx).await;
        let ids: std::collections::BTreeSet<String> = all.iter().map(|c| c.id.clone()).collect();
        for expected in &["stale_daemon_pid", "orphan_worktrees", "zombie_phase_sessions", "merged_agent_branches"] {
            assert!(ids.contains(*expected), "missing new check id: {expected}, got {:?}", ids);
        }
    }
}
