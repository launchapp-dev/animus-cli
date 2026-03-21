use anyhow::Result;

use crate::{
    DaemonRuntimeOptions, ProjectTickExecutionOutcome, ProjectTickHooks, ProjectTickRunMode, ProjectTickSummary,
    ProjectTickTime,
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
    run_project_tick_at(root, args, mode, pool_draining, hooks, ProjectTickTime::now()).await
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

    if context.initial_preparation.schedule_plan.should_process_due_schedules {
        hooks.process_due_schedules(root, tick_time.schedule_at());
    }

    let snapshot = hooks.capture_snapshot(root).await?;
    let preparation = mode.build_preparation(&context, args, now, pool_draining, &snapshot);
    let reconciled_workflows = hooks.reconcile_manual_timeouts(root).await?;
    let reconciled_runner_blocked_tasks = hooks.reconcile_runner_blocked_tasks(root).await?;
    let (executed_workflow_phases, failed_workflow_phases) = hooks.reconcile_completed_processes(root).await?;
    let reconciled_zombie_workflows = hooks.reconcile_zombie_workflows(root).await?;
    if args.reconcile_stale {
        hooks.reconcile_stale_in_progress_tasks(root).await?;
    }
    let mut execution_outcome = ProjectTickExecutionOutcome {
        reconciled_workflows: reconciled_workflows + reconciled_zombie_workflows,
        reconciled_runner_blocked_tasks,
        executed_workflow_phases,
        failed_workflow_phases,
        ..Default::default()
    };
    if preparation.ready_dispatch_limit > 0 {
        execution_outcome.ready_workflow_starts =
            hooks.dispatch_ready_tasks(root, preparation.ready_dispatch_limit).await?;
    }

    let health = hooks.collect_health(root).await?;
    let summary_input =
        snapshot.into_summary_input(root.to_string(), health, execution_outcome, mode.include_phase_execution_events());
    hooks.build_summary(args, summary_input).await
}
