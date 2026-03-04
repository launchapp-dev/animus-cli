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
}

fn schedule_state_path(project_root: &Path) -> PathBuf {
    let scoped_root = protocol::scoped_state_root(project_root)
        .unwrap_or_else(|| project_root.join(".ao"));
    scoped_root.join("state").join(SCHEDULE_STATE_FILE_NAME)
}

pub fn load_schedule_state(project_root: &Path) -> Result<ScheduleState> {
    let path = schedule_state_path(project_root);
    if !path.exists() {
        return Ok(ScheduleState::default());
    }

    let raw = std::fs::read_to_string(&path).with_context(|| {
        format!("failed to read schedule state from {}", path.display())
    })?;
    serde_json::from_str(&raw).with_context(|| {
        format!("failed to parse schedule state JSON from {}", path.display())
    })
}

pub fn save_schedule_state(project_root: &Path, state: &ScheduleState) -> Result<()> {
    let path = schedule_state_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create schedule state directory {}",
                parent.display()
            )
        })?;
    }
    let payload = serde_json::to_string_pretty(state)?;
    std::fs::write(&path, payload).with_context(|| {
        format!("failed to write schedule state to {}", path.display())
    })
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
