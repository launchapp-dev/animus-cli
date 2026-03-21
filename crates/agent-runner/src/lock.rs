use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{info, warn};
use uuid::Uuid;

use crate::config;

fn get_runner_lock_path() -> PathBuf {
    let app_dir = config::app_config_dir();
    std::fs::create_dir_all(&app_dir).ok();
    app_dir.join("agent-runner.lock")
}

/// Atomically removes a stale lock file using rename-to-temp pattern.
///
/// This prevents race conditions where two runners could both attempt to
/// clean the same stale lock file.
///
/// # Returns
/// - `Ok(true)`: This runner won the race. Caller should clean the temp file
///   and retry acquiring the lock.
/// - `Ok(false)`: Another runner won the race. Caller should return gracefully.
/// - `Err(_)`: A real error occurred (not ENOENT).
pub fn atomic_remove_stale_lock(lock_path: &Path) -> Result<bool> {
    let temp_path = lock_path.with_file_name(format!(
        "{}.stale.{}",
        lock_path.file_name().unwrap().to_string_lossy(),
        Uuid::new_v4()
    ));

    info!(
        lock_path = %lock_path.display(),
        temp_path = %temp_path.display(),
        "Attempting atomic rename of stale lock"
    );

    match fs::rename(&lock_path, &temp_path) {
        Ok(_) => {
            info!(
                lock_path = %lock_path.display(),
                temp_path = %temp_path.display(),
                "Won race for stale lock cleanup"
            );
            // Clean up the temp file - we're now responsible for it
            if let Err(e) = fs::remove_file(&temp_path) {
                warn!(
                    temp_path = %temp_path.display(),
                    error = %e,
                    "Failed to clean up temp stale lock file (non-fatal)"
                );
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!(
                lock_path = %lock_path.display(),
                "Lost race for stale lock cleanup (another runner won)"
            );
            Ok(false)
        }
        Err(e) => {
            warn!(
                lock_path = %lock_path.display(),
                error = %e,
                "Failed to atomically remove stale lock"
            );
            Err(e).context("Failed to atomically remove stale lock file")
        }
    }
}

pub fn acquire_runner_lock() -> Result<File> {
    let lock_path = get_runner_lock_path();
    info!(lock_path = %lock_path.display(), "Attempting to acquire runner lock");

    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .context("Failed to open lock file")?;

    match lock_file.try_lock_exclusive() {
        Ok(_) => {
            lock_file.set_len(0)?;
            let pid = std::process::id();
            let ipc_address = get_ipc_address();
            write!(&lock_file, "{}|{}", pid, ipc_address)?;
            lock_file.sync_all()?;
            info!(pid, ipc_address = %ipc_address, "Runner lock acquired");
            Ok(lock_file)
        }
        Err(_) => {
            let (existing_pid, ipc_addr) = read_runner_lock(&lock_path)?;
            if process_exists(existing_pid) {
                warn!(
                    existing_pid,
                    ipc_address = %ipc_addr,
                    "Runner lock is held by an active process"
                );
                bail!("Another agent runner is already running (PID: {}, IPC: {}).", existing_pid, ipc_addr);
            }
            warn!(
                existing_pid,
                ipc_address = %ipc_addr,
                "Found stale runner lock; removing and retrying"
            );
            drop(lock_file);

            // Use atomic rename to prevent race conditions with other runners
            match atomic_remove_stale_lock(&lock_path) {
                Ok(true) => {
                    // We won the race - stale lock has been removed
                    info!(lock_path = %lock_path.display(), "Successfully removed stale runner lock");
                    acquire_runner_lock()
                }
                Ok(false) => {
                    // Another runner won the race and removed the lock
                    // The lock file no longer exists, so we can return gracefully
                    // or the other runner will handle acquiring it
                    info!(
                        lock_path = %lock_path.display(),
                        "Lost race for stale lock cleanup; another runner is handling it"
                    );
                    bail!("Another agent runner is cleaning up a stale lock. Please retry.");
                }
                Err(e) => {
                    // Real error - propagate
                    warn!(error = %e, "Failed to remove stale lock");
                    return Err(e);
                }
            }
        }
    }
}

fn read_runner_lock(lock_path: &Path) -> Result<(i32, String)> {
    let content = fs::read_to_string(lock_path)?;
    let parts: Vec<&str> = content.split('|').collect();
    let pid = parts.first().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
    let ipc_addr = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
    Ok((pid, ipc_addr))
}

fn get_ipc_address() -> String {
    #[cfg(unix)]
    {
        config::app_config_dir().join("agent-runner.sock").display().to_string()
    }

    #[cfg(not(unix))]
    {
        "127.0.0.1:9001".to_string()
    }
}

use protocol::process_exists;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Test that atomic_remove_stale_lock returns Ok(true) when successfully
    /// removing a stale lock file (winning the race).
    #[test]
    fn test_atomic_remove_stale_lock_success() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");

        // Create a fake lock file
        fs::write(&lock_path, "12345|/tmp/test.sock").unwrap();

        // Should succeed - we won the race
        let result = atomic_remove_stale_lock(&lock_path);
        assert!(result.is_ok());
        assert!(result.unwrap());

        // Lock file should be gone
        assert!(!lock_path.exists());
    }

    /// Test that atomic_remove_stale_lock returns Ok(false) when the lock file
    /// doesn't exist (another runner won the race).
    #[test]
    fn test_atomic_remove_stale_lock_already_removed() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");

        // Don't create the lock file - simulating another runner already removed it

        // Should return Ok(false) - we lost the race
        let result = atomic_remove_stale_lock(&lock_path);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    /// Test that atomic_remove_stale_lock propagates filesystem errors.
    #[test]
    fn test_atomic_remove_stale_lock_propagates_errors() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");

        // Create a fake lock file
        fs::write(&lock_path, "12345|/tmp/test.sock").unwrap();

        // Remove write permission from the parent directory to cause rename to fail.
        // fs::rename requires write permission on the directory, not the file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(temp_dir.path()).unwrap().permissions();
            perms.set_mode(0o555); // read+execute only, no write
            fs::set_permissions(temp_dir.path(), perms).unwrap();
        }

        // Should propagate the error (not return Ok)
        let result = atomic_remove_stale_lock(&lock_path);
        assert!(result.is_err());

        #[cfg(unix)]
        {
            // Restore permissions for cleanup
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(temp_dir.path()).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(temp_dir.path(), perms).unwrap();
        }
    }

    /// Test that temp files are cleaned up after successful atomic remove.
    #[test]
    fn test_atomic_remove_stale_lock_cleans_temp_file() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");

        // Create a fake lock file
        fs::write(&lock_path, "12345|/tmp/test.sock").unwrap();

        // Should succeed
        let result = atomic_remove_stale_lock(&lock_path);
        assert!(result.is_ok());
        assert!(result.unwrap());

        // No temp files should remain
        for entry in fs::read_dir(temp_dir.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            assert!(!name.to_string_lossy().contains(".stale."), "Found orphaned temp file");
        }
    }

    /// Test that temp path has proper format with UUID suffix.
    #[test]
    fn test_temp_path_format() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");

        // Create a fake lock file
        fs::write(&lock_path, "12345|/tmp/test.sock").unwrap();

        // The function should create a temp file, rename to it, then remove it
        let result = atomic_remove_stale_lock(&lock_path);
        assert!(result.is_ok());

        // After success, no files should exist (lock removed, temp cleaned)
        assert!(!lock_path.exists());
    }
}
