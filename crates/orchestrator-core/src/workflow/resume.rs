use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{OrchestratorWorkflow, WorkflowStatus};

use super::state_manager::WorkflowStateManager;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeConfig {
    pub max_age_hours: i64,
    pub auto_resume_enabled: bool,
    pub resume_paused: bool,
    pub resume_failed: bool,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        Self { max_age_hours: 24, auto_resume_enabled: true, resume_paused: true, resume_failed: false }
    }
}

impl ResumeConfig {
    pub fn load(project_root: &Path) -> Result<Self> {
        let base = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".ao"));
        let config_path = base.join("resume-config.json");
        if !config_path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(config_path)?;
        Ok(serde_json::from_str(&content)?)
    }
}

#[derive(Debug, Clone)]
pub enum ResumabilityStatus {
    Resumable { workflow_id: String, reason: String },
    Stale { workflow_id: String, age_hours: i64, max_age_hours: i64 },
    InvalidState { workflow_id: String, status: WorkflowStatus, reason: String },
}

impl ResumabilityStatus {
    pub fn is_resumable(&self) -> bool {
        matches!(self, ResumabilityStatus::Resumable { .. })
    }
}

pub struct WorkflowResumeManager {
    state_manager: WorkflowStateManager,
    pub config: ResumeConfig,
}

impl WorkflowResumeManager {
    pub fn new(project_root: impl Into<PathBuf>) -> Result<Self> {
        let project_root = project_root.into();
        let config = ResumeConfig::load(&project_root)?;
        let state_manager = WorkflowStateManager::new(project_root);
        Ok(Self { state_manager, config })
    }

    pub fn detect_interrupted_workflows(&self) -> Result<Vec<OrchestratorWorkflow>> {
        let all = self.state_manager.list()?;
        Ok(all
            .into_iter()
            .filter(|workflow| match workflow.status {
                WorkflowStatus::Running => true,
                WorkflowStatus::Paused if self.config.resume_paused => true,
                WorkflowStatus::Failed if self.config.resume_failed => true,
                WorkflowStatus::Escalated => true,
                WorkflowStatus::Pending
                | WorkflowStatus::Paused
                | WorkflowStatus::Completed
                | WorkflowStatus::Failed
                | WorkflowStatus::Cancelled => false,
            })
            .collect())
    }

    pub fn validate_resumability(&self, workflow: &OrchestratorWorkflow) -> ResumabilityStatus {
        let now = Utc::now();
        let age = now.signed_duration_since(workflow.started_at);
        let max_age = ChronoDuration::hours(self.config.max_age_hours);

        if age > max_age {
            return ResumabilityStatus::Stale {
                workflow_id: workflow.id.clone(),
                age_hours: age.num_hours(),
                max_age_hours: self.config.max_age_hours,
            };
        }

        match workflow.status {
            WorkflowStatus::Completed => ResumabilityStatus::InvalidState {
                workflow_id: workflow.id.clone(),
                status: workflow.status,
                reason: "workflow already completed".to_string(),
            },
            WorkflowStatus::Cancelled => ResumabilityStatus::InvalidState {
                workflow_id: workflow.id.clone(),
                status: workflow.status,
                reason: "workflow cancelled".to_string(),
            },
            _ => ResumabilityStatus::Resumable {
                workflow_id: workflow.id.clone(),
                reason: format!(
                    "workflow {:?} can resume from machine state {:?}",
                    workflow.status, workflow.machine_state
                ),
            },
        }
    }

    pub fn get_resumable_workflows(&self) -> Result<Vec<(OrchestratorWorkflow, ResumabilityStatus)>> {
        let interrupted = self.detect_interrupted_workflows()?;
        Ok(interrupted
            .into_iter()
            .filter_map(|workflow| {
                let status = self.validate_resumability(&workflow);
                if status.is_resumable() {
                    Some((workflow, status))
                } else {
                    None
                }
            })
            .collect())
    }
}
