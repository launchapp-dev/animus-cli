//! Zombie phase session detection.
//!
//! Workflow execution writes per-phase JSON files at
//! `~/.animus/<repo-scope>/runs/<workflow_id>/phases/<phase_id>.session.json`.
//! A session whose `status` is still `running` long after `started_at`
//! indicates the daemon (or provider) crashed mid-phase. The doctor
//! detects these and, when `--fix` is requested, normalizes the status
//! to `failed` and records the close-out note in `blocked_reason` — so
//! `list_running_checkpoints` (which deserializes via
//! `SessionCheckpointStatus`: `pending|running|completed|failed|blocked`)
//! continues to parse the file cleanly. Writing an unknown status value
//! would make the next daemon scan skip every running checkpoint.
//!
//! Threshold: 6 hours. Anything shorter risks racing legitimately
//! long-running phases (research / large refactors regularly run > 1h).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "workflow_state";
const STALE_RUNNING_THRESHOLD: Duration = Duration::from_hours(6);

pub(crate) fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();

    let Some(runs_dir) = runs_dir(&ctx.project_root) else {
        out.push(
            DiagnosticCheck::new(
                "zombie_phase_sessions",
                CATEGORY,
                CheckStatus::Skipped,
                "Zombie running phase sessions",
            )
            .details("scoped state root unavailable (no HOME)"),
        );
        return out;
    };

    if !runs_dir.exists() {
        out.push(
            DiagnosticCheck::new("zombie_phase_sessions", CATEGORY, CheckStatus::Pass, "Zombie running phase sessions")
                .details(format!("no runs directory at {}", runs_dir.display())),
        );
        return out;
    }

    let zombies = collect_zombie_phase_sessions(&runs_dir);
    if zombies.is_empty() {
        out.push(
            DiagnosticCheck::new("zombie_phase_sessions", CATEGORY, CheckStatus::Pass, "Zombie running phase sessions")
                .details(format!(
                    "no phase session JSON with status=running older than {}h",
                    STALE_RUNNING_THRESHOLD.as_secs() / 3600
                )),
        );
        return out;
    }

    let preview = zombies.iter().take(3).map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
    out.push(
        DiagnosticCheck::new("zombie_phase_sessions", CATEGORY, CheckStatus::Warn, "Zombie running phase sessions")
            .current(format!("{} stale session(s): {preview}", zombies.len()))
            .expected("no phase sessions stuck in status=running".to_string())
            .fix(CheckFix::auto_no_command(
                "normalize_zombie_phase_sessions",
                "Rewrite each session JSON to set status=failed, stamp completed_at=now, and record the close-out note in blocked_reason.",
            )),
    );

    out
}

pub(crate) fn collect_zombie_for_fix(project_root: &Path) -> Vec<PathBuf> {
    let Some(runs_dir) = runs_dir(project_root) else {
        return Vec::new();
    };
    if !runs_dir.exists() {
        return Vec::new();
    }
    collect_zombie_phase_sessions(&runs_dir)
}

/// Outcome of attempting to normalize a single session file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NormalizeOutcome {
    /// Rewrote the file from running → failed.
    Rewritten,
    /// The file is no longer a zombie (someone else completed it, or it
    /// is no longer old enough). The race is benign — we skip silently.
    NoLongerZombie,
}

/// Idempotent close-out for a stuck phase session.
///
/// Read-validate-write must operate on the same bytes: if we read once,
/// re-check the file separately, and then write the original Value, the
/// daemon can update the file between the two reads and we'd persist
/// the stale snapshot (overwriting e.g. a freshly-captured
/// provider_session_id). The single-read variant of the zombie
/// predicate (`is_zombie_value`) closes the race window — what we
/// validate is exactly what we mutate.
pub(crate) fn normalize_session_file(path: &Path) -> std::io::Result<NormalizeOutcome> {
    let raw = std::fs::read_to_string(path)?;
    let mut value: Value = serde_json::from_str(&raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("invalid session JSON: {e}")))?;
    if !is_zombie_value(&value, path, &SystemTime::now()) {
        return Ok(NormalizeOutcome::NoLongerZombie);
    }
    let now = Utc::now().to_rfc3339();
    if let Some(obj) = value.as_object_mut() {
        // `SessionCheckpointStatus` accepts pending/running/completed/failed/blocked.
        // We use `failed` (not "crashed") so the runtime's checkpoint
        // scanner can keep deserializing the file.
        obj.insert("status".to_string(), Value::String("failed".to_string()));
        obj.insert("completed_at".to_string(), Value::String(now.clone()));
        let note = format!("doctor close-out: phase was stuck in status=running at {now}");
        obj.insert("blocked_reason".to_string(), Value::String(note));
    }
    let serialized =
        serde_json::to_string_pretty(&value).map_err(|e| std::io::Error::other(format!("serialize failed: {e}")))?;
    std::fs::write(path, serialized)?;
    Ok(NormalizeOutcome::Rewritten)
}

fn runs_dir(project_root: &Path) -> Option<PathBuf> {
    Some(protocol::scoped_state_root(project_root)?.join("runs"))
}

fn collect_zombie_phase_sessions(runs_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let now = SystemTime::now();
    let Ok(entries) = std::fs::read_dir(runs_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let phases_dir = path.join("phases");
        if !phases_dir.is_dir() {
            continue;
        }
        let Ok(phase_entries) = std::fs::read_dir(&phases_dir) else {
            continue;
        };
        for phase_entry in phase_entries.flatten() {
            let phase_path = phase_entry.path();
            if !phase_path.is_file() {
                continue;
            }
            let Some(name) = phase_path.file_name().and_then(|n| n.to_str()) else { continue };
            if !name.ends_with(".session.json") {
                continue;
            }
            if is_zombie_session(&phase_path, &now) {
                out.push(phase_path);
            }
        }
    }
    out
}

fn is_zombie_session(path: &Path, now: &SystemTime) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else { return false };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else { return false };
    is_zombie_value(&value, path, now)
}

/// Pure predicate over an already-parsed session JSON. Splitting this
/// out lets `normalize_session_file` validate exactly the bytes it is
/// about to mutate, eliminating a read-after-read race that could let
/// the daemon's update get clobbered.
fn is_zombie_value(value: &Value, path: &Path, now: &SystemTime) -> bool {
    let status = value.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "running" {
        return false;
    }
    let started_at = value.get("started_at").and_then(|v| v.as_str()).unwrap_or("");
    let started: Option<SystemTime> =
        DateTime::parse_from_rfc3339(started_at).ok().map(|dt| dt.with_timezone(&Utc).into());
    if let Some(started) = started {
        return now.duration_since(started).map(|d| d >= STALE_RUNNING_THRESHOLD).unwrap_or(false);
    }
    // No parseable started_at — fall back to mtime so we don't strand
    // truly broken files. Anything older than the threshold and still
    // marked running gets normalized.
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .map(|d| d >= STALE_RUNNING_THRESHOLD)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::test_utils::EnvVarGuard;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner())
    }

    fn write_session(path: &Path, status: &str, started_at: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let payload = serde_json::json!({
            "workflow_id": "wf-test",
            "phase_id": "implement",
            "status": status,
            "started_at": started_at,
        });
        std::fs::write(path, serde_json::to_string_pretty(&payload).unwrap()).unwrap();
    }

    #[test]
    fn detects_zombie_running_session_older_than_threshold() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let runs = protocol::scoped_state_root(&project).unwrap().join("runs");
        let phase = runs.join("wf-1").join("phases").join("implement.session.json");
        write_session(&phase, "running", "2020-01-01T00:00:00+00:00");

        let ctx = CheckContext { project_root: project.clone(), skip_subprocess: true };
        let checks = run(&ctx);
        assert_eq!(checks[0].status, CheckStatus::Warn);

        let zombies = collect_zombie_for_fix(&project);
        assert_eq!(zombies.len(), 1);
        assert_eq!(zombies[0], phase);
    }

    #[test]
    fn ignores_fresh_running_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project2");
        std::fs::create_dir_all(&project).unwrap();
        let runs = protocol::scoped_state_root(&project).unwrap().join("runs");
        let phase = runs.join("wf-2").join("phases").join("implement.session.json");
        let now = chrono::Utc::now().to_rfc3339();
        write_session(&phase, "running", &now);

        let zombies = collect_zombie_for_fix(&project);
        assert!(zombies.is_empty());
    }

    #[test]
    fn ignores_completed_session_regardless_of_age() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = lock();
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project = temp.path().join("project3");
        std::fs::create_dir_all(&project).unwrap();
        let runs = protocol::scoped_state_root(&project).unwrap().join("runs");
        let phase = runs.join("wf-3").join("phases").join("implement.session.json");
        write_session(&phase, "completed", "2020-01-01T00:00:00+00:00");

        let zombies = collect_zombie_for_fix(&project);
        assert!(zombies.is_empty());
    }

    #[test]
    fn normalize_session_file_marks_failed_and_stamps_completed_at() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("implement.session.json");
        write_session(&path, "running", "2020-01-01T00:00:00+00:00");

        let outcome = normalize_session_file(&path).expect("normalize");
        assert_eq!(outcome, NormalizeOutcome::Rewritten);
        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            value.get("status").and_then(|v| v.as_str()),
            Some("failed"),
            "must use a SessionCheckpointStatus enum variant so the runtime can still deserialize the file",
        );
        assert!(value.get("completed_at").and_then(|v| v.as_str()).is_some());
        let blocked = value.get("blocked_reason").and_then(|v| v.as_str()).expect("blocked_reason populated");
        assert!(blocked.contains("doctor close-out"));
    }

    #[test]
    fn normalize_session_file_output_still_deserializes_as_session_checkpoint() {
        use animus_runtime_shared::phase_session::{SessionCheckpoint, SessionCheckpointStatus};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("implement.session.json");
        // Write a payload that matches the real on-disk SessionCheckpoint
        // shape (provider, run_id) so the deserialization round-trip is
        // exercised end to end.
        let payload = serde_json::json!({
            "workflow_id": "wf-cp-roundtrip",
            "phase_id": "implement",
            "provider": "claude",
            "run_id": "wf-cp-roundtrip-implement",
            "status": "running",
            "started_at": "2020-01-01T00:00:00+00:00",
        });
        std::fs::write(&path, serde_json::to_string_pretty(&payload).unwrap()).unwrap();

        let outcome = normalize_session_file(&path).expect("normalize");
        assert_eq!(outcome, NormalizeOutcome::Rewritten);
        let raw = std::fs::read_to_string(&path).unwrap();
        let cp: SessionCheckpoint = serde_json::from_str(&raw).expect("must round-trip through SessionCheckpoint");
        assert_eq!(cp.status, SessionCheckpointStatus::Failed);
        assert!(cp.completed_at.is_some());
        assert!(cp.blocked_reason.as_deref().unwrap_or("").contains("doctor close-out"));
    }

    #[test]
    fn normalize_session_file_skips_when_daemon_already_completed_session() {
        // Race repro: doctor collected the path while status was running
        // and stale, then the daemon finished the phase before the doctor
        // rewrite. We must not stomp the daemon's outcome.
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("implement.session.json");
        write_session(&path, "completed", "2020-01-01T00:00:00+00:00");

        let outcome = normalize_session_file(&path).expect("normalize");
        assert_eq!(outcome, NormalizeOutcome::NoLongerZombie);
        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            value.get("status").and_then(|v| v.as_str()),
            Some("completed"),
            "must not overwrite a session the daemon already finished",
        );
    }

    #[test]
    fn normalize_session_file_skips_when_session_was_resumed_inside_threshold() {
        // The daemon may resume an old session and update `started_at`
        // back inside the threshold while doctor was iterating. The
        // re-check should preserve the fresh running state.
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("implement.session.json");
        let now = chrono::Utc::now().to_rfc3339();
        write_session(&path, "running", &now);

        let outcome = normalize_session_file(&path).expect("normalize");
        assert_eq!(outcome, NormalizeOutcome::NoLongerZombie);
        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value.get("status").and_then(|v| v.as_str()), Some("running"));
    }
}
