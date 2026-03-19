use workflow_runner_v2::PhaseExecutionEvent;

use crate::DispatchWorkflowStartSummary;

#[derive(Debug, Clone, Default)]
pub struct ProjectTickExecutionOutcome {
    pub cleaned_stale_workflows: usize,
    pub resumed_workflows: usize,
    pub reconciled_workflows: usize,
    pub reconciled_dependency_tasks: usize,
    pub reconciled_merge_tasks: usize,
    pub reconciled_runner_blocked_tasks: usize,
    pub ready_workflow_starts: DispatchWorkflowStartSummary,
    pub executed_workflow_phases: usize,
    pub failed_workflow_phases: usize,
    pub phase_execution_events: Vec<PhaseExecutionEvent>,
}
