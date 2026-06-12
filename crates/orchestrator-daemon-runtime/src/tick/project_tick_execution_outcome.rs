use animus_runtime_shared::PhaseExecutionEvent;

use crate::{BudgetBreachEvent, DispatchWorkflowStartSummary, WorkflowFailureEvent};

#[derive(Debug, Clone, Default)]
pub struct ProjectTickExecutionOutcome {
    pub cleaned_stale_workflows: usize,
    pub resumed_workflows: usize,
    pub reconciled_workflows: usize,
    pub reconciled_dependency_tasks: usize,
    pub reconciled_merge_tasks: usize,
    pub ready_workflow_starts: DispatchWorkflowStartSummary,
    pub executed_workflow_phases: usize,
    pub failed_workflow_phases: usize,
    pub phase_execution_events: Vec<PhaseExecutionEvent>,
    pub workflow_failures: Vec<WorkflowFailureEvent>,
    pub budget_breaches: Vec<BudgetBreachEvent>,
}
