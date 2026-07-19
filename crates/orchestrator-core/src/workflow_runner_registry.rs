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

/// A read-only diagnostic snapshot of the runner-pid registry entry for a single
/// `workflow_id`, resolving the SAME facts `active_workflow_runner_ids` uses to
/// decide liveness (see `runner_is_live`) WITHOUT its side effect of pruning a
/// stale pid file. The orphan reconciler reads this to explain, in logs, why a
/// run's registered runner did (or did not) shield it from the sweep — in
/// particular why a pid that is alive is still classified dead (a REUSED pid: the
/// recorded start time no longer matches the current holder's).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerLiveness {
    /// A pid file exists for this `workflow_id`.
    pub present: bool,
    /// The pid recorded in that file, if it parsed.
    pub pid: Option<u32>,
    /// Whether that pid is currently alive (bare OS liveness check).
    pub pid_alive: bool,
    /// The process start time recorded at registration (the REUSE guard), if any.
    pub recorded_start: Option<String>,
    /// The start time of whatever process now holds that pid, if resolvable.
    pub current_start: Option<String>,
    /// The final liveness decision (`runner_is_live`): the pid is alive AND
    /// (both start times match, or either is absent — the legacy/non-`proc`
    /// fallback). A live-but-reused pid is `false`.
    pub live: bool,
}

/// Resolve the [`RunnerLiveness`] snapshot for `workflow_id`, mirroring the
/// per-file liveness logic in [`active_workflow_runner_ids`] but WITHOUT removing
/// a stale file — so the reconciler can log the raw facts (recorded vs current
/// start time) that `active_workflow_runner_ids` would otherwise erase by
/// pruning the file it just judged dead.
pub fn workflow_runner_liveness(project_root: &Path, workflow_id: &str) -> RunnerLiveness {
    let path = workflow_runner_pid_path(project_root, workflow_id);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => return RunnerLiveness::default(),
    };
    let mut lines = raw.lines();
    let pid = lines.next().and_then(|value| value.trim().parse::<u32>().ok());
    let recorded_start = lines.next().map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    let pid_alive = pid.map(protocol::is_process_alive).unwrap_or(false);
    let current_start = pid.and_then(process_start_time);
    let live = match pid {
        Some(_) => runner_is_live(pid_alive, recorded_start.as_deref(), current_start.as_deref()),
        None => false,
    };
    RunnerLiveness { present: true, pid, pid_alive, recorded_start, current_start, live }
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
    use super::{register_workflow_runner_pid, runner_is_live, workflow_runner_liveness, workflow_runner_pid_path};
    use std::fs;
    use tempfile::TempDir;

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

    // The diagnostic accessor mirrors `active_workflow_runner_ids`' per-file
    // liveness resolution WITHOUT pruning stale files, so the reconciler can log
    // the raw facts. Cases: absent, live (self), dead, legacy, and reused.
    #[test]
    fn workflow_runner_liveness_reports_registry_facts() {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path();

        // Absent: no pid file => not present, not live.
        let absent = workflow_runner_liveness(root, "WF-absent");
        assert!(!absent.present, "no pid file => not present");
        assert!(!absent.live);
        assert_eq!(absent.pid, None);

        // Live: register the current (alive) process. On Linux the recorded and
        // current start times match; on platforms without `/proc` both are None
        // and the bare-liveness fallback still yields live.
        let me = std::process::id();
        register_workflow_runner_pid(root, "WF-live", me).expect("pid should register");
        let live = workflow_runner_liveness(root, "WF-live");
        assert!(live.present, "registered pid file => present");
        assert_eq!(live.pid, Some(me));
        assert!(live.pid_alive, "the current process is alive");
        assert!(live.live, "a live, non-reused runner must resolve live");

        // Dead: a pid that is not alive => not live regardless of start times.
        // u32::MAX is never a real pid.
        fs::write(workflow_runner_pid_path(root, "WF-dead"), u32::MAX.to_string()).expect("write dead pid");
        let dead = workflow_runner_liveness(root, "WF-dead");
        assert!(dead.present);
        assert_eq!(dead.pid, Some(u32::MAX));
        assert!(!dead.pid_alive, "u32::MAX is not a live pid");
        assert!(!dead.live);
        // Read-only: the accessor must NOT prune the stale file it just judged dead.
        assert!(workflow_runner_pid_path(root, "WF-dead").exists(), "accessor must not delete pid files");

        // Legacy: a pid-only file (no recorded start) for an alive pid => the
        // bare-liveness fallback keeps it live, and recorded_start is None.
        fs::write(workflow_runner_pid_path(root, "WF-legacy"), me.to_string()).expect("write legacy pid");
        let legacy = workflow_runner_liveness(root, "WF-legacy");
        assert!(legacy.present);
        assert_eq!(legacy.pid, Some(me));
        assert_eq!(legacy.recorded_start, None, "legacy file records no start time");
        assert!(legacy.live, "legacy file falls back to bare liveness");

        // Reused: an alive pid whose recorded start time is a sentinel that can
        // never match the real one. The accessor must READ that recorded start
        // verbatim (the KEY reap diagnostic); on Linux, where the current start
        // resolves, the mismatch makes it non-live.
        fs::write(workflow_runner_pid_path(root, "WF-reused"), format!("{me}\nsentinel-start-0"))
            .expect("write reused pid");
        let reused = workflow_runner_liveness(root, "WF-reused");
        assert!(reused.present);
        assert_eq!(reused.pid, Some(me));
        assert!(reused.pid_alive);
        assert_eq!(reused.recorded_start.as_deref(), Some("sentinel-start-0"), "recorded start must be surfaced");
        #[cfg(target_os = "linux")]
        {
            // On Linux the current start time resolves and differs from the
            // sentinel, so the reused pid is correctly classified dead.
            assert!(reused.current_start.is_some(), "current start resolves on Linux");
            assert!(!reused.live, "a live-but-reused pid must resolve NOT live");
        }
    }
}
