use orchestrator_core::{DaemonHealth, OrchestratorTask, RequirementItem};
use serde_json::Value;

use crate::{ProjectTickExecutionOutcome, ProjectTickSummaryInput};

#[derive(Debug, Clone)]
pub struct ProjectTickSnapshot {
    pub requirements_before: Vec<RequirementItem>,
    pub tasks_before: Vec<OrchestratorTask>,
    pub started_daemon: bool,
    pub daemon_health: Option<DaemonHealth>,
}

impl ProjectTickSnapshot {
    pub fn into_summary_input(
        self,
        project_root: String,
        health: Value,
        execution_outcome: ProjectTickExecutionOutcome,
        phase_execution_events: bool,
    ) -> ProjectTickSummaryInput {
        ProjectTickSummaryInput {
            project_root,
            started_daemon: self.started_daemon,
            health,
            requirements_before: self.requirements_before,
            tasks_before: self.tasks_before,
            resumed_workflows: execution_outcome.resumed_workflows,
            cleaned_stale_workflows: execution_outcome.cleaned_stale_workflows,
            reconciled_workflows: execution_outcome.reconciled_workflows,
            reconciled_dependency_tasks: execution_outcome.reconciled_dependency_tasks,
            reconciled_merge_tasks: execution_outcome.reconciled_merge_tasks,
            reconciled_runner_blocked_tasks: execution_outcome.reconciled_runner_blocked_tasks,
            ready_started_count: execution_outcome.ready_workflow_starts.started,
            ready_started_workflows: execution_outcome.ready_workflow_starts.started_workflows,
            executed_workflow_phases: execution_outcome.executed_workflow_phases,
            failed_workflow_phases: execution_outcome.failed_workflow_phases,
            phase_execution_events: if phase_execution_events {
                execution_outcome.phase_execution_events
            } else {
                Vec::new()
            },
        }
    }
}
