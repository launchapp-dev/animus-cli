use serde::{Deserialize, Serialize};
use serde_json::Value;
use workflow_runner_v2::PhaseExecutionEvent;

use crate::DispatchSelectionSource;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateChangeEvent {
    pub task_id: String,
    pub from_status: String,
    pub to_status: String,
    pub changed_at: String,
    pub selection_source: Option<DispatchSelectionSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTickSummary {
    pub project_root: String,
    pub started_daemon: bool,
    pub health: Value,
    pub tasks_total: usize,
    pub tasks_ready: usize,
    pub tasks_in_progress: usize,
    pub tasks_blocked: usize,
    pub tasks_done: usize,
    pub stale_in_progress_count: usize,
    pub stale_in_progress_threshold_hours: u64,
    pub stale_in_progress_task_ids: Vec<String>,
    pub workflows_running: usize,
    pub workflows_completed: usize,
    pub workflows_failed: usize,
    pub resumed_workflows: usize,
    pub cleaned_stale_workflows: usize,
    pub reconciled_workflows: usize,
    pub started_ready_workflows: usize,
    pub executed_workflow_phases: usize,
    pub failed_workflow_phases: usize,
    pub task_state_changes: Vec<TaskStateChangeEvent>,
    pub phase_execution_events: Vec<PhaseExecutionEvent>,
}
