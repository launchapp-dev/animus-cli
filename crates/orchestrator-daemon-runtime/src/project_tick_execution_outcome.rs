use workflow_runner::executor::PhaseExecutionEvent;

use crate::DispatchWorkflowStartSummary;

#[derive(Debug, Clone)]
pub struct ProjectTickExecutionOutcome {
    pub cleaned_stale_workflows: usize,
    pub resumed_workflows: usize,
    pub reconciled_stale_tasks: usize,
    pub reconciled_dependency_tasks: usize,
    pub reconciled_merge_tasks: usize,
    pub ready_workflow_starts: DispatchWorkflowStartSummary,
    pub executed_workflow_phases: usize,
    pub failed_workflow_phases: usize,
    pub phase_execution_events: Vec<PhaseExecutionEvent>,
}

impl Default for ProjectTickExecutionOutcome {
    fn default() -> Self {
        Self {
            cleaned_stale_workflows: 0,
            resumed_workflows: 0,
            reconciled_stale_tasks: 0,
            reconciled_dependency_tasks: 0,
            reconciled_merge_tasks: 0,
            ready_workflow_starts: DispatchWorkflowStartSummary::default(),
            executed_workflow_phases: 0,
            failed_workflow_phases: 0,
            phase_execution_events: Vec::new(),
        }
    }
}
