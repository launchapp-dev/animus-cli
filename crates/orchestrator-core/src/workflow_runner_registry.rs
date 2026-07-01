use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const WORKFLOW_RUNNER_DIR: &str = "workflow-runner-pids";
const PID_FILE_EXT: &str = "pid";

fn workflow_runner_pid_dir(project_root: &Path) -> PathBuf {
    let base = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".animus"));
    base.join("state").join(WORKFLOW_RUNNER_DIR)
}

fn workflow_runner_pid_path(project_root: &Path, workflow_id: &str) -> PathBuf {
    workflow_runner_pid_dir(project_root).join(format!("{workflow_id}.{PID_FILE_EXT}"))
}

/// The start time of a running process, as an opaque string, used to detect PID
/// REUSE. On Linux this is field 22 (`starttime`) of `/proc/<pid>/stat` — a
/// value fixed for the life of that specific process. Recording it alongside a
/// runner pid lets the liveness check reject a pid whose current holder is a
/// DIFFERENT process than the one registered. This is the classic failure after
/// a container OR host restart: the daemon restarts, the OS reuses the dead
/// runner's PID for an unrelated process, and a bare `is_process_alive(pid)`
/// check mistakes it for a live runner — stranding the interrupted run, shielded
/// from both cancellation and resume. Unlike a host boot id, a per-process start
/// time also distinguishes a CONTAINER restart on the same host. Returns `None`
/// on platforms without `/proc` (dev), where the check falls back to bare
/// liveness.
fn process_start_time(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` (field 2) is parenthesized and may itself contain spaces/parens, so
    // split AFTER the last ')'. Post-comm token 0 is field 3 (`state`);
    // `starttime` is field 22, i.e. post-comm index 19.
    stat.rsplit_once(')')?.1.split_whitespace().nth(19).map(ToOwned::to_owned)
}

pub fn register_workflow_runner_pid(project_root: &Path, workflow_id: &str, pid: u32) -> Result<()> {
    let dir = workflow_runner_pid_dir(project_root);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create workflow runner registry directory at {}", dir.display()))?;
    // Record `<pid>\n<start_time>` so a restart (which reuses PIDs) can never
    // mistake a dead runner for a live one. Legacy files with only a pid stay
    // readable via the bare-liveness fallback in `active_workflow_runner_ids`.
    let contents = match process_start_time(pid) {
        Some(start) => format!("{pid}\n{start}"),
        None => pid.to_string(),
    };
    fs::write(workflow_runner_pid_path(project_root, workflow_id), contents)
        .with_context(|| format!("failed to record active workflow runner pid for workflow '{workflow_id}'"))?;
    Ok(())
}

pub fn unregister_workflow_runner_pid(project_root: &Path, workflow_id: &str) -> Result<()> {
    let path = workflow_runner_pid_path(project_root, workflow_id);
    match fs::remove_file(&path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => {
            Err(err).with_context(|| format!("failed to remove workflow runner registry entry at {}", path.display()))
        }
    }
}

/// Pure liveness decision from the resolved facts: whether the recorded pid is
/// alive, the start time recorded at registration (if any), and the start time
/// of whatever process now holds that pid (if resolvable). A pid that is alive
/// but whose current start time DIFFERS from the recorded one is a REUSED pid (a
/// different process) and is treated as dead. When either start time is absent
/// (a legacy file, or a platform without `/proc`), fall back to bare liveness.
fn runner_is_live(pid_alive: bool, recorded_start: Option<&str>, current_start: Option<&str>) -> bool {
    if !pid_alive {
        return false;
    }
    match (recorded_start, current_start) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => true,
    }
}

pub fn active_workflow_runner_ids(project_root: &Path) -> Result<HashSet<String>> {
    let dir = workflow_runner_pid_dir(project_root);
    if !dir.exists() {
        return Ok(HashSet::new());
    }

    let mut active = HashSet::new();
    for entry in fs::read_dir(&dir)
        .with_context(|| format!("failed to read workflow runner registry directory at {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some(PID_FILE_EXT) {
            continue;
        }

        let Some(workflow_id) = path.file_stem().and_then(|value| value.to_str()).map(ToOwned::to_owned) else {
            continue;
        };

        let raw = fs::read_to_string(&path).unwrap_or_default();
        let mut lines = raw.lines();
        let pid = lines.next().and_then(|value| value.trim().parse::<u32>().ok());
        let recorded_start = lines.next().map(|value| value.trim().to_string()).filter(|value| !value.is_empty());

        let alive = match pid {
            Some(pid) => runner_is_live(
                protocol::is_process_alive(pid),
                recorded_start.as_deref(),
                process_start_time(pid).as_deref(),
            ),
            None => false,
        };

        if alive {
            active.insert(workflow_id);
        } else {
            let _ = fs::remove_file(&path);
        }
    }

    Ok(active)
}

#[cfg(test)]
mod tests {
    use super::runner_is_live;

    #[test]
    fn reused_pid_after_restart_is_never_live() {
        // Same process: alive + start time matches => live.
        assert!(runner_is_live(true, Some("8421"), Some("8421")));
        // REUSED pid after a restart: alive but a DIFFERENT start time => stale.
        // Pre-fix this returned true and stranded the interrupted run as a zombie.
        assert!(!runner_is_live(true, Some("8421"), Some("99999")));
        // Dead pid => never live.
        assert!(!runner_is_live(false, Some("8421"), Some("8421")));
        // Legacy file (no recorded start time) => bare-liveness fallback.
        assert!(runner_is_live(true, None, Some("8421")));
        // No current start time (non-Linux dev) => bare-liveness fallback.
        assert!(runner_is_live(true, Some("8421"), None));
    }
}
