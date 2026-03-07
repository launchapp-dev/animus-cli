use std::sync::Arc;

use anyhow::Result;
use orchestrator_daemon_runtime::{DaemonRuntimeOptions, ProjectTickSummary};
use orchestrator_core::{services::ServiceHub, OrchestratorTask, RequirementItem, WorkflowStatus};

use super::{
    collect_requirement_lifecycle_transitions, collect_task_state_transitions,
    is_terminally_completed_workflow, ReadyTaskWorkflowStart,
};
use crate::services::runtime::stale_in_progress_summary;
use workflow_runner::executor::PhaseExecutionEvent;

pub(super) struct TickSummaryBuilder;

impl TickSummaryBuilder {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn build(
        hub: Arc<dyn ServiceHub>,
        args: &DaemonRuntimeOptions,
        project_root: String,
        started_daemon: bool,
        health: serde_json::Value,
        requirements_before: &[RequirementItem],
        tasks_before: &[OrchestratorTask],
        resumed_workflows: usize,
        cleaned_stale_workflows: usize,
        reconciled_stale_tasks: usize,
        reconciled_dependency_tasks: usize,
        reconciled_merge_tasks: usize,
        ready_started_count: usize,
        ready_started_workflows: &[ReadyTaskWorkflowStart],
        executed_workflow_phases: usize,
        failed_workflow_phases: usize,
        phase_execution_events: Vec<PhaseExecutionEvent>,
    ) -> Result<ProjectTickSummary> {
        let tasks = hub.tasks().list().await?;
        let workflows = hub.workflows().list().await.unwrap_or_default();

        let tasks_total = tasks.len();
        let tasks_ready = tasks
            .iter()
            .filter(|task| {
                matches!(
                    task.status,
                    orchestrator_core::TaskStatus::Ready | orchestrator_core::TaskStatus::Backlog
                )
            })
            .count();
        let tasks_in_progress = tasks
            .iter()
            .filter(|task| task.status == orchestrator_core::TaskStatus::InProgress)
            .count();
        let tasks_blocked = tasks.iter().filter(|task| task.status.is_blocked()).count();
        let tasks_done = tasks
            .iter()
            .filter(|task| task.status.is_terminal())
            .count();
        let stale_in_progress =
            stale_in_progress_summary(&tasks, args.stale_threshold_hours, chrono::Utc::now());

        let workflows_running = workflows
            .iter()
            .filter(|workflow| {
                matches!(
                    workflow.status,
                    WorkflowStatus::Running | WorkflowStatus::Paused
                )
            })
            .count();
        let workflows_completed = workflows
            .iter()
            .filter(|workflow| is_terminally_completed_workflow(workflow))
            .count();
        let workflows_failed = workflows
            .iter()
            .filter(|workflow| workflow.status == WorkflowStatus::Failed)
            .count();
        let requirements_after = hub.planning().list_requirements().await.unwrap_or_default();
        let requirement_lifecycle_transitions =
            collect_requirement_lifecycle_transitions(requirements_before, &requirements_after);
        let task_state_transitions = collect_task_state_transitions(
            tasks_before,
            &tasks,
            &workflows,
            &phase_execution_events,
            ready_started_workflows,
        );

        Ok(ProjectTickSummary {
            project_root,
            started_daemon,
            health,
            tasks_total,
            tasks_ready,
            tasks_in_progress,
            tasks_blocked,
            tasks_done,
            stale_in_progress_count: stale_in_progress.count,
            stale_in_progress_threshold_hours: stale_in_progress.threshold_hours,
            stale_in_progress_task_ids: stale_in_progress.task_ids(),
            workflows_running,
            workflows_completed,
            workflows_failed,
            resumed_workflows,
            cleaned_stale_workflows,
            reconciled_stale_tasks: reconciled_stale_tasks
                .saturating_add(reconciled_dependency_tasks)
                .saturating_add(reconciled_merge_tasks),
            started_ready_workflows: ready_started_count,
            executed_workflow_phases,
            failed_workflow_phases,
            phase_execution_events,
            requirement_lifecycle_transitions,
            task_state_transitions,
        })
    }
}
