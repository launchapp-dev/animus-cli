//! Orphan worktree detection.
//!
//! Looks for directories under `~/.animus/<repo-scope>/worktrees/` that no
//! longer correspond to an active workflow run. We treat a worktree as
//! orphan when its directory is not registered with the parent repo via
//! `git worktree list --porcelain`. This catches the common
//! "branch deleted but worktree dir lingers" failure mode without trusting
//! the in-tree workflow registry (which may itself be stale).
//!
//! Removal is auto-applicable only when `--yes` is passed; the spec calls
//! this out as a destructive action that should require consent.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "worktrees";

pub(crate) fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();

    if ctx.skip_subprocess {
        out.push(
            DiagnosticCheck::new("orphan_worktrees", CATEGORY, CheckStatus::Skipped, "Orphan task worktrees")
                .details("skipped because --skip-subprocess is set (orphan detection shells out to git worktree list)"),
        );
        return out;
    }

    let Some(worktrees_dir) = worktrees_dir(&ctx.project_root) else {
        out.push(
            DiagnosticCheck::new("orphan_worktrees", CATEGORY, CheckStatus::Skipped, "Orphan task worktrees")
                .details("scoped state root unavailable (no HOME)"),
        );
        return out;
    };

    if !worktrees_dir.exists() {
        out.push(
            DiagnosticCheck::new("orphan_worktrees", CATEGORY, CheckStatus::Pass, "Orphan task worktrees")
                .details(format!("no worktree directory at {}", worktrees_dir.display())),
        );
        return out;
    }

    let candidates = list_candidate_worktrees(&worktrees_dir);
    if candidates.is_empty() {
        out.push(
            DiagnosticCheck::new("orphan_worktrees", CATEGORY, CheckStatus::Pass, "Orphan task worktrees")
                .details(format!("no worktree subdirectories under {}", worktrees_dir.display())),
        );
        return out;
    }

    let registered = match list_registered_worktrees(&ctx.project_root) {
        Some(list) => list,
        None => {
            out.push(
                DiagnosticCheck::new("orphan_worktrees", CATEGORY, CheckStatus::Skipped, "Orphan task worktrees")
                    .details(format!(
                        "could not query `git worktree list --porcelain` in {} — skipping orphan detection to avoid destructive false positives",
                        ctx.project_root.display(),
                    )),
            );
            return out;
        }
    };
    let orphans: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|candidate| {
            let candidate_canonical = candidate.canonicalize().unwrap_or_else(|_| candidate.clone());
            !registered.iter().any(|reg| {
                let reg_canonical = reg.canonicalize().unwrap_or_else(|_| reg.clone());
                reg_canonical == candidate_canonical
            })
        })
        .collect();

    if orphans.is_empty() {
        out.push(
            DiagnosticCheck::new("orphan_worktrees", CATEGORY, CheckStatus::Pass, "Orphan task worktrees")
                .details(format!("all worktree directories under {} are registered", worktrees_dir.display())),
        );
        return out;
    }

    let preview = orphans.iter().take(3).map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
    out.push(
        DiagnosticCheck::new("orphan_worktrees", CATEGORY, CheckStatus::Warn, "Orphan task worktrees")
            .current(format!("{} orphan worktree(s): {preview}", orphans.len()))
            .expected("each subdirectory of worktrees/ is registered with git".to_string())
            .details(
                "Detection trusts `git worktree list --porcelain`. If a non-terminal task still references one of these paths via `worktree_path`, removal will strand the task — `--fix --yes` is gated behind explicit consent for exactly this reason. Cross-check `animus subject list --kind task` before consenting."
                    .to_string(),
            )
            // NOT auto_applicable: --fix alone is a no-op here; we want
            // `safe_fixes_available` to ignore this so the human hint
            // ("Run `animus doctor --fix` to apply N safe fix(es)") doesn't
            // promise a fix that requires --yes. The corresponding
            // `apply_safe_fixes` arm still actuates when --yes is passed.
            .fix(CheckFix::command(
                "remove_orphan_worktrees",
                "Remove each orphan worktree (requires --yes; doctor will then call `git worktree remove --force`).",
                "animus doctor --fix --yes",
            )),
    );

    out
}

pub(crate) fn collect_orphan_worktrees_for_fix(project_root: &Path) -> Vec<PathBuf> {
    let Some(worktrees_dir) = worktrees_dir(project_root) else {
        return Vec::new();
    };
    if !worktrees_dir.exists() {
        return Vec::new();
    }
    let candidates = list_candidate_worktrees(&worktrees_dir);
    // Fail closed: if git cannot enumerate worktrees we refuse to call
    // anything orphaned. The doctor surface marks the check as skipped in
    // this state so the operator gets a clear signal rather than silent
    // data loss.
    let Some(registered) = list_registered_worktrees(project_root) else {
        return Vec::new();
    };
    candidates
        .into_iter()
        .filter(|candidate| {
            let candidate_canonical = candidate.canonicalize().unwrap_or_else(|_| candidate.clone());
            !registered.iter().any(|reg| {
                let reg_canonical = reg.canonicalize().unwrap_or_else(|_| reg.clone());
                reg_canonical == candidate_canonical
            })
        })
        .collect()
}

fn worktrees_dir(project_root: &Path) -> Option<PathBuf> {
    Some(protocol::scoped_state_root(project_root)?.join("worktrees"))
}

fn list_candidate_worktrees(worktrees_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(worktrees_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else { continue };
        if !file_type.is_dir() {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with('.')).unwrap_or(true) {
            continue;
        }
        out.push(path);
    }
    out
}

/// Returns the set of worktrees registered with git.
///
/// `None` means "we could not ask git" — either the binary is missing,
/// the project root is not a real repository, or git itself errored.
/// In that case the orphan detection must skip rather than treat every
/// candidate dir as orphan (which would lead `--fix --yes` to delete
/// legitimately-registered worktrees).
fn list_registered_worktrees(project_root: &Path) -> Option<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            out.push(PathBuf::from(path.trim()));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::test_utils::EnvVarGuard;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn early_skips_when_skip_subprocess_flag_set() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project-skip");
        std::fs::create_dir_all(&project).unwrap();
        let wt_dir = protocol::scoped_state_root(&project).unwrap().join("worktrees");
        std::fs::create_dir_all(wt_dir.join("task-orphan")).unwrap();

        let ctx = CheckContext { project_root: project.clone(), skip_subprocess: true };
        let checks = run(&ctx);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, CheckStatus::Skipped, "{:?}", checks[0]);
        assert!(checks[0].details.contains("--skip-subprocess"));
    }

    #[test]
    fn skips_when_git_unavailable_so_we_never_delete_registered_worktrees() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let wt_dir = protocol::scoped_state_root(&project).unwrap().join("worktrees");
        std::fs::create_dir_all(wt_dir.join("task-orphan")).unwrap();

        let ctx = CheckContext { project_root: project.clone(), skip_subprocess: false };
        let checks = run(&ctx);
        assert_eq!(checks.len(), 1);
        // `project` is not a git repo, so `git worktree list --porcelain`
        // returns non-zero. We must skip rather than warn — surfacing it
        // as orphan would let --fix --yes delete every candidate dir.
        assert_eq!(checks[0].status, CheckStatus::Skipped, "{:?}", checks[0]);
        let orphans = collect_orphan_worktrees_for_fix(&project);
        assert!(orphans.is_empty(), "must not surface orphans without registered list");
    }

    #[test]
    fn passes_when_worktrees_dir_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project2");
        std::fs::create_dir_all(&project).unwrap();
        let ctx = CheckContext { project_root: project.clone(), skip_subprocess: false };
        let checks = run(&ctx);
        assert_eq!(checks[0].status, CheckStatus::Pass);
    }

    #[test]
    fn detects_orphan_when_git_repo_lists_no_worktrees() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project-git");
        std::fs::create_dir_all(&project).unwrap();
        // Init a real git repo so `git worktree list --porcelain` succeeds.
        // The single registered worktree is `project` itself; anything we
        // create under `<scope>/worktrees/` is therefore unambiguously
        // orphan.
        std::process::Command::new("git").arg("init").arg("--quiet").current_dir(&project).status().expect("git init");
        let wt_dir = protocol::scoped_state_root(&project).unwrap().join("worktrees");
        std::fs::create_dir_all(wt_dir.join("task-orphan")).unwrap();

        let ctx = CheckContext { project_root: project.clone(), skip_subprocess: false };
        let checks = run(&ctx);
        assert_eq!(checks[0].status, CheckStatus::Warn, "{:?}", checks[0]);
        let orphans = collect_orphan_worktrees_for_fix(&project);
        assert_eq!(orphans.len(), 1);
        assert!(orphans[0].ends_with("task-orphan"));
    }
}
