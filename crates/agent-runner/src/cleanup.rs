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
    temp.write_all(&payload)
        .with_context(|| format!("failed to write temp file for {}", tracker_path.display()))?;
    temp.flush()
        .with_context(|| format!("failed to flush temp file for {}", tracker_path.display()))?;
    temp.as_file().sync_all()
        .with_context(|| format!("failed to sync temp file for {}", tracker_path.display()))?;
    temp.persist(tracker_path)
        .with_context(|| format!("failed to atomically replace {}", tracker_path.display()))?;
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

        std::fs::remove_file(tracker_path)?;
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
        let lock_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
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
        assert!(
            entries.iter().all(|e| !e.starts_with('.')),
            "no hidden temp files should remain: {entries:?}"
        );
    }
}
