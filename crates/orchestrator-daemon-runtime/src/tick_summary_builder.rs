use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::{services::ServiceHub, DaemonTickMetrics};

use crate::{DaemonRuntimeOptions, ProjectTickSummary, ProjectTickSummaryInput};

pub struct TickSummaryBuilder;

impl TickSummaryBuilder {
    pub async fn build(
        hub: Arc<dyn ServiceHub>,
        args: &DaemonRuntimeOptions,
        input: ProjectTickSummaryInput,
    ) -> Result<ProjectTickSummary> {
        let metrics = DaemonTickMetrics::collect(hub, args.stale_threshold_hours).await?;

        Ok(ProjectTickSummary {
            project_root: input.project_root,
            started_daemon: input.started_daemon,
            health: input.health,
            tasks_total: metrics.tasks_total,
            tasks_ready: metrics.tasks_ready,
            tasks_in_progress: metrics.tasks_in_progress,
            tasks_blocked: metrics.tasks_blocked,
            tasks_done: metrics.tasks_done,
            stale_in_progress_count: metrics.stale_in_progress_count,
            stale_in_progress_threshold_hours: args.stale_threshold_hours,
            stale_in_progress_task_ids: metrics.stale_in_progress_task_ids,
            workflows_running: metrics.workflows_running,
            workflows_completed: metrics.workflows_completed,
            workflows_failed: metrics.workflows_failed,
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
        })
    }
}
