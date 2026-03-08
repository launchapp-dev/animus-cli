use crate::ReadyTaskWorkflowStartSummary;

#[derive(Debug, Clone)]
pub enum ProjectTickActionEffect {
    ResumedInterrupted {
        cleaned_stale_workflows: usize,
        resumed_workflows: usize,
    },
    ReconciledStaleTasks {
        count: usize,
    },
    ReconciledDependencyTasks {
        count: usize,
    },
    ReconciledMergeTasks {
        count: usize,
    },
    ReconciledCompletedProcesses {
        executed_workflow_phases: usize,
        failed_workflow_phases: usize,
    },
    ReadyWorkflowStarts {
        summary: ReadyTaskWorkflowStartSummary,
    },
}
