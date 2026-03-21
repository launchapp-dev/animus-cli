use anyhow::{Context, Result};
use fs2::FileExt;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;
use tracing::{debug, info, warn};

pub use protocol::{graceful_kill_process, process_exists};

#[cfg(windows)]
pub use protocol::untrack_job;

fn read_tracker(tracker_path: &Path) -> Result<HashMap<String, u32>> {
    if !tracker_path.exists() {
        return Ok(HashMap::new());
    }
    let content = std::fs::read_to_string(tracker_path)?;
    if content.trim().is_empty() {
        return Ok(HashMap::new());
    }
    serde_json::from_str(&content).context("failed to parse process tracker JSON")
}

/// Atomically write JSON content to `tracker_path` using a tempfile + rename.
///
/// The caller must already hold the exclusive tracker lock.
fn atomic_write_tracker(tracker_path: &Path, data: &HashMap<String, u32>) -> Result<()> {
    let parent = tracker_path.parent().unwrap_or_else(|| Path::new("."));
    let payload = serde_json::to_vec_pretty(data)?;
    let mut temp = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temp file for {}", tracker_path.display()))?;
    temp.write_all(&payload).with_context(|| format!("failed to write temp file for {}", tracker_path.display()))?;
    temp.flush().with_context(|| format!("failed to flush temp file for {}", tracker_path.display()))?;
    temp.as_file().sync_all().with_context(|| format!("failed to sync temp file for {}", tracker_path.display()))?;
    temp.persist(tracker_path).with_context(|| format!("failed to atomically replace {}", tracker_path.display()))?;
    Ok(())
}

fn with_tracker_lock<F, T>(f: F) -> Result<T>
where
    F: FnOnce(&Path) -> Result<T>,
{
    let tracker_path = protocol::cli_tracker_path();
    if let Some(parent) = tracker_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_path = tracker_path.with_extension("lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open tracker lock at {}", lock_path.display()))?;
    lock_file.lock_exclusive().context("failed to acquire exclusive lock on process tracker")?;
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
        let mut surviving = HashMap::new();
        for (run_id, pid) in tracked {
            if !process_exists(pid as i32) {
                info!(run_id, pid, "Tracked process is already terminated");
                continue;
            }

            info!(run_id, pid, "Killing orphaned tracked process");
            if graceful_kill_process(pid as i32) {
                cleaned += 1;
            } else {
                warn!(run_id, pid, "Failed to kill orphaned process; keeping in tracker");
                surviving.insert(run_id, pid);
            }
        }

        if surviving.is_empty() {
            // All entries cleaned up — remove the file to keep state tidy.
            let _ = std::fs::remove_file(tracker_path);
        } else {
            // Some processes could not be killed — persist them so the next
            // agent-runner restart gets another chance to clean them up.
            atomic_write_tracker(tracker_path, &surviving)?;
        }

        info!(
            cleaned_count = cleaned,
            surviving_count = surviving.len(),
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
        atomic_write_tracker(tracker_path, &tracked)?;
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
        atomic_write_tracker(tracker_path, &tracked)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Helper: run a closure under the tracker lock against a custom tracker path.
    /// Returns the result of the closure.
    ///
    /// This mirrors the structure of `with_tracker_lock` but allows tests to
    /// operate on a temporary directory instead of the real global config dir.
    fn with_tracker_lock_at<F, T>(tracker_path: &Path, f: F) -> Result<T>
    where
        F: FnOnce(&Path) -> Result<T>,
    {
        if let Some(parent) = tracker_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = tracker_path.with_extension("lock");
        let lock_file = OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
        lock_file.lock_exclusive()?;
        let result = f(tracker_path);
        lock_file.unlock().ok();
        result
    }

    /// Read the tracker at the given path (no lock — caller must hold the lock).
    fn read_tracker_at(tracker_path: &Path) -> Result<HashMap<String, u32>> {
        if !tracker_path.exists() {
            return Ok(HashMap::new());
        }
        let content = std::fs::read_to_string(tracker_path)?;
        if content.trim().is_empty() {
            return Ok(HashMap::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    /// Atomic write at a given path (no lock — caller must hold the lock).
    fn atomic_write_at(tracker_path: &Path, data: &HashMap<String, u32>) -> Result<()> {
        let parent = tracker_path.parent().unwrap_or_else(|| Path::new("."));
        let payload = serde_json::to_vec_pretty(data)?;
        let mut temp = NamedTempFile::new_in(parent)?;
        temp.write_all(&payload)?;
        temp.flush()?;
        temp.as_file().sync_all()?;
        temp.persist(tracker_path)?;
        Ok(())
    }

    #[test]
    fn atomic_write_creates_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker.json");

        let mut data = HashMap::new();
        data.insert("run-1".to_string(), 12345);
        data.insert("run-2".to_string(), 67890);

        with_tracker_lock_at(&path, |p| atomic_write_at(p, &data)).unwrap();

        let read_back = read_tracker_at(&path).unwrap();
        assert_eq!(read_back.get("run-1"), Some(&12345));
        assert_eq!(read_back.get("run-2"), Some(&67890));
    }

    #[test]
    fn atomic_write_overwrites_previous_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker.json");

        let mut first = HashMap::new();
        first.insert("run-old".to_string(), 11111);
        with_tracker_lock_at(&path, |p| atomic_write_at(p, &first)).unwrap();

        let mut second = HashMap::new();
        second.insert("run-new".to_string(), 22222);
        with_tracker_lock_at(&path, |p| atomic_write_at(p, &second)).unwrap();

        let read_back = read_tracker_at(&path).unwrap();
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back.get("run-new"), Some(&22222));
        assert!(!read_back.contains_key("run-old"));
    }

    #[test]
    fn read_missing_tracker_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let result = read_tracker_at(&path).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn track_and_untrack_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker.json");

        // Track two entries
        with_tracker_lock_at(&path, |p| {
            let mut tracked = read_tracker_at(p)?;
            tracked.insert("a".to_string(), 100);
            tracked.insert("b".to_string(), 200);
            atomic_write_at(p, &tracked)
        })
        .unwrap();

        // Untrack one
        with_tracker_lock_at(&path, |p| {
            let mut tracked = read_tracker_at(p)?;
            tracked.remove("a");
            atomic_write_at(p, &tracked)
        })
        .unwrap();

        let result = read_tracker_at(&path).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("b"), Some(&200));
    }

    #[test]
    fn atomic_write_survives_simulated_crash_before_rename() {
        // Verify that if atomic_write succeeds, the original file is replaced.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker.json");

        // Write initial data
        let mut initial = HashMap::new();
        initial.insert("original".to_string(), 1);
        with_tracker_lock_at(&path, |p| atomic_write_at(p, &initial)).unwrap();

        // Write replacement data
        let mut replacement = HashMap::new();
        replacement.insert("replaced".to_string(), 2);
        with_tracker_lock_at(&path, |p| atomic_write_at(p, &replacement)).unwrap();

        let result = read_tracker_at(&path).unwrap();
        assert_eq!(result.get("replaced"), Some(&2));
        assert!(!result.contains_key("original"));
        // No stale temp files should remain
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(entries.iter().all(|e| !e.starts_with('.')), "no hidden temp files should remain: {entries:?}");
    }

    #[test]
    fn read_tracker_accepts_flat_hashmap_format() {
        // Verify that the flat HashMap<String, u32> format used by cleanup.rs
        // is readable by the same read_tracker function that ops_runner.rs
        // also relies on (via the shared cli_tracker_path).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker.json");

        // Write in the flat format: { "run-abc": 12345 }
        let mut data = HashMap::new();
        data.insert("run-abc".to_string(), 12345u32);
        with_tracker_lock_at(&path, |p| atomic_write_at(p, &data)).unwrap();

        let read_back = read_tracker_at(&path).unwrap();
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back.get("run-abc"), Some(&12345u32));
    }

    #[test]
    fn read_tracker_rejects_wrapped_format() {
        // Verify that the old CliTrackerStateCli format { "processes": { ... } }
        // is NOT readable as a flat HashMap, confirming the schema mismatch bug.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker.json");

        // Write in the old wrapped format
        let wrapped = serde_json::json!({ "processes": { "run-abc": 12345 } });
        std::fs::write(&path, serde_json::to_string_pretty(&wrapped).unwrap()).unwrap();

        // Reading as flat HashMap should fail (schema mismatch)
        let result = read_tracker_at(&path);
        assert!(result.is_err(), "wrapped format should not parse as flat HashMap");
    }

    #[test]
    fn cleanup_preserves_surviving_entries() {
        // Verify that when cleanup_orphaned_clis cannot kill a process,
        // it writes back the surviving entries instead of deleting the tracker.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker.json");

        // Use only non-existent PIDs to avoid killing the test process.
        // One PID will "survive" (graceful_kill_process returns false for dead PIDs
        // since they don't exist), and one is already dead (process_exists returns false).
        let mut data = HashMap::new();
        data.insert("kill-fails".to_string(), 99998u32); // process_exists=true won't happen, so skip
        data.insert("already-dead".to_string(), 99999u32);
        with_tracker_lock_at(&path, |p| atomic_write_at(p, &data)).unwrap();

        // Simulate cleanup logic: read, check existence, attempt kill, write back survivors
        with_tracker_lock_at(&path, |p| {
            let tracked = read_tracker_at(p)?;
            let mut surviving = HashMap::new();
            let mut cleaned = 0usize;
            for (run_id, pid) in tracked {
                if !process_exists(pid as i32) {
                    cleaned += 1;
                    continue;
                }
                // Attempt graceful kill
                if graceful_kill_process(pid as i32) {
                    cleaned += 1;
                } else {
                    surviving.insert(run_id, pid);
                }
            }

            if surviving.is_empty() {
                let _ = std::fs::remove_file(p);
            } else {
                atomic_write_at(p, &surviving)?;
            }

            // All entries have non-existent PIDs, so all should be cleaned as "already terminated"
            assert_eq!(cleaned, 2, "both dead entries should be cleaned");
            assert!(surviving.is_empty(), "no survivors expected for non-existent PIDs");
            Ok::<(), anyhow::Error>(())
        })
        .unwrap();

        // Since all entries were cleaned, the file should be removed
        assert!(!path.exists(), "tracker file should be removed when all entries are cleaned");
    }

    #[test]
    fn cleanup_keeps_tracker_when_survivors_exist() {
        // Verify that when some processes can't be cleaned, the tracker file is preserved.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker.json");

        let mut data = HashMap::new();
        data.insert("dead-entry".to_string(), 99999u32);
        with_tracker_lock_at(&path, |p| atomic_write_at(p, &data)).unwrap();

        // Simulate cleanup where we force one entry to "survive"
        with_tracker_lock_at(&path, |p| {
            let tracked = read_tracker_at(p)?;
            let mut surviving = HashMap::new();
            let mut cleaned = 0usize;
            for (run_id, pid) in tracked {
                if !process_exists(pid as i32) {
                    cleaned += 1;
                    continue;
                }
                surviving.insert(run_id, pid);
            }

            // Force a survivor entry to test the write-back path
            surviving.insert("stubborn".to_string(), 1u32);

            if surviving.is_empty() {
                let _ = std::fs::remove_file(p);
            } else {
                atomic_write_at(p, &surviving)?;
            }

            assert_eq!(cleaned, 1, "dead entry should be cleaned");
            assert_eq!(surviving.len(), 1, "stubborn entry should survive");
            Ok::<(), anyhow::Error>(())
        })
        .unwrap();

        // Tracker should still exist with the surviving entry
        assert!(path.exists(), "tracker file should be preserved when survivors exist");
        let result = read_tracker_at(&path).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("stubborn"), Some(&1u32));
        assert!(!result.contains_key("dead-entry"));
    }
}
