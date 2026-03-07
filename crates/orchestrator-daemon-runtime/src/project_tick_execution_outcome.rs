use workflow_runner::executor::PhaseExecutionEvent;

use crate::{ProjectTickActionEffect, ReadyTaskWorkflowStartSummary};

#[derive(Debug, Clone)]
pub struct ProjectTickExecutionOutcome {
    pub cleaned_stale_workflows: usize,
    pub resumed_workflows: usize,
    pub reconciled_stale_tasks: usize,
    pub reconciled_dependency_tasks: usize,
    pub reconciled_merge_tasks: usize,
    pub ready_workflow_starts: ReadyTaskWorkflowStartSummary,
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
            ready_workflow_starts: ReadyTaskWorkflowStartSummary::default(),
            executed_workflow_phases: 0,
            failed_workflow_phases: 0,
            phase_execution_events: Vec::new(),
        }
    }
}

impl ProjectTickExecutionOutcome {
    pub fn apply_effect(&mut self, effect: ProjectTickActionEffect) {
        match effect {
            ProjectTickActionEffect::Noop => {}
            ProjectTickActionEffect::ResumedInterrupted {
                cleaned_stale_workflows,
                resumed_workflows,
            } => {
                self.cleaned_stale_workflows = cleaned_stale_workflows;
                self.resumed_workflows = resumed_workflows;
            }
            ProjectTickActionEffect::ReconciledStaleTasks { count } => {
                self.reconciled_stale_tasks = count;
            }
            ProjectTickActionEffect::ReconciledDependencyTasks { count } => {
                self.reconciled_dependency_tasks = count;
            }
            ProjectTickActionEffect::ReconciledMergeTasks { count } => {
                self.reconciled_merge_tasks = count;
            }
            ProjectTickActionEffect::ReconciledCompletedProcesses {
                executed_workflow_phases,
                failed_workflow_phases,
            } => {
                self.executed_workflow_phases = executed_workflow_phases;
                self.failed_workflow_phases = failed_workflow_phases;
            }
            ProjectTickActionEffect::ReadyWorkflowStarts { summary } => {
                self.ready_workflow_starts = summary;
            }
        }
    }
}
