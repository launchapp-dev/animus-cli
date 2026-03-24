use std::path::Path;

use serde::{Deserialize, Serialize};

pub const REGRESSION_FAILURE_THRESHOLD: usize = 3;
pub const REGRESSION_WINDOW_SECS: u64 = 3600;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegressionState {
    pub tracked_fixes: Vec<TrackedFix>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedFix {
    pub task_id: String,
    pub fixed_at: String,
    pub phase_id: String,
}

pub fn check_regression_on_failure(
    _project_root: &Path,
    _task_id: &str,
    _phase_id: &str,
    _failure_reason: Option<&str>,
) -> Option<String> {
    None
}

pub fn load_regression_state(_project_root: &Path) -> RegressionState {
    RegressionState::default()
}

pub fn record_fix_completion(
    _project_root: &Path,
    _task_id: &str,
    _phase_id: &str,
) {
}
