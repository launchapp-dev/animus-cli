//! Filesystem perms + stale lock detection.
//!
//! Writability is probed via a real temp-file create-and-delete in the
//! target directory so we don't trust mode bits that lie on networked
//! mounts. Stale locks are any `*.lock` file older than 1h that we can
//! statvfs / `metadata().modified()` on.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "filesystem";
const STALE_LOCK_AGE: Duration = Duration::from_hours(1);

pub(crate) fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();

    let animus_home = animus_home_dir();
    out.push(probe_writable("animus_home_writable", "~/.animus writable", &animus_home));

    let project_ao = ctx.project_root.join(".animus");
    if project_ao.exists() {
        out.push(probe_writable("project_animus_writable", ".animus/ writable", &project_ao));
    } else {
        out.push(
            DiagnosticCheck::new("project_animus_writable", CATEGORY, CheckStatus::Skipped, ".animus/ writable")
                .details(format!("project does not have a .animus dir at {}", project_ao.display())),
        );
    }

    let stale = find_stale_locks(&animus_home);
    if stale.is_empty() {
        out.push(
            DiagnosticCheck::new("stale_locks", CATEGORY, CheckStatus::Pass, "Stale lock files")
                .details(format!("no .lock files older than {}m under ~/.animus", STALE_LOCK_AGE.as_secs() / 60)),
        );
    } else {
        let count = stale.len();
        let preview = stale.iter().take(3).map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
        out.push(
            DiagnosticCheck::new("stale_locks", CATEGORY, CheckStatus::Warn, "Stale lock files")
                .current(format!("{count} stale .lock file(s): {preview}"))
                .expected(format!("no .lock files older than {}m", STALE_LOCK_AGE.as_secs() / 60))
                .fix(CheckFix::auto(
                    "remove_stale_locks",
                    &format!("Remove {count} stale lock file(s)."),
                    "animus doctor --fix",
                )),
        );
    }

    out
}

pub(crate) fn collect_stale_locks_for_fix(project_root: &Path) -> Vec<PathBuf> {
    let _ = project_root; // currently we only scan ~/.animus
    find_stale_locks(&animus_home_dir())
}

fn probe_writable(id: &str, title: &str, dir: &Path) -> DiagnosticCheck {
    if !dir.exists() {
        return DiagnosticCheck::new(id, CATEGORY, CheckStatus::Warn, title)
            .current(format!("missing at {}", dir.display()))
            .expected("directory exists and is writable".to_string())
            .fix(CheckFix::auto(
                "create_dir",
                &format!("Create {} so animus can write state.", dir.display()),
                &format!("mkdir -p {}", dir.display()),
            ));
    }
    if !dir.is_dir() {
        return DiagnosticCheck::new(id, CATEGORY, CheckStatus::Fail, title)
            .current(format!("{} is not a directory", dir.display()))
            .expected("directory".to_string())
            .fix(CheckFix::manual(
                "replace_non_dir",
                &format!("Move or remove the non-directory at {} and re-run.", dir.display()),
            ));
    }
    let probe = dir.join(".animus-doctor-write-probe");
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            DiagnosticCheck::new(id, CATEGORY, CheckStatus::Pass, title)
                .details(format!("{} is writable", dir.display()))
        }
        Err(e) => DiagnosticCheck::new(id, CATEGORY, CheckStatus::Fail, title)
            .current(format!("write to {} failed: {e}", dir.display()))
            .expected("user has +rw on the directory".to_string())
            .fix(CheckFix::manual(
                "chmod_dir",
                &format!("Run `chmod u+rwx {}` (or correct ownership).", dir.display()),
            )),
    }
}

fn find_stale_locks(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let now = SystemTime::now();
    walk_lock_files(root, &now, &mut out, 0);
    out
}

fn walk_lock_files(dir: &Path, now: &SystemTime, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return; // bound recursion; doctor must never wedge on a deep tree
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            walk_lock_files(&path, now, out, depth + 1);
            continue;
        }
        if file_type.is_file() && path.extension().map(|e| e == "lock").unwrap_or(false) {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if now.duration_since(modified).map(|d| d >= STALE_LOCK_AGE).unwrap_or(false) {
                        out.push(path);
                    }
                }
            }
        }
    }
}

fn animus_home_dir() -> PathBuf {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".animus")).unwrap_or_else(|| PathBuf::from(".animus"))
}
