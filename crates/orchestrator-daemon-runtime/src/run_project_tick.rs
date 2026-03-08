use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::services::ServiceHub;

use crate::{
    DaemonRuntimeOptions, ProjectTickExecutionOutcome, ProjectTickHooks, ProjectTickRunMode,
    ProjectTickSnapshot, ProjectTickSummary, ProjectTickTime, TickSummaryBuilder,
};

pub async fn run_project_tick<H>(
    root: &str,
    args: &DaemonRuntimeOptions,
    mode: ProjectTickRunMode,
    pool_draining: bool,
    hooks: &mut H,
) -> Result<ProjectTickSummary>
where
    H: ProjectTickHooks,
{
    run_project_tick_at(
        root,
        args,
        mode,
        pool_draining,
        hooks,
        ProjectTickTime::now(),
    )
    .await
}

pub async fn run_project_tick_at<H>(
    root: &str,
    args: &DaemonRuntimeOptions,
    mode: ProjectTickRunMode,
    pool_draining: bool,
    hooks: &mut H,
    tick_time: ProjectTickTime,
) -> Result<ProjectTickSummary>
where
    H: ProjectTickHooks,
{
    let now = tick_time.local_time();
    let context = mode.load_context(root, args, now, pool_draining);

    if !context
        .initial_preparation
        .schedule_plan
        .within_active_hours
    {
        if let Some(message) = context.active_hours_skip_message() {
            hooks.emit_notice(&message);
        }
    }

    if context
        .initial_preparation
        .schedule_plan
        .should_process_due_schedules
    {
        hooks.process_due_schedules(root, tick_time.schedule_at());
    }

    let hub: Arc<dyn ServiceHub> = hooks.build_hub(root)?;

    let snapshot = ProjectTickSnapshot::capture(hub.clone()).await?;
    let preparation = mode.build_preparation(&context, args, now, pool_draining, &snapshot);
    let mut execution_outcome = ProjectTickExecutionOutcome::default();
    let (executed_workflow_phases, failed_workflow_phases) =
        hooks.reconcile_completed_processes(hub.clone(), root).await?;
    execution_outcome.executed_workflow_phases = executed_workflow_phases;
    execution_outcome.failed_workflow_phases = failed_workflow_phases;

    if preparation.ready_dispatch_limit > 0 {
        execution_outcome.ready_workflow_starts = hooks
            .dispatch_ready_tasks(hub.clone(), root, preparation.ready_dispatch_limit)
            .await?;
    }

    let health = serde_json::to_value(hub.daemon().health().await?)?;
    let summary_input = snapshot.into_summary_input(
        root.to_string(),
        health,
        execution_outcome,
        mode.include_phase_execution_events(),
    );
    TickSummaryBuilder::build(hub, args, summary_input).await
}
