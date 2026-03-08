use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::services::ServiceHub;

use crate::{
    execute_project_tick_script, DaemonRuntimeOptions, ProjectTickHooks, ProjectTickRunMode,
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
    let mut executor = crate::ProjectTickOperationExecutor::new(args, hooks, hub.clone(), root);
    let execution_outcome =
        execute_project_tick_script(&preparation.tick_script, &mut executor).await?;

    let health = serde_json::to_value(hub.daemon().health().await?)?;
    let summary_input = snapshot.into_summary_input(
        root.to_string(),
        health,
        execution_outcome,
        mode.include_phase_execution_events(),
    );
    TickSummaryBuilder::build(hub, args, summary_input).await
}
