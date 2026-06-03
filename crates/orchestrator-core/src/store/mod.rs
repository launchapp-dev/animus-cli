use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use tempfile::{NamedTempFile, TempPath};

pub fn project_state_dir(project_root: &str) -> PathBuf {
    if let Some(scoped) = protocol::scoped_state_root(Path::new(project_root)) {
        return scoped.join("state");
    }
    Path::new(project_root).join(".animus").join("state")
}

pub fn read_json_or_default<T>(path: &Path) -> Result<T>
where
    T: Default + DeserializeOwned,
{
    if !path.exists() {
        return Ok(T::default());
    }
    let content = std::fs::read_to_string(path)?;
    let parsed = serde_json::from_str::<T>(&content)?;
    Ok(parsed)
}

// Durable atomic JSON write. Sequence:
//   1. write to a temp file in the target's parent dir,
//   2. fsync the temp file (flushes data + metadata to the disk; on macOS,
//      std's `sync_all` issues F_FULLFSYNC since Rust 1.79 so the bytes
//      reach platter, not just drive cache),
//   3. atomically rename temp -> final,
//   4. fsync the parent directory so the rename itself is durable across
//      power loss / kernel panic (rename only updates dir metadata; without
//      a dir fsync the rename can be lost even though the data file is
//      fully on disk).
// fsync is expensive (~5-50ms SSD, more on HDD) but workflow checkpoints
// fire at most a handful of times per phase (which themselves run for
// seconds-to-minutes), so the cost is negligible against the durability
// guarantee. Callers in the hot path (e.g. high-frequency log appends)
// should not use this helper.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let payload = serde_json::to_vec_pretty(value)?;
    let mut temp_file =
        NamedTempFile::new_in(parent).with_context(|| format!("failed to create temp file for {}", path.display()))?;
    temp_file.write_all(&payload).with_context(|| format!("failed to write temp file for {}", path.display()))?;
    temp_file.flush().with_context(|| format!("failed to flush temp file for {}", path.display()))?;
    temp_file.as_file().sync_all().with_context(|| format!("failed to sync temp file for {}", path.display()))?;

    persist_temp_path(temp_file.into_temp_path(), path)?;
    fsync_dir(parent).with_context(|| format!("failed to fsync parent dir for {}", path.display()))?;
    Ok(())
}

pub fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_json_atomic(path, value)
}

pub fn write_json_if_missing<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if path.exists() {
        return Ok(());
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    // Best-effort durability for the create: missing file means a fresh
    // seed write, and we want it to survive power loss the same as a
    // mutation through write_json_atomic.
    if let Ok(file) = File::open(path) {
        let _ = file.sync_all();
    }
    let _ = fsync_dir(parent);
    Ok(())
}

// fsync a directory entry so a child file's create/rename is durable.
// On Unix this opens the dir read-only and calls fsync; on Windows there
// is no equivalent — `File::open` on a directory fails — so we silently
// no-op (NTFS journals dir metadata transactionally, so the parent-dir
// fsync gap does not exist there).
pub fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let file = File::open(dir)?;
        file.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

// Durable rename helper for callers that already manage their own temp
// file. Fsyncs the temp file's data, performs the rename, then fsyncs the
// parent directory so the rename survives power loss.
//
// macOS note: `File::sync_all` invokes F_FULLFSYNC under Rust 1.79+, which
// forces the drive to flush its write cache. Plain `fsync(2)` on macOS
// only schedules the flush — it returns before the platter write
// completes — which is why this helper is preferred over hand-rolled
// rename pairs.
pub fn fsync_rename(tempfile: &Path, target: &Path) -> std::io::Result<()> {
    // Fsync the staged data first. If the caller already sync_all'd the
    // file handle before closing it, this is a cheap no-op on most
    // filesystems; if not, this is the line that prevents a torn write
    // from masquerading as a complete checkpoint after a crash.
    {
        let file = File::open(tempfile)?;
        file.sync_all()?;
    }
    std::fs::rename(tempfile, target)?;
    if let Some(parent) = target.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

fn persist_temp_path(temp_path: TempPath, path: &Path) -> Result<()> {
    match temp_path.persist(path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let tempfile::PathPersistError { error, path: temp_path } = error;
            if path.exists() {
                std::fs::remove_file(path)
                    .with_context(|| format!("failed to replace {} after rename failure", path.display()))?;
                temp_path
                    .persist(path)
                    .with_context(|| format!("failed to atomically move temp file to {}", path.display()))?;
                Ok(())
            } else {
                Err(error).with_context(|| format!("failed to atomically move temp file to {}", path.display()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsync_dir_succeeds_on_existing_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        fsync_dir(temp.path()).expect("fsync existing dir");
    }

    #[test]
    fn fsync_rename_promotes_temp_to_final_and_makes_it_readable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tmp_path = temp.path().join("payload.tmp");
        let final_path = temp.path().join("payload.json");
        std::fs::write(&tmp_path, b"{\"ok\":true}").expect("write tmp");
        fsync_rename(&tmp_path, &final_path).expect("durable rename");
        assert!(!tmp_path.exists(), "temp file should be moved");
        assert!(final_path.exists(), "final file should exist");
        let read = std::fs::read_to_string(&final_path).expect("read final");
        assert_eq!(read, "{\"ok\":true}");
    }

    #[test]
    fn write_json_atomic_round_trip_persists_value_durably() {
        #[derive(serde::Serialize, serde::Deserialize, Default, Debug, PartialEq)]
        struct State {
            tick: u64,
            label: String,
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("state").join("snapshot.json");
        let state = State { tick: 42, label: "checkpoint".to_string() };
        write_json_atomic(&target, &state).expect("durable write");
        let loaded: State = read_json_or_default(&target).expect("read state");
        assert_eq!(loaded, state);
    }
}
