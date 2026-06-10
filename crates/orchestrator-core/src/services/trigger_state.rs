use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const TRIGGER_STATE_FILE_NAME: &str = "trigger-state.json";
const TRIGGER_STATE_LOCK_FILE_NAME: &str = "trigger-state.lock";

/// Persisted state for all configured event triggers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TriggerState {
    /// Per-trigger run state, keyed by trigger id.
    #[serde(default)]
    pub triggers: HashMap<String, TriggerRunState>,
}

/// A webhook event received via HTTP that is pending dispatch by the next
/// daemon tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEvent {
    /// Unique event identifier (UUID).  Used to detect duplicate deliveries.
    pub event_id: String,
    /// UTC timestamp when the event was received by the HTTP handler.
    pub received_at: DateTime<Utc>,
    /// Raw request body forwarded as the workflow input payload.
    pub payload: serde_json::Value,
}

/// Per-trigger run state shared across trigger types.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TriggerRunState {
    /// When the trigger last successfully dispatched a pipeline.
    #[serde(default)]
    pub last_dispatched: Option<DateTime<Utc>>,
    /// Status of the last dispatch attempt.
    #[serde(default)]
    pub last_status: String,
    /// Number of times this trigger has dispatched.
    #[serde(default)]
    pub dispatch_count: u64,
    /// Type-specific extra state (e.g. mtime for file watchers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
    /// Webhook events received via HTTP and awaiting dispatch on the next tick.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_events: Vec<WebhookEvent>,
    /// Start of the current rate-limit window (rolling 60 s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_window_start: Option<DateTime<Utc>>,
    /// Number of events accepted in the current rate-limit window.
    #[serde(default)]
    pub rate_window_count: u32,
}

fn trigger_state_path(project_root: &Path) -> PathBuf {
    let scoped_root = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".animus"));
    scoped_root.join("state").join(TRIGGER_STATE_FILE_NAME)
}

/// Acquire an exclusive cross-process/cross-task lock guarding
/// load→mutate→save cycles on `trigger-state.json`. Hold the returned
/// `File` for the full read-modify-write; the lock releases on drop.
/// Multiple writers (trigger plugin event router, tick-loop trigger
/// dispatch, `animus trigger fire`) otherwise interleave and silently
/// lose or duplicate pending events.
pub fn lock_trigger_state(project_root: &Path) -> Result<std::fs::File> {
    let lock_path = trigger_state_path(project_root).with_file_name(TRIGGER_STATE_LOCK_FILE_NAME);
    lock_file_exclusive(&lock_path)
}

fn lock_file_exclusive(lock_path: &Path) -> Result<std::fs::File> {
    use fs2::FileExt;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create trigger state directory {}", parent.display()))?;
    }
    let file = std::fs::File::create(lock_path)
        .with_context(|| format!("failed to create trigger state lock file {}", lock_path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("failed to acquire trigger state lock at {}", lock_path.display()))?;
    Ok(file)
}

pub fn load_trigger_state(project_root: &Path) -> Result<TriggerState> {
    let path = trigger_state_path(project_root);
    if !path.exists() {
        return Ok(TriggerState::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read trigger state from {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse trigger state JSON from {}", path.display()))
}

// Trigger state holds the pending webhook event queue plus rate-limit
// windows. After a power-loss we want pending events to survive (so the
// next daemon tick can dispatch them) and we want the dispatch_count to
// stay monotone (so dedupe logic stays sane). Route through the durable
// helper which fsyncs the data file and the parent directory.
pub fn save_trigger_state(project_root: &Path, state: &TriggerState) -> Result<()> {
    let path = trigger_state_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create trigger state directory {}", parent.display()))?;
    }
    crate::store::write_json_pretty(&path, state)
        .with_context(|| format!("failed to write trigger state to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_trigger_state_returns_default() {
        let temp = tempdir().expect("tempdir");
        let loaded = load_trigger_state(temp.path()).expect("load default state");
        assert!(loaded.triggers.is_empty());
    }

    #[test]
    fn lock_trigger_state_is_exclusive() {
        use fs2::FileExt;
        let temp = tempdir().expect("tempdir");
        // Test the path-parameterized seam directly: lock_trigger_state's
        // path derivation goes through HOME, which other tests in this crate
        // swap process-wide.
        let lock_path = temp.path().join(super::TRIGGER_STATE_LOCK_FILE_NAME);
        let held = lock_file_exclusive(&lock_path).expect("first lock acquires");

        let contender = std::fs::OpenOptions::new().write(true).open(&lock_path).expect("open lock file");
        assert!(contender.try_lock_exclusive().is_err(), "second lock must be refused while the first is held");

        drop(held);
        // Release-on-close is not instantaneous under parallel test load on
        // macOS; only eventual acquirability is the contract.
        let mut reacquired = contender.try_lock_exclusive();
        for _ in 0..100 {
            if reacquired.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            reacquired = contender.try_lock_exclusive();
        }
        assert!(reacquired.is_ok(), "lock must be acquirable after the holder drops");
    }

    #[test]
    fn save_and_load_trigger_state_round_trip() {
        let temp = tempdir().expect("tempdir");
        let mut original = TriggerState::default();
        original.triggers.insert(
            "on-file-change".to_string(),
            TriggerRunState {
                last_dispatched: Some(Utc::now()),
                last_status: "dispatched".to_string(),
                dispatch_count: 2,
                ..Default::default()
            },
        );

        save_trigger_state(temp.path(), &original).expect("save state");
        let loaded = load_trigger_state(temp.path()).expect("load state");

        assert_eq!(loaded.triggers.len(), 1);
        let run_state = loaded.triggers.get("on-file-change").expect("trigger run state should exist");
        assert_eq!(run_state.last_status, "dispatched");
        assert_eq!(run_state.dispatch_count, 2);
        assert!(run_state.last_dispatched.is_some());
    }
}
