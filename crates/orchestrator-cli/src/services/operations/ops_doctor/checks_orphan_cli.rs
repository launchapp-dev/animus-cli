//! Orphaned CLI process check.
//!
//! Absorbed from the deleted `animus runner orphans {detect,cleanup}` verbs:
//! the cli-tracker file under the global config dir records the PID of every
//! provider CLI process spawned per run. The tracker is global (shared across
//! repo scopes), so a live tracked PID cannot be assumed orphaned — it may
//! belong to an in-flight run in another project. The doctor therefore only
//! auto-fixes the unambiguous case: tracker entries whose process already
//! exited are pruned by `animus doctor --fix`. Live tracked PIDs are surfaced
//! with a manual `kill` suggestion so the operator keeps explicit selection
//! before terminating anything.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};

use anyhow::Result;
use fs2::FileExt;
use protocol::process_exists;

use super::super::{read_json_or_default, write_json_pretty};
use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

fn load_cli_tracker() -> Result<HashMap<String, u32>> {
    read_json_or_default(&protocol::cli_tracker_path())
}

fn save_cli_tracker(tracker: &HashMap<String, u32>) -> Result<()> {
    write_json_pretty(&protocol::cli_tracker_path(), tracker)
}

/// Acquire an exclusive file lock on the CLI tracker for atomic read-modify-write.
fn acquire_tracker_lock() -> Result<std::fs::File> {
    let tracker_path = protocol::cli_tracker_path();
    if let Some(parent) = tracker_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = tracker_path.with_extension("lock");
    let lock_file = OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
    lock_file.lock_exclusive()?;
    Ok(lock_file)
}

/// Tracker entries split into `(live, dead)` by process liveness, each a
/// sorted list of `(run_id, pid)` pairs.
#[allow(clippy::type_complexity)]
fn partition_tracker() -> Result<(Vec<(String, u32)>, Vec<(String, u32)>)> {
    let tracker = load_cli_tracker()?;
    let (mut live, mut dead): (Vec<_>, Vec<_>) = tracker.into_iter().partition(|(_, pid)| process_exists(*pid as i32));
    live.sort();
    dead.sort();
    Ok((live, dead))
}

fn list_entries(entries: &[(String, u32)]) -> String {
    let listed =
        entries.iter().take(5).map(|(run_id, pid)| format!("{run_id} (pid {pid})")).collect::<Vec<_>>().join(", ");
    let suffix = if entries.len() > 5 { format!(", … (+{} more)", entries.len() - 5) } else { String::new() };
    format!("{listed}{suffix}")
}

pub(super) fn run(_ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let check = match partition_tracker() {
        Err(err) => {
            DiagnosticCheck::new("orphan_cli_processes", "processes", CheckStatus::Skipped, "Orphaned CLI processes")
                .details(format!("could not read cli-tracker: {err}"))
        }
        Ok((live, dead)) if live.is_empty() && dead.is_empty() => {
            DiagnosticCheck::new("orphan_cli_processes", "processes", CheckStatus::Pass, "Orphaned CLI processes")
                .details("no orphaned CLI processes tracked under the cli-tracker")
        }
        Ok((live, dead)) => {
            let mut details = Vec::new();
            if !live.is_empty() {
                details.push(format!("{} tracked CLI process(es) still running: {}", live.len(), list_entries(&live)));
            }
            if !dead.is_empty() {
                details.push(format!(
                    "{} stale tracker entr(y/ies) for exited process(es): {}",
                    dead.len(),
                    list_entries(&dead)
                ));
            }
            let mut check =
                DiagnosticCheck::new("orphan_cli_processes", "processes", CheckStatus::Warn, "Orphaned CLI processes")
                    .details(details.join("; "))
                    .expected("no tracked CLI processes outliving their runs");
            if !dead.is_empty() {
                check = check.fix(CheckFix::auto(
                    "prune_stale_cli_tracker_entries",
                    "prune cli-tracker entries whose process already exited",
                    "animus doctor --fix",
                ));
            }
            if !live.is_empty() {
                let kill_cmd =
                    format!("kill {}", live.iter().map(|(_, pid)| pid.to_string()).collect::<Vec<_>>().join(" "));
                check = check.fix(CheckFix::command(
                    "kill_orphan_cli_processes",
                    "the cli-tracker is global across projects — verify these runs are not active anywhere, then kill them manually",
                    &kill_cmd,
                ));
            }
            check
        }
    };
    vec![check]
}

/// Prune tracker entries whose process already exited. Never kills anything —
/// live tracked PIDs may belong to in-flight runs in other projects (the
/// tracker is global), so terminating them stays a manual operator decision.
/// Returns the pruned run ids.
pub(super) fn prune_stale_entries_for_fix() -> Result<Vec<String>> {
    let _lock = acquire_tracker_lock()?;
    let mut tracker = load_cli_tracker()?;
    let mut pruned: Vec<String> =
        tracker.iter().filter(|(_, pid)| !process_exists(**pid as i32)).map(|(run_id, _)| run_id.clone()).collect();
    if pruned.is_empty() {
        return Ok(pruned);
    }
    for run_id in &pruned {
        tracker.remove(run_id);
    }
    pruned.sort();
    save_cli_tracker(&tracker)?;
    Ok(pruned)
}
