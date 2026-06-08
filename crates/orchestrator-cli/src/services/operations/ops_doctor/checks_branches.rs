//! Merged-and-done branch detection (report-only).
//!
//! Per CLAUDE.md safety protocol, doctor never deletes branches. This
//! check enumerates local branches whose names match the agent-task
//! conventions (`animus/task/*`, `animus/*`, `agent/v*`) and that have
//! been merged into the configured upstream. Operators get a paste-ready
//! `git branch -d` list — actuating it stays manual.

use std::path::Path;
use std::process::Command;

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "branches";

pub(crate) fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();

    if ctx.skip_subprocess {
        out.push(
            DiagnosticCheck::new("merged_agent_branches", CATEGORY, CheckStatus::Skipped, "Merged agent branches")
                .details("skipped because --skip-subprocess is set (branch enumeration shells out to git)"),
        );
        return out;
    }

    if !ctx.project_root.join(".git").exists() {
        out.push(
            DiagnosticCheck::new("merged_agent_branches", CATEGORY, CheckStatus::Skipped, "Merged agent branches")
                .details("project is not a git repository"),
        );
        return out;
    }

    let upstream = detect_default_upstream(&ctx.project_root);
    let Some(upstream) = upstream else {
        out.push(
            DiagnosticCheck::new("merged_agent_branches", CATEGORY, CheckStatus::Skipped, "Merged agent branches")
                .details("could not detect default upstream (origin/main or origin/master)"),
        );
        return out;
    };

    let merged = list_merged_agent_branches(&ctx.project_root, &upstream);
    if merged.is_empty() {
        out.push(
            DiagnosticCheck::new("merged_agent_branches", CATEGORY, CheckStatus::Pass, "Merged agent branches")
                .details(format!("no merged agent branches against {upstream}")),
        );
        return out;
    }

    let preview = merged.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    // Shell-escape the branch name so a pathological branch like
    // `animus/task/foo&bar` (legal per `git check-ref-format`) can't run
    // arbitrary shell when the operator pastes the suggestion.
    let suggested =
        merged.iter().map(|b| format!("git branch -d -- {}", shell_quote(b))).collect::<Vec<_>>().join("; ");

    out.push(
        DiagnosticCheck::new("merged_agent_branches", CATEGORY, CheckStatus::Warn, "Merged agent branches")
            .current(format!("{} merged agent branch(es): {preview}", merged.len()))
            .expected("agent task branches cleaned up after merge".to_string())
            .fix(CheckFix::command(
                "delete_merged_agent_branches",
                "Delete each merged agent branch (manual; doctor never deletes branches automatically).",
                &suggested,
            )),
    );

    out
}

fn detect_default_upstream(project_root: &Path) -> Option<String> {
    for candidate in ["origin/main", "origin/master"] {
        let ok = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", candidate])
            .current_dir(project_root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(candidate.to_string());
        }
    }
    None
}

fn list_merged_agent_branches(project_root: &Path, upstream: &str) -> Vec<String> {
    let output = Command::new("git")
        .args(["branch", "--merged", upstream, "--format=%(refname:short)"])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(output) = output else { return Vec::new() };
    if !output.status.success() {
        return Vec::new();
    }
    let current_branch = current_branch(project_root);
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }
        if Some(name) == current_branch.as_deref() {
            continue;
        }
        if matches_agent_convention(name) {
            out.push(name.to_string());
        }
    }
    out
}

fn current_branch(project_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Wrap a value in single quotes for safe shell paste-back. Embedded
/// single quotes are escaped via the standard `'\''` POSIX trick.
fn shell_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn matches_agent_convention(name: &str) -> bool {
    name.starts_with("animus/task/")
        || name.starts_with("animus/")
        || name.starts_with("agent/v")
        || name.starts_with("agent/task/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_agent_convention_recognizes_expected_patterns() {
        assert!(matches_agent_convention("animus/task/TASK-001"));
        assert!(matches_agent_convention("animus/v0.5.7/foo"));
        assert!(matches_agent_convention("agent/v0.5.7-foo"));
        assert!(matches_agent_convention("agent/task/TASK-002"));
        assert!(!matches_agent_convention("main"));
        assert!(!matches_agent_convention("feature/random"));
    }

    #[test]
    fn skips_non_git_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = CheckContext { project_root: temp.path().to_path_buf(), skip_subprocess: false };
        let checks = run(&ctx);
        assert_eq!(checks[0].status, CheckStatus::Skipped);
    }

    #[test]
    fn skip_subprocess_flag_short_circuits_branches_check() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = CheckContext { project_root: temp.path().to_path_buf(), skip_subprocess: true };
        let checks = run(&ctx);
        assert_eq!(checks[0].status, CheckStatus::Skipped);
        assert!(checks[0].details.contains("--skip-subprocess"));
    }

    #[test]
    fn shell_quote_escapes_metacharacters_and_embedded_single_quotes() {
        assert_eq!(shell_quote("animus/task/foo&bar"), "'animus/task/foo&bar'");
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("with'quote"), "'with'\\''quote'");
    }
}
