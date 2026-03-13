use anyhow::{Context, Result};
use fs2::FileExt;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::Path;
use tracing::{debug, info, warn};

pub use protocol::{graceful_kill_process, kill_process, process_exists};

#[cfg(windows)]
pub use protocol::{track_job, untrack_job};

fn read_tracker(tracker_path: &Path) -> Result<HashMap<String, u32>> {
    if !tracker_path.exists() {
        return Ok(HashMap::new());
    }
    let content = fs::read_to_string(tracker_path)?;
    if content.trim().is_empty() {
        return Ok(HashMap::new());
    }
    serde_json::from_str(&content).context("failed to parse process tracker JSON")
}

fn with_tracker_lock<F, T>(f: F) -> Result<T>
where
    F: FnOnce(&Path) -> Result<T>,
{
    let tracker_path = protocol::cli_tracker_path();
    if let Some(parent) = tracker_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = tracker_path.with_extension("lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open tracker lock at {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .context("failed to acquire exclusive lock on process tracker")?;
    let result = f(&tracker_path);
    lock_file.unlock().ok();
    result
}

pub fn cleanup_orphaned_clis() -> Result<()> {
    with_tracker_lock(|tracker_path| {
        if !tracker_path.exists() {
            debug!(path = %tracker_path.display(), "No orphan tracker file found");
            return Ok(());
        }

        let tracked = read_tracker(tracker_path)?;
        info!(
            tracked_count = tracked.len(),
            tracker_path = %tracker_path.display(),
            "Loaded tracked CLI processes for orphan cleanup"
        );

        let mut cleaned = 0;
        for (run_id, pid) in tracked {
            if !process_exists(pid as i32) {
                info!(run_id, pid, "Tracked process is already terminated");
                continue;
            }

            info!(run_id, pid, "Killing orphaned tracked process");
            if graceful_kill_process(pid as i32) {
                cleaned += 1;
            } else {
                warn!(run_id, pid, "Failed to kill orphaned process");
            }
        }

        fs::remove_file(tracker_path)?;
        info!(
            cleaned_count = cleaned,
            tracker_path = %tracker_path.display(),
            "Finished orphaned process cleanup"
        );
        Ok(())
    })
}

pub fn track_process(run_id: &str, pid: u32) -> Result<()> {
    with_tracker_lock(|tracker_path| {
        let mut tracked = read_tracker(tracker_path)?;
        tracked.insert(run_id.to_string(), pid);
        fs::write(tracker_path, serde_json::to_string(&tracked)?)?;
        debug!(
            run_id,
            pid,
            tracked_count = tracked.len(),
            tracker_path = %tracker_path.display(),
            "Tracked CLI process"
        );
        Ok(())
    })
}

pub fn untrack_process(run_id: &str) -> Result<()> {
    with_tracker_lock(|tracker_path| {
        if !tracker_path.exists() {
            return Ok(());
        }
        let mut tracked = read_tracker(tracker_path)?;
        let removed = tracked.remove(run_id).is_some();
        fs::write(tracker_path, serde_json::to_string(&tracked)?)?;
        debug!(
            run_id,
            removed,
            remaining = tracked.len(),
            tracker_path = %tracker_path.display(),
            "Untracked CLI process"
        );
        Ok(())
    })
}
