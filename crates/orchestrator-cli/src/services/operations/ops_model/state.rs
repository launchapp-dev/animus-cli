use super::super::project_state_dir;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::status::ModelStatusDtoCli;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ModelRosterStoreCli {
    pub(super) refreshed_at: String,
    pub(super) candidates: Vec<ModelStatusDtoCli>,
}

impl Default for ModelRosterStoreCli {
    fn default() -> Self {
        Self { refreshed_at: Utc::now().to_rfc3339(), candidates: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct ModelEvaluationReportCli {
    pub(super) report_id: String,
    pub(super) generated_at: String,
    pub(super) total: usize,
    pub(super) available: usize,
    pub(super) unavailable: usize,
    pub(super) statuses: Vec<ModelStatusDtoCli>,
}

pub(super) fn model_roster_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("model-roster.json")
}

pub(super) fn model_eval_report_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("model-eval-report.json")
}
