use std::sync::Arc;

use anyhow::Result;
use chrono::{Local, Utc};
use orchestrator_core::services::ServiceHub;

use crate::{
    execute_project_tick_script, DaemonRuntimeOptions, ProjectTickDriver,
    ProjectTickOperationExecutor, ProjectTickRunMode, ProjectTickSnapshot, ProjectTickSummary,
    TickSummaryBuilder,
};

pub async fn run_project_tick<D>(
    root: &str,
    args: &DaemonRuntimeOptions,
    mode: ProjectTickRunMode,
    pool_draining: bool,
    driver: &mut D,
) -> Result<ProjectTickSummary>
where
    D: ProjectTickDriver,
{
    let now = Local::now().time();
    let context = mode.load_context(root, args, now, pool_draining);

    if !context.initial_preparation.schedule_plan.within_active_hours {
        if let Some(message) = context.active_hours_skip_message() {
            driver.emit_notice(&message);
        }
    }

    if context
        .initial_preparation
        .schedule_plan
        .should_process_due_schedules
    {
        driver.process_due_schedules(root, Utc::now());
    }

    let hub: Arc<dyn ServiceHub> = driver.build_hub(root)?;
    driver.flush_git_outbox(root);

    let snapshot = ProjectTickSnapshot::capture(hub.clone()).await?;
    let preparation = mode.build_preparation(&context, args, now, pool_draining, &snapshot);
    let mut operations = driver.build_operations(hub.clone(), root);
    let mut executor = ProjectTickOperationExecutor::new(args, &mut operations);
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
