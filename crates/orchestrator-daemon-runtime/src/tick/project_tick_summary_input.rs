use orchestrator_core::{OrchestratorTask, RequirementItem};
use serde_json::Value;
use workflow_runner_v2::PhaseExecutionEvent;

use crate::DispatchWorkflowStart;

#[derive(Debug, Clone)]
pub struct ProjectTickSummaryInput {
    pub project_root: String,
    pub started_daemon: bool,
    pub health: Value,
    pub requirements_before: Vec<RequirementItem>,
    pub tasks_before: Vec<OrchestratorTask>,
    pub resumed_workflows: usize,
    pub cleaned_stale_workflows: usize,
    pub reconciled_workflows: usize,
    pub reconciled_dependency_tasks: usize,
    pub reconciled_merge_tasks: usize,
    pub ready_started_count: usize,
    pub ready_started_workflows: Vec<DispatchWorkflowStart>,
    pub executed_workflow_phases: usize,
    pub failed_workflow_phases: usize,
    pub phase_execution_events: Vec<PhaseExecutionEvent>,
}
