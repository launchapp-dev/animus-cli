use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::{services::ServiceHub, OrchestratorTask, TaskStatus, WorkflowStatus};

use crate::{
    collect_requirement_lifecycle_transitions, collect_task_state_transitions,
    DaemonRuntimeOptions, ProjectTickSummary, ProjectTickSummaryInput,
};

pub struct TickSummaryBuilder;

impl TickSummaryBuilder {
    pub async fn build(
        hub: Arc<dyn ServiceHub>,
        args: &DaemonRuntimeOptions,
        input: ProjectTickSummaryInput,
    ) -> Result<ProjectTickSummary> {
        let tasks = hub.tasks().list().await?;
        let workflows = hub.workflows().list().await.unwrap_or_default();

        let tasks_total = tasks.len();
        let tasks_ready = tasks
            .iter()
            .filter(|task| matches!(task.status, TaskStatus::Ready | TaskStatus::Backlog))
            .count();
        let tasks_in_progress = tasks
            .iter()
            .filter(|task| task.status == TaskStatus::InProgress)
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
        let requirement_lifecycle_transitions = collect_requirement_lifecycle_transitions(
            &input.requirements_before,
            &requirements_after,
        );
        let task_state_transitions = collect_task_state_transitions(
            &input.tasks_before,
            &tasks,
            &workflows,
            &input.phase_execution_events,
            &input.ready_started_workflows,
        );

        Ok(ProjectTickSummary {
            project_root: input.project_root,
            started_daemon: input.started_daemon,
            health: input.health,
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
            resumed_workflows: input.resumed_workflows,
            cleaned_stale_workflows: input.cleaned_stale_workflows,
            reconciled_stale_tasks: input
                .reconciled_stale_tasks
                .saturating_add(input.reconciled_dependency_tasks)
                .saturating_add(input.reconciled_merge_tasks),
            started_ready_workflows: input.ready_started_count,
            executed_workflow_phases: input.executed_workflow_phases,
            failed_workflow_phases: input.failed_workflow_phases,
            phase_execution_events: input.phase_execution_events,
            requirement_lifecycle_transitions,
            task_state_transitions,
        })
    }
}

#[derive(Debug, Clone)]
struct StaleInProgressSummary {
    threshold_hours: u64,
    count: usize,
    tasks: Vec<StaleInProgressEntry>,
}

impl StaleInProgressSummary {
    fn task_ids(&self) -> Vec<String> {
        self.tasks
            .iter()
            .map(|entry| entry.task_id.clone())
            .collect()
    }
}

#[derive(Debug, Clone)]
struct StaleInProgressEntry {
    task_id: String,
}

fn stale_in_progress_summary(
    tasks: &[OrchestratorTask],
    threshold_hours: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> StaleInProgressSummary {
    let threshold_seconds = threshold_hours.saturating_mul(3600);
    let mut stale_tasks: Vec<&OrchestratorTask> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::InProgress)
        .filter(|task| task_age_seconds(now, task.metadata.updated_at) >= threshold_seconds)
        .collect();

    stale_tasks.sort_by(|a, b| {
        a.metadata
            .updated_at
            .cmp(&b.metadata.updated_at)
            .then(a.id.cmp(&b.id))
    });

    let stale_entries: Vec<StaleInProgressEntry> = stale_tasks
        .into_iter()
        .map(|task| StaleInProgressEntry {
            task_id: task.id.clone(),
        })
        .collect();

    StaleInProgressSummary {
        threshold_hours,
        count: stale_entries.len(),
        tasks: stale_entries,
    }
}

fn task_age_seconds(
    now: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> u64 {
    now.signed_duration_since(updated_at).num_seconds().max(0) as u64
}

fn is_terminally_completed_workflow(workflow: &orchestrator_core::OrchestratorWorkflow) -> bool {
    workflow.status == WorkflowStatus::Completed
        && workflow.machine_state == orchestrator_core::WorkflowMachineState::Completed
        && workflow.completed_at.is_some()
}
