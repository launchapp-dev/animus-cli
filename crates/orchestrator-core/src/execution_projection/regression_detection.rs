use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{services::ServiceHub, TaskStatus};

use super::project_task_status;

pub const REGRESSION_WINDOW_SECS: i64 = 86400;
pub const REGRESSION_FAILURE_THRESHOLD: u32 = 2;

const REGRESSION_TRACKING_FILE_NAME: &str = "regression-tracking.json";
const REGRESSION_FIX_TAGS: &[&str] = &["auto-optimizer", "fix", "regression-fix"];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegressionState {
    #[serde(default)]
    pub fixes: HashMap<String, TrackedFix>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedFix {
    pub task_id: String,
    pub workflow_ref: String,
    pub completed_at: DateTime<Utc>,
    #[serde(default)]
    pub post_fix_failures: u32,
    #[serde(default)]
    pub post_fix_successes: u32,
    #[serde(default)]
    pub regression_detected: bool,
}

fn regression_tracking_path(project_root: &Path) -> PathBuf {
    let scoped_root = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".ao"));
    scoped_root.join("state").join(REGRESSION_TRACKING_FILE_NAME)
}

pub fn load_regression_state(project_root: &Path) -> Result<RegressionState> {
    let path = regression_tracking_path(project_root);
    if !path.exists() {
        return Ok(RegressionState::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read regression state from {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse regression state from {}", path.display()))
}

fn save_regression_state(project_root: &Path, state: &RegressionState) -> Result<()> {
    let path = regression_tracking_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create regression state directory {}", parent.display()))?;
    }
    let payload = serde_json::to_string_pretty(state)?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write regression state to {}", path.display()))
}

fn has_fix_tag(tags: &[String]) -> bool {
    tags.iter().any(|tag| {
        let normalized = tag.trim().to_ascii_lowercase();
        REGRESSION_FIX_TAGS.contains(&normalized.as_str())
    })
}

pub async fn record_fix_completion(
    hub: Arc<dyn ServiceHub>,
    project_root: &Path,
    task_id: &str,
    workflow_ref: &str,
) {
    let task = match hub.tasks().get(task_id).await {
        Ok(task) => task,
        Err(_) => return,
    };
    if !has_fix_tag(&task.tags) {
        return;
    }

    let mut state = load_regression_state(project_root).unwrap_or_default();

    let cutoff = Utc::now() - chrono::Duration::seconds(REGRESSION_WINDOW_SECS);
    state.fixes.retain(|_, fix| fix.completed_at > cutoff);

    state.fixes.insert(
        task_id.to_string(),
        TrackedFix {
            task_id: task_id.to_string(),
            workflow_ref: workflow_ref.to_string(),
            completed_at: Utc::now(),
            post_fix_failures: 0,
            post_fix_successes: 0,
            regression_detected: false,
        },
    );

    if let Err(err) = save_regression_state(project_root, &state) {
        eprintln!("{}: failed to save regression state: {}", protocol::ACTOR_DAEMON, err);
    } else {
        eprintln!(
            "{}: regression tracking: recorded fix completion for task {} (workflow_ref={})",
            protocol::ACTOR_DAEMON,
            task_id,
            workflow_ref
        );
    }
}

pub async fn check_regression_on_failure(
    hub: Arc<dyn ServiceHub>,
    project_root: &Path,
    failing_task_id: &str,
    workflow_ref: &str,
) {
    let mut state = match load_regression_state(project_root) {
        Ok(s) => s,
        Err(_) => return,
    };
    if state.fixes.is_empty() {
        return;
    }

    let cutoff = Utc::now() - chrono::Duration::seconds(REGRESSION_WINDOW_SECS);
    let mut state_changed = false;
    let mut regressions: Vec<String> = Vec::new();

    for fix in state.fixes.values_mut() {
        if fix.regression_detected {
            continue;
        }
        if fix.completed_at <= cutoff {
            continue;
        }
        if fix.workflow_ref != workflow_ref {
            continue;
        }
        if fix.task_id == failing_task_id {
            continue;
        }
        fix.post_fix_failures = fix.post_fix_failures.saturating_add(1);
        state_changed = true;
        if fix.post_fix_failures >= REGRESSION_FAILURE_THRESHOLD {
            fix.regression_detected = true;
            regressions.push(fix.task_id.clone());
        }
    }

    if state_changed {
        let _ = save_regression_state(project_root, &state);
    }

    for task_id in regressions {
        eprintln!(
            "{}: regression detected! Task {} fix may have regressed (workflow_ref={}, {} post-fix failures); reopening task",
            protocol::ACTOR_DAEMON,
            task_id,
            workflow_ref,
            REGRESSION_FAILURE_THRESHOLD
        );
        let _ = project_task_status(hub.clone(), &task_id, TaskStatus::Backlog).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_regression_state_returns_default() {
        let temp = tempdir().expect("tempdir");
        let state = load_regression_state(temp.path()).expect("load default state");
        assert!(state.fixes.is_empty());
    }

    #[test]
    fn has_fix_tag_detects_auto_optimizer_tag() {
        let tags = vec!["auto-optimizer".to_string(), "high".to_string()];
        assert!(has_fix_tag(&tags));
    }

    #[test]
    fn has_fix_tag_detects_fix_tag() {
        let tags = vec!["fix".to_string()];
        assert!(has_fix_tag(&tags));
    }

    #[test]
    fn has_fix_tag_rejects_unrelated_tags() {
        let tags = vec!["feature".to_string(), "frontend".to_string()];
        assert!(!has_fix_tag(&tags));
    }

    #[test]
    fn save_and_load_regression_state_round_trip() {
        let temp = tempdir().expect("tempdir");
        let mut state = RegressionState::default();
        state.fixes.insert(
            "TASK-1".to_string(),
            TrackedFix {
                task_id: "TASK-1".to_string(),
                workflow_ref: "implementation".to_string(),
                completed_at: Utc::now(),
                post_fix_failures: 1,
                post_fix_successes: 0,
                regression_detected: false,
            },
        );
        save_regression_state(temp.path(), &state).expect("save state");
        let loaded = load_regression_state(temp.path()).expect("load state");
        assert_eq!(loaded.fixes.len(), 1);
        let fix = loaded.fixes.get("TASK-1").expect("fix should exist");
        assert_eq!(fix.workflow_ref, "implementation");
        assert_eq!(fix.post_fix_failures, 1);
        assert!(!fix.regression_detected);
    }
}
