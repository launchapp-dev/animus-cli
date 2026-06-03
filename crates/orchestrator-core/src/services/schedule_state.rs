use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const SCHEDULE_STATE_FILE_NAME: &str = "schedule-state.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScheduleState {
    #[serde(default)]
    pub schedules: HashMap<String, ScheduleRunState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScheduleRunState {
    #[serde(default)]
    pub last_run: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_status: String,
    #[serde(default)]
    pub run_count: u64,
    /// Number of times this schedule was due but could not be dispatched
    /// (e.g. ProcessManager refused the spawn because the pool was at the
    /// runtime quota). Separate from `run_count` so ops can distinguish
    /// "ran" from "missed because we ran out of slots".
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub missed_count: u64,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn schedule_state_path(project_root: &Path) -> PathBuf {
    let scoped_root = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".animus"));
    scoped_root.join("state").join(SCHEDULE_STATE_FILE_NAME)
}

pub fn load_schedule_state(project_root: &Path) -> Result<ScheduleState> {
    let path = schedule_state_path(project_root);
    if !path.exists() {
        return Ok(ScheduleState::default());
    }

    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read schedule state from {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse schedule state JSON from {}", path.display()))
}

// Schedule state drives cron-style trigger dispatch — losing the last_run
// timestamp after a power-loss would replay schedules and double-fire
// pipelines. Route through the durable write helper which sync_all's the
// data file and then fsync's the parent directory so the rename itself is
// crash-safe.
pub fn save_schedule_state(project_root: &Path, state: &ScheduleState) -> Result<()> {
    let path = schedule_state_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create schedule state directory {}", parent.display()))?;
    }
    crate::store::write_json_pretty(&path, state)
        .with_context(|| format!("failed to write schedule state to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_schedule_state_returns_default() {
        let temp = tempdir().expect("tempdir");
        let loaded = load_schedule_state(temp.path()).expect("load default state");
        assert!(loaded.schedules.is_empty());
    }

    #[test]
    fn save_and_load_schedule_state_round_trip() {
        let temp = tempdir().expect("tempdir");
        let mut original = ScheduleState::default();
        original.schedules.insert(
            "nightly".to_string(),
            ScheduleRunState {
                last_run: Some(Utc::now()),
                last_status: "completed".to_string(),
                run_count: 3,
                missed_count: 0,
            },
        );

        save_schedule_state(temp.path(), &original).expect("save state");
        let loaded = load_schedule_state(temp.path()).expect("load state");

        assert_eq!(loaded.schedules.len(), 1);
        let run_state = loaded.schedules.get("nightly").expect("run state should exist");
        assert_eq!(run_state.last_status, "completed");
        assert_eq!(run_state.run_count, 3);
        assert!(run_state.last_run.is_some());
    }
}
