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
    /// The delegated environment context this phase is bound to, if any.
    /// Persisted right after `environment/prepare` succeeds so a daemon
    /// restart can teardown/reattach the node BY HANDLE instead of leaking
    /// it. `None` for local (non-delegated) runs.
    ///
    /// Backward-compat: `#[serde(default, skip_serializing_if = ...)]` means
    /// an older out-of-tree runner that does not know this field simply omits
    /// it on write and (because `SessionCheckpoint` is NOT
    /// `deny_unknown_fields`) ignores it on read — no runner rebuild is forced
    /// for schema compatibility. A runner that pre-dates this field will,
    /// however, DROP it when it rewrites a checkpoint it did not author, so the
    /// canonical durable write must come from a runner that carries this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentBinding>,
}

/// The delegated environment context (Railway/container/remote node) a phase
/// is bound to. Persisted into the phase [`SessionCheckpoint`] the instant
/// `environment/prepare` returns a handle, so restart reconciliation can reap
/// (or reattach to) the node by handle rather than leaking it and preparing a
/// brand-new one on re-dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentBinding {
    /// Resolved environment plugin id (what `EnvironmentClient::resolve` binds).
    /// Stored explicitly so a restart knows WHICH environment plugin to talk to
    /// for teardown/exec — the handle alone does not carry it.
    pub environment_id: String,
    /// The full, serializable prepared-context handle (id + workspace_root +
    /// opaque plugin metadata: container/remote/relay ids). Teardown and
    /// on-node re-exec are handle-only, so this is sufficient to reap or
    /// reattach after a restart.
    pub handle: animus_environment_protocol::EnvironmentHandle,
    /// RFC3339 timestamp of when the binding was persisted (prepare success).
    pub bound_at: String,
    /// Set true after a successful teardown so subsequent reconciliation sweeps
    /// skip an already-reaped node (idempotent, no double-free).
    #[serde(default)]
    pub torn_down: bool,
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
        environment: None,
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

/// Persist the delegated environment binding for this phase, right after
/// `environment/prepare` succeeds. Mirrors `update_provider_session_id`: the
/// canonical caller is the out-of-tree workflow runner (which owns
/// `scoped_root`/`workflow_id`/`phase_id` for a delegated coding phase and
/// already writes the pending/running/provider-session-id fields). Once
/// persisted, daemon restart reconciliation can reap the node by handle
/// instead of leaking it.
pub fn update_session_environment(
    scoped_root: &Path,
    workflow_id: &str,
    phase_id: &str,
    mut binding: EnvironmentBinding,
) -> io::Result<()> {
    // This writer records a newly prepared/re-adopted lease. Never trust a
    // caller-carried cleanup marker here: callers commonly derive replacement
    // bindings from an existing checkpoint, and retaining `torn_down = true`
    // would make the new handle invisible to restart reconciliation. The only
    // operation allowed to set this bit is a successful teardown via one of
    // the `mark_*_torn_down` helpers below.
    binding.torn_down = false;
    mutate(scoped_root, workflow_id, phase_id, |checkpoint| {
        checkpoint.environment = Some(binding);
    })
}

/// Mark the delegated node for this phase as torn down after a successful
/// `environment/teardown`. Leaves the binding in place (so the handle is still
/// visible for auditing) but flips `torn_down` so subsequent reconciliation
/// sweeps do not attempt a second teardown. A no-op if the checkpoint carries
/// no environment binding.
pub fn mark_environment_torn_down(scoped_root: &Path, workflow_id: &str, phase_id: &str) -> io::Result<()> {
    mutate(scoped_root, workflow_id, phase_id, |checkpoint| {
        if let Some(env) = checkpoint.environment.as_mut() {
            env.torn_down = true;
        }
    })
}

/// Mark every persisted phase binding for a workflow torn down.
///
/// Terminal cleanup can happen after the current checkpoint has already moved
/// out of `Running`, so a steady-state cleanup retry cannot rely on
/// `list_running_checkpoints`.
pub fn mark_workflow_environments_torn_down(scoped_root: &Path, workflow_id: &str) -> io::Result<usize> {
    let phases_dir = scoped_root.join("runs").join(sanitize(workflow_id)).join("phases");
    let entries = match fs::read_dir(phases_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut marked = 0usize;
    for entry in entries {
        let path = entry?.path();
        if !is_session_checkpoint_path(&path) {
            continue;
        }
        let raw = fs::read_to_string(&path)?;
        let mut checkpoint: SessionCheckpoint = serde_json::from_str(&raw).map_err(io::Error::other)?;
        let Some(binding) = checkpoint.environment.as_mut() else {
            continue;
        };
        if binding.torn_down {
            continue;
        }
        binding.torn_down = true;
        write_atomic(&path, &checkpoint)?;
        marked += 1;
    }
    Ok(marked)
}

/// Mark phase bindings for one exact delegated environment handle torn down.
///
/// A workflow can retain more than one historical binding, so startup cleanup
/// must not mark unrelated nodes merely because one broker record was reaped.
pub fn mark_workflow_environment_torn_down(
    scoped_root: &Path,
    workflow_id: &str,
    environment_id: &str,
    handle: &animus_environment_protocol::EnvironmentHandle,
) -> io::Result<usize> {
    let phases_dir = scoped_root.join("runs").join(sanitize(workflow_id)).join("phases");
    let entries = match fs::read_dir(phases_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut marked = 0usize;
    for entry in entries {
        let path = entry?.path();
        if !is_session_checkpoint_path(&path) {
            continue;
        }
        let raw = fs::read_to_string(&path)?;
        let mut checkpoint: SessionCheckpoint = serde_json::from_str(&raw).map_err(io::Error::other)?;
        let Some(binding) = checkpoint.environment.as_mut() else {
            continue;
        };
        if binding.torn_down || binding.environment_id != environment_id || binding.handle != *handle {
            continue;
        }
        binding.torn_down = true;
        write_atomic(&path, &checkpoint)?;
        marked += 1;
    }
    Ok(marked)
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

fn is_session_checkpoint_path(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(".session.json"))
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

/// Read every phase checkpoint persisted for one workflow, regardless of
/// status or whether the phase still appears in the workflow's current plan.
///
/// Retained delegated environments are workflow-scoped leases. A workflow
/// definition can change between the run that created a checkpoint and a
/// later cancellation, so cleanup must use the durable checkpoint directory
/// as its source of truth instead of scanning only the current phase plan.
pub fn list_workflow_checkpoints(scoped_root: &Path, workflow_id: &str) -> io::Result<Vec<SessionCheckpoint>> {
    let phases_dir = scoped_root.join("runs").join(sanitize(workflow_id)).join("phases");
    let entries = match fs::read_dir(phases_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !is_session_checkpoint_path(&path) {
            continue;
        }
        if let Some(checkpoint) = read_path(&path)? {
            // The sanitized directory name is not an ownership boundary:
            // distinct workflow ids can sanitize to the same path, and files
            // can also be misplaced. Never return a checkpoint whose durable
            // embedded owner does not match the workflow being enumerated.
            if checkpoint.workflow_id != workflow_id {
                continue;
            }
            out.push(checkpoint);
        }
    }
    Ok(out)
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

/// Find the non-terminal phase checkpoint which owns an agent run.
///
/// Environment preparation can race the runner's `Pending` -> `Running`
/// checkpoint transition. Binding persistence must therefore consider both
/// states: restricting this lookup to [`list_running_checkpoints`] creates a
/// prepare-to-persistence hole where a workflow-linked child is mistaken for
/// an ad-hoc run and its prepared node cannot be recovered after a restart.
pub fn find_active_checkpoint_by_run_id(scoped_root: &Path, run_id: &str) -> io::Result<Option<SessionCheckpoint>> {
    let runs_dir = scoped_root.join("runs");
    let run_entries = match fs::read_dir(runs_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    for run_entry in run_entries {
        let phases_dir = run_entry?.path().join("phases");
        let phase_entries = match fs::read_dir(phases_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        for phase_entry in phase_entries {
            let path = phase_entry?.path();
            if !is_session_checkpoint_path(&path) {
                continue;
            }
            let Some(checkpoint) = read_path(&path)? else {
                continue;
            };
            if checkpoint.run_id == run_id
                && matches!(checkpoint.status, SessionCheckpointStatus::Pending | SessionCheckpointStatus::Running)
            {
                return Ok(Some(checkpoint));
            }
        }
    }
    Ok(None)
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

    fn sample_binding() -> EnvironmentBinding {
        EnvironmentBinding {
            environment_id: "animus-environment-railway".to_string(),
            handle: animus_environment_protocol::EnvironmentHandle {
                id: "node-abc".to_string(),
                workspace_root: "/work".to_string(),
                metadata: serde_json::json!({ "railway_service_id": "svc-1", "relay": "wss://relay/x" }),
            },
            bound_at: Utc::now().to_rfc3339(),
            torn_down: false,
        }
    }

    // TASK-933: the EnvironmentBinding must survive a full serde round-trip
    // through the on-disk checkpoint (the whole point is teardown-by-handle
    // after a restart reads it back), including the opaque handle metadata.
    #[test]
    fn environment_binding_round_trips_through_the_checkpoint() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        write_session_pending(scoped_root, "wf-env-1", "phase-a", "claude", "run-1", None).expect("pending");

        // Freshly-written pending checkpoint has no binding.
        let cp = read_checkpoint(scoped_root, "wf-env-1", "phase-a").expect("read").expect("present");
        assert!(cp.environment.is_none(), "a local run carries no environment binding");

        let binding = sample_binding();
        update_session_environment(scoped_root, "wf-env-1", "phase-a", binding.clone()).expect("persist binding");

        let cp = read_checkpoint(scoped_root, "wf-env-1", "phase-a").expect("read").expect("present");
        assert_eq!(cp.environment.as_ref(), Some(&binding), "the binding round-trips byte-for-byte");
        let env = cp.environment.expect("present");
        assert_eq!(env.handle.id, "node-abc");
        assert_eq!(
            env.handle.metadata.pointer("/railway_service_id").and_then(Value::as_str),
            Some("svc-1"),
            "opaque handle metadata survives the round-trip"
        );
        assert!(!env.torn_down);
    }

    #[test]
    fn active_run_lookup_covers_pending_and_running_but_not_terminal_checkpoints() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        write_session_pending(scoped_root, "wf-lookup", "phase-a", "claude", "run-pending", None).expect("pending");
        write_session_pending(scoped_root, "wf-lookup", "phase-b", "claude", "run-running", None)
            .expect("second pending");
        update_session_running(scoped_root, "wf-lookup", "phase-b").expect("running");

        let pending = find_active_checkpoint_by_run_id(scoped_root, "run-pending")
            .expect("lookup pending")
            .expect("pending checkpoint");
        assert_eq!(pending.phase_id, "phase-a");
        let running = find_active_checkpoint_by_run_id(scoped_root, "run-running")
            .expect("lookup running")
            .expect("running checkpoint");
        assert_eq!(running.phase_id, "phase-b");

        update_session_failed(scoped_root, "wf-lookup", "phase-a", "finished").expect("terminalize");
        assert!(
            find_active_checkpoint_by_run_id(scoped_root, "run-pending").expect("lookup terminal").is_none(),
            "a stale terminal checkpoint must never claim a reused run id"
        );
    }

    // TASK-811/933: mark_environment_torn_down flips the flag and is
    // idempotent (a second call is a harmless no-op), and it never touches a
    // checkpoint that has no binding.
    #[test]
    fn mark_environment_torn_down_is_idempotent_and_binding_gated() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        write_session_pending(scoped_root, "wf-env-2", "phase-a", "claude", "run-2", None).expect("pending");

        // No binding yet: mark is a silent no-op (does not error, does not
        // fabricate a binding).
        mark_environment_torn_down(scoped_root, "wf-env-2", "phase-a").expect("no-op ok");
        let cp = read_checkpoint(scoped_root, "wf-env-2", "phase-a").expect("read").expect("present");
        assert!(cp.environment.is_none(), "mark with no binding must not fabricate one");

        update_session_environment(scoped_root, "wf-env-2", "phase-a", sample_binding()).expect("persist");
        mark_environment_torn_down(scoped_root, "wf-env-2", "phase-a").expect("mark");
        let cp = read_checkpoint(scoped_root, "wf-env-2", "phase-a").expect("read").expect("present");
        assert!(cp.environment.as_ref().expect("binding").torn_down, "torn_down set after first mark");

        // Idempotent: a second mark leaves it torn_down, no error, no change.
        mark_environment_torn_down(scoped_root, "wf-env-2", "phase-a").expect("second mark");
        let cp = read_checkpoint(scoped_root, "wf-env-2", "phase-a").expect("read").expect("present");
        assert!(cp.environment.expect("binding").torn_down, "torn_down stays set on the second mark");
    }

    #[test]
    fn workflow_cleanup_marks_terminal_phase_bindings_after_successful_retry() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        for phase_id in ["phase-a", "phase-b"] {
            write_session_pending(scoped_root, "wf-retry", phase_id, "claude", "run-retry", None).expect("pending");
            update_session_environment(scoped_root, "wf-retry", phase_id, sample_binding()).expect("binding");
        }
        update_session_completed(scoped_root, "wf-retry", "phase-a").expect("terminalize first phase");

        assert_eq!(mark_workflow_environments_torn_down(scoped_root, "wf-retry").expect("mark workflow"), 2);
        for phase_id in ["phase-a", "phase-b"] {
            let checkpoint = read_checkpoint(scoped_root, "wf-retry", phase_id).expect("read").expect("checkpoint");
            assert!(
                checkpoint.environment.expect("binding").torn_down,
                "cleanup retry must mark {phase_id}, regardless of terminal status"
            );
        }
        assert_eq!(mark_workflow_environments_torn_down(scoped_root, "wf-retry").expect("idempotent mark"), 0);
    }

    #[test]
    fn workflow_cleanup_ignores_non_checkpoint_json_files() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        write_session_pending(scoped_root, "wf-artifact", "phase-a", "claude", "run-artifact", None).expect("pending");
        update_session_environment(scoped_root, "wf-artifact", "phase-a", sample_binding()).expect("binding");
        let phases_dir = scoped_root.join("runs").join("wf-artifact").join("phases");
        fs::write(phases_dir.join("provider-output.json"), b"not a session checkpoint").expect("artifact");

        assert_eq!(mark_workflow_environments_torn_down(scoped_root, "wf-artifact").expect("mark workflow"), 1);
        let checkpoint = read_checkpoint(scoped_root, "wf-artifact", "phase-a").expect("read").expect("checkpoint");
        assert!(checkpoint.environment.expect("binding").torn_down);
    }

    #[test]
    fn workflow_checkpoint_listing_includes_historical_terminal_phases() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        for phase_id in ["removed-phase", "current-phase"] {
            write_session_pending(scoped_root, "wf-history", phase_id, "claude", phase_id, None).expect("pending");
        }
        update_session_completed(scoped_root, "wf-history", "removed-phase").expect("terminalize historical phase");
        let phases_dir = scoped_root.join("runs").join("wf-history").join("phases");
        fs::write(phases_dir.join("provider-output.json"), b"not a checkpoint").expect("artifact");

        let mut phase_ids = list_workflow_checkpoints(scoped_root, "wf-history")
            .expect("list checkpoints")
            .into_iter()
            .map(|checkpoint| checkpoint.phase_id)
            .collect::<Vec<_>>();
        phase_ids.sort();
        assert_eq!(phase_ids, ["current-phase", "removed-phase"]);
    }

    #[test]
    fn workflow_checkpoint_listing_rejects_mismatched_embedded_owner() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        write_session_pending(scoped_root, "wf-owner", "phase-a", "claude", "run-owner", None).expect("pending");

        let path = phase_session_path(scoped_root, "wf-owner", "phase-a");
        let mut checkpoint: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read checkpoint json")).expect("parse checkpoint");
        checkpoint["workflow_id"] = Value::String("wf-foreign".to_string());
        fs::write(&path, serde_json::to_vec_pretty(&checkpoint).expect("serialize checkpoint"))
            .expect("misplace foreign checkpoint");

        assert!(
            list_workflow_checkpoints(scoped_root, "wf-owner").expect("list owner checkpoints").is_empty(),
            "a checkpoint's embedded workflow owner must match the workflow directory being enumerated"
        );
    }

    #[test]
    fn workflow_handle_cleanup_marks_only_the_reaped_binding() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        let first = sample_binding();
        let mut second = sample_binding();
        second.handle.id = "node-other".to_string();
        for (phase_id, binding) in [("phase-a", first.clone()), ("phase-b", second)] {
            write_session_pending(scoped_root, "wf-reap", phase_id, "claude", "run-reap", None).expect("pending");
            update_session_environment(scoped_root, "wf-reap", phase_id, binding).expect("binding");
        }
        let phases_dir = scoped_root.join("runs").join("wf-reap").join("phases");
        fs::write(phases_dir.join("provider-output.json"), b"not a session checkpoint").expect("artifact");

        assert_eq!(
            mark_workflow_environment_torn_down(scoped_root, "wf-reap", &first.environment_id, &first.handle,)
                .expect("mark exact handle"),
            1
        );
        let first_checkpoint =
            read_checkpoint(scoped_root, "wf-reap", "phase-a").expect("read").expect("first checkpoint");
        let second_checkpoint =
            read_checkpoint(scoped_root, "wf-reap", "phase-b").expect("read").expect("second checkpoint");
        assert!(first_checkpoint.environment.expect("first binding").torn_down);
        assert!(!second_checkpoint.environment.expect("second binding").torn_down);
    }

    #[test]
    fn workflow_handle_cleanup_includes_environment_plugin_identity() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        let binding = sample_binding();
        write_session_pending(scoped_root, "wf-plugin", "phase-a", "claude", "run-plugin", None).expect("pending");
        update_session_environment(scoped_root, "wf-plugin", "phase-a", binding.clone()).expect("binding");

        assert_eq!(
            mark_workflow_environment_torn_down(
                scoped_root,
                "wf-plugin",
                "different-environment-plugin",
                &binding.handle,
            )
            .expect("ignore a handle owned by another plugin"),
            0
        );
        let checkpoint = read_checkpoint(scoped_root, "wf-plugin", "phase-a").expect("read").expect("checkpoint");
        assert!(
            !checkpoint.environment.expect("binding").torn_down,
            "an opaque handle is scoped to its environment plugin"
        );
    }

    // TASK-811: terminalizing a dead delegation must not discard the durable
    // handle or its cleanup state. Reconciliation can fail the checkpoint
    // after teardown, and operators still need the retained binding for audit
    // while later scans must see that the node has already been reaped.
    #[test]
    fn failing_checkpoint_preserves_torn_down_environment_binding() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        write_session_pending(scoped_root, "wf-env-3", "phase-a", "claude", "run-3", None).expect("pending");

        let binding = sample_binding();
        update_session_environment(scoped_root, "wf-env-3", "phase-a", binding.clone()).expect("persist binding");
        mark_environment_torn_down(scoped_root, "wf-env-3", "phase-a").expect("mark torn down");
        update_session_failed(scoped_root, "wf-env-3", "phase-a", "delegated node died").expect("fail checkpoint");

        let cp = read_checkpoint(scoped_root, "wf-env-3", "phase-a").expect("read").expect("present");
        assert_eq!(cp.status, SessionCheckpointStatus::Failed);
        assert_eq!(cp.blocked_reason.as_deref(), Some("delegated node died"));
        let environment = cp.environment.expect("terminal checkpoint retains environment binding");
        assert_eq!(environment.handle, binding.handle);
        assert_eq!(environment.environment_id, binding.environment_id);
        assert!(environment.torn_down, "terminal mutation retains the completed cleanup marker");
    }

    // TASK-933: after reconciliation reaps an unusable node, a phase-boundary
    // redispatch may prepare a replacement for the same checkpoint. Persisting
    // that replacement must overwrite the old handle AND clear its completed
    // teardown marker, otherwise the replacement is invisible to subsequent
    // restart recovery and can leak.
    #[test]
    fn replacement_environment_binding_supersedes_torn_down_node() {
        let temp = tempdir().expect("tempdir");
        let scoped_root = temp.path();
        write_session_pending(scoped_root, "wf-env-4", "phase-a", "claude", "run-4", None).expect("pending");

        update_session_environment(scoped_root, "wf-env-4", "phase-a", sample_binding()).expect("persist first");
        mark_environment_torn_down(scoped_root, "wf-env-4", "phase-a").expect("mark first torn down");

        let mut replacement = sample_binding();
        replacement.handle.id = "node-2".to_string();
        replacement.bound_at = "2025-01-02T00:00:00Z".to_string();
        // Model a caller deriving the replacement from the old checkpoint.
        // The binding writer owns the live-lease invariant and must clear this
        // stale cleanup marker rather than hiding the replacement from restart
        // reconciliation.
        replacement.torn_down = true;
        update_session_environment(scoped_root, "wf-env-4", "phase-a", replacement.clone())
            .expect("persist replacement");

        let cp = read_checkpoint(scoped_root, "wf-env-4", "phase-a").expect("read").expect("present");
        let persisted = cp.environment.expect("replacement binding");
        assert_eq!(persisted.handle, replacement.handle);
        assert_eq!(persisted.environment_id, replacement.environment_id);
        assert_eq!(persisted.bound_at, replacement.bound_at);
        assert!(!persisted.torn_down);
    }

    // Backward-compat: a checkpoint JSON written by an OLDER runner that does
    // not know the `environment` field must still deserialize (serde ignores
    // unknown fields; the struct is not deny_unknown_fields), with
    // environment defaulting to None.
    #[test]
    fn legacy_checkpoint_without_environment_field_deserializes() {
        let legacy = serde_json::json!({
            "workflow_id": "wf-legacy",
            "phase_id": "phase-a",
            "provider": "claude",
            "run_id": "run-legacy",
            "status": "running",
            "started_at": Utc::now().to_rfc3339(),
        });
        let cp: SessionCheckpoint =
            serde_json::from_value(legacy).expect("legacy checkpoint without `environment` must deserialize");
        assert!(cp.environment.is_none(), "missing environment field defaults to None");
        assert_eq!(cp.status, SessionCheckpointStatus::Running);
    }
}
