use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionCheckpointStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    pub workflow_id: String,
    pub phase_id: String,
    pub provider: String,
    pub run_id: String,
    // `provider_session_id` is the provider plugin's external session id
    // (what `resume_agent` accepts). It is None until the plugin has
    // reported one back. Pre-v0.4.6 checkpoints incorrectly stored
    // `run_id` in a `session_id` slot; that legacy field is consumed by
    // `legacy_session_id` below and deliberately NOT promoted to
    // `provider_session_id` (the bytes are not a real provider id), so
    // auto-resume safely skips legacy checkpoints rather than dispatching
    // an unknown id to the plugin.
    #[serde(default)]
    pub provider_session_id: Option<String>,
    // Captured purely to drain stale on-disk values written before the
    // v0.4.6 fix split run_id from provider_session_id. Never read by the
    // runtime; never serialized back out.
    #[serde(default, rename = "session_id", skip_serializing)]
    pub legacy_session_id: Option<String>,
    pub status: SessionCheckpointStatus,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Value>,
}

pub fn phase_session_path(scoped_root: &Path, workflow_id: &str, phase_id: &str) -> PathBuf {
    scoped_root
        .join("runs")
        .join(sanitize(workflow_id))
        .join("phases")
        .join(format!("{}.session.json", sanitize(phase_id)))
}

pub fn write_session_pending(
    scoped_root: &Path,
    workflow_id: &str,
    phase_id: &str,
    provider: &str,
    run_id: &str,
    request: Option<Value>,
) -> io::Result<SessionCheckpoint> {
    #[cfg(any(test, feature = "test-fault"))]
    test_fault::maybe_fail(test_fault::FaultOp::Pending)?;
    let path = phase_session_path(scoped_root, workflow_id, phase_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let checkpoint = SessionCheckpoint {
        workflow_id: workflow_id.to_string(),
        phase_id: phase_id.to_string(),
        provider: provider.to_string(),
        run_id: run_id.to_string(),
        provider_session_id: None,
        legacy_session_id: None,
        status: SessionCheckpointStatus::Pending,
        started_at: Utc::now().to_rfc3339(),
        completed_at: None,
        blocked_reason: None,
        request,
    };
    write_atomic(&path, &checkpoint)?;
    Ok(checkpoint)
}

// Marks a checkpoint Running WITHOUT setting provider_session_id. The
// provider's external id arrives asynchronously (e.g. via a sidecar the
// runner persists after the plugin's first response); callers should
// invoke `update_provider_session_id` separately once it is known.
pub fn update_session_running(scoped_root: &Path, workflow_id: &str, phase_id: &str) -> io::Result<()> {
    #[cfg(any(test, feature = "test-fault"))]
    test_fault::maybe_fail(test_fault::FaultOp::Running)?;
    mutate(scoped_root, workflow_id, phase_id, |checkpoint| {
        checkpoint.status = SessionCheckpointStatus::Running;
    })
}

// Records the provider plugin's external session id (the one resume_agent
// will accept). Called after the plugin reports its session id back to the
// runner — never with the internal run_id.
pub fn update_provider_session_id(
    scoped_root: &Path,
    workflow_id: &str,
    phase_id: &str,
    provider_session_id: &str,
) -> io::Result<()> {
    mutate(scoped_root, workflow_id, phase_id, |checkpoint| {
        if checkpoint.provider_session_id.as_deref() != Some(provider_session_id) {
            checkpoint.provider_session_id = Some(provider_session_id.to_string());
        }
    })
}

pub fn update_session_running_after_resume(
    scoped_root: &Path,
    workflow_id: &str,
    phase_id: &str,
    new_provider_session_id: Option<&str>,
) -> io::Result<()> {
    mutate(scoped_root, workflow_id, phase_id, |checkpoint| {
        if let Some(sid) = new_provider_session_id {
            checkpoint.provider_session_id = Some(sid.to_string());
        }
        checkpoint.status = SessionCheckpointStatus::Running;
        checkpoint.blocked_reason = None;
        checkpoint.started_at = Utc::now().to_rfc3339();
    })
}

pub fn update_session_completed(scoped_root: &Path, workflow_id: &str, phase_id: &str) -> io::Result<()> {
    #[cfg(any(test, feature = "test-fault"))]
    test_fault::maybe_fail(test_fault::FaultOp::Completed)?;
    mutate(scoped_root, workflow_id, phase_id, |checkpoint| {
        checkpoint.status = SessionCheckpointStatus::Completed;
        checkpoint.completed_at = Some(Utc::now().to_rfc3339());
    })
}

pub fn update_session_blocked(scoped_root: &Path, workflow_id: &str, phase_id: &str, reason: &str) -> io::Result<()> {
    mutate(scoped_root, workflow_id, phase_id, |checkpoint| {
        checkpoint.status = SessionCheckpointStatus::Blocked;
        checkpoint.blocked_reason = Some(reason.to_string());
    })
}

// Marks a checkpoint terminally Failed after the phase event stream returned
// an Err (agent crash, non-zero exit, transport disconnect). Distinct from
// Blocked so `list_running_checkpoints` does not surface it for daemon-restart
// auto-resume — the run is over, not paused waiting for input.
pub fn update_session_failed(scoped_root: &Path, workflow_id: &str, phase_id: &str, reason: &str) -> io::Result<()> {
    #[cfg(any(test, feature = "test-fault"))]
    test_fault::maybe_fail(test_fault::FaultOp::Failed)?;
    mutate(scoped_root, workflow_id, phase_id, |checkpoint| {
        checkpoint.status = SessionCheckpointStatus::Failed;
        checkpoint.blocked_reason = Some(reason.to_string());
        checkpoint.completed_at = Some(Utc::now().to_rfc3339());
    })
}

// Best-effort lookup of the provider plugin's external session id from the
// runner-sessions sidecar the agent-runner writes when a native session
// backend produces a `Started { session_id }` event. Returns None when the
// sidecar is missing, malformed, or the plugin never reported an id (e.g.
// CLI-only providers without a resumable session).
pub fn lookup_runner_session_sidecar(run_id: &str) -> Option<String> {
    let runs_root = std::env::var_os("ANIMUS_RUNNER_SESSION_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".animus").join("runner-sessions")))?;
    let path = runs_root.join(format!("{}.session.json", sanitize(run_id)));
    let contents = fs::read_to_string(&path).ok()?;
    let parsed: Value = serde_json::from_str(&contents).ok()?;
    let sid = parsed.get("session_id").and_then(Value::as_str)?.trim();
    if sid.is_empty() {
        None
    } else {
        Some(sid.to_string())
    }
}

pub fn read_checkpoint(scoped_root: &Path, workflow_id: &str, phase_id: &str) -> io::Result<Option<SessionCheckpoint>> {
    let path = phase_session_path(scoped_root, workflow_id, phase_id);
    read_path(&path)
}

pub fn read_path(path: &Path) -> io::Result<Option<SessionCheckpoint>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let trimmed = contents.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let checkpoint: SessionCheckpoint = serde_json::from_str(trimmed).map_err(io::Error::other)?;
            Ok(Some(checkpoint))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

pub fn list_running_checkpoints(scoped_root: &Path) -> io::Result<Vec<(PathBuf, SessionCheckpoint)>> {
    let runs_dir = scoped_root.join("runs");
    let mut out = Vec::new();
    let entries = match fs::read_dir(&runs_dir) {
        Ok(e) => e,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    for run_entry in entries {
        let run_entry = run_entry?;
        let phases_dir = run_entry.path().join("phases");
        let phase_entries = match fs::read_dir(&phases_dir) {
            Ok(e) => e,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        for phase_entry in phase_entries {
            let phase_entry = phase_entry?;
            let path = phase_entry.path();
            if !path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".session.json")) {
                continue;
            }
            if let Some(checkpoint) = read_path(&path)? {
                if matches!(checkpoint.status, SessionCheckpointStatus::Running) {
                    out.push((path, checkpoint));
                }
            }
        }
    }
    Ok(out)
}

/// Workflow ids that have at least one session checkpoint in `Blocked` state.
///
/// A `Blocked` checkpoint marks a mid-phase resume that was intentionally held
/// (e.g. the provider plugin is not installed, or `resume_agent` returned a
/// failure) and is waiting on an operator `animus workflow resume --force`. The
/// daemon's journal-resume re-dispatch consults this so it does NOT spawn a
/// fresh runner for such a run and bypass the hold.
pub fn blocked_checkpoint_workflow_ids(scoped_root: &Path) -> io::Result<std::collections::HashSet<String>> {
    let runs_dir = scoped_root.join("runs");
    let mut out = std::collections::HashSet::new();
    let entries = match fs::read_dir(&runs_dir) {
        Ok(e) => e,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(err) => return Err(err),
    };
    for run_entry in entries {
        let run_entry = run_entry?;
        let phases_dir = run_entry.path().join("phases");
        let phase_entries = match fs::read_dir(&phases_dir) {
            Ok(e) => e,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        for phase_entry in phase_entries {
            let phase_entry = phase_entry?;
            let path = phase_entry.path();
            if !path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".session.json")) {
                continue;
            }
            if let Some(checkpoint) = read_path(&path)? {
                if matches!(checkpoint.status, SessionCheckpointStatus::Blocked) {
                    out.insert(checkpoint.workflow_id);
                }
            }
        }
    }
    Ok(out)
}

fn mutate(
    scoped_root: &Path,
    workflow_id: &str,
    phase_id: &str,
    f: impl FnOnce(&mut SessionCheckpoint),
) -> io::Result<()> {
    let path = phase_session_path(scoped_root, workflow_id, phase_id);
    let mut checkpoint = read_path(&path)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, format!("no session checkpoint at {}", path.display()))
    })?;
    f(&mut checkpoint);
    write_atomic(&path, &checkpoint)
}

// Session checkpoints are the recovery oracle for in-flight phases after a
// daemon crash or power loss. The write sequence is:
//   1. write the payload to a sibling tempfile,
//   2. open + sync_all() the tempfile so the data blocks reach the disk
//      (macOS: F_FULLFSYNC via std::fs since Rust 1.79; Linux: fsync),
//   3. rename tempfile -> final (atomic on POSIX),
//   4. fsync the parent directory so the rename itself is durable.
// Without step 4 the rename can land in the dir cache and be lost on
// power loss even though the data file is fully on disk, which would
// surface as a missing or stale checkpoint after reboot.
// Cost: roughly one extra fsync (~5-50ms SSD) per checkpoint mutation.
// Phases run for seconds-to-minutes so this is negligible vs. the
// durability guarantee.
fn write_atomic(path: &Path, checkpoint: &SessionCheckpoint) -> io::Result<()> {
    let payload = serde_json::to_vec_pretty(checkpoint).map_err(io::Error::other)?;
    let tmp = path.with_extension("session.json.tmp");
    {
        use std::io::Write;
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&payload)?;
        file.sync_all()?;
    }
    orchestrator_core::store::fsync_rename(&tmp, path)
}

fn sanitize(value: &str) -> String {
    value.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect()
}

/// Per-thread fault-injection seam for the four durable-checkpoint write
/// paths. Tests install a guard that arms a specific [`FaultOp`] for the
/// duration of the test; the matching `write_session_pending` /
/// `update_session_running` / `update_session_completed` /
/// `update_session_failed` call returns a synthetic
/// `io::ErrorKind::PermissionDenied` instead of writing.
///
/// This exists so the crash-replay invariant tests in
/// [`crate::phase_executor`] can assert that the dispatcher treats each
/// checkpoint failure as FATAL — without resorting to chmod games on the
/// tempdir, which are platform-fragile and race the parent-directory fsync.
#[cfg(any(test, feature = "test-fault"))]
pub mod test_fault {
    use std::cell::Cell;
    use std::io;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FaultOp {
        Pending,
        Running,
        Completed,
        Failed,
    }

    thread_local! {
        static ARMED: Cell<Option<FaultOp>> = const { Cell::new(None) };
    }

    /// RAII guard. Arms the fault for the current thread on construction
    /// and disarms on drop. Tests must not span threads while the guard is
    /// live; the per-thread cell means each parallel test gets its own
    /// arming without serializing on a global mutex.
    pub struct FaultGuard;

    impl FaultGuard {
        pub fn arm(op: FaultOp) -> Self {
            ARMED.with(|cell| cell.set(Some(op)));
            Self
        }
    }

    impl Drop for FaultGuard {
        fn drop(&mut self) {
            ARMED.with(|cell| cell.set(None));
        }
    }

    pub fn maybe_fail(op: FaultOp) -> io::Result<()> {
        let armed = ARMED.with(Cell::get);
        if armed == Some(op) {
            // Disarm so a single armed op doesn't spuriously fire on
            // re-entry (e.g. the dispatcher's own retry path on the next
            // tick, which legitimately re-attempts the same mutation).
            ARMED.with(|cell| cell.set(None));
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("test_fault::maybe_fail injected failure for {:?}", op),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // We can't directly observe fsync syscalls without ptrace, so the
    // proxy test is: a checkpoint written through write_atomic must be
    // immediately readable, the tmp sibling must be cleaned up, and a
    // second mutation must produce the new state (no leftover tmp,
    // no half-written file). This covers the fsync-then-rename flow
    // end-to-end short of an actual power-cut harness.
    #[test]
    fn checkpoint_write_round_trip_through_fsync_path() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        let cp = write_session_pending(scoped_root, "wf-fsync-1", "phase-a", "claude", "run-1", None)
            .expect("write pending checkpoint");
        assert_eq!(cp.status, SessionCheckpointStatus::Pending);
        let read = read_checkpoint(scoped_root, "wf-fsync-1", "phase-a").expect("read").expect("present");
        assert_eq!(read.run_id, "run-1");

        // Final path exists; tmp sibling was cleaned up by rename.
        let final_path = phase_session_path(scoped_root, "wf-fsync-1", "phase-a");
        assert!(final_path.exists(), "final checkpoint file should exist");
        let tmp_path = final_path.with_extension("session.json.tmp");
        assert!(!tmp_path.exists(), "tmp file must not survive the rename");

        update_session_running(scoped_root, "wf-fsync-1", "phase-a").expect("flip running");
        let after = read_checkpoint(scoped_root, "wf-fsync-1", "phase-a").expect("re-read").expect("present");
        assert_eq!(after.status, SessionCheckpointStatus::Running);
        assert!(!tmp_path.exists(), "tmp file must not survive the second rename either");
    }

    // Verifies the parent-dir fsync path: every mutation must leave the
    // directory in a state where `read_dir` immediately sees the final
    // file (no torn-rename window).
    #[test]
    fn parent_dir_fsync_makes_rename_visible_immediately() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        write_session_pending(scoped_root, "wf-fsync-2", "phase-b", "codex", "run-2", None).expect("write pending");

        let phases_dir = scoped_root.join("runs").join(sanitize("wf-fsync-2")).join("phases");
        let entries: Vec<_> = std::fs::read_dir(&phases_dir)
            .expect("read phases dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        let session_file = std::ffi::OsString::from("phase-b.session.json");
        assert!(entries.contains(&session_file), "phases dir should show the final session file: {entries:?}");
        // And no leftover .tmp siblings.
        let tmp_count = entries.iter().filter(|name| name.to_string_lossy().ends_with(".tmp")).count();
        assert_eq!(tmp_count, 0, "no .tmp siblings should remain after fsync_rename");
    }
}
