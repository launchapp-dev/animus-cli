use anyhow::Result;

use crate::{
    schedule_headroom, DaemonRuntimeOptions, ProjectTickExecutionOutcome, ProjectTickHooks, ProjectTickRunMode,
    ProjectTickSummary, ProjectTickTime, TickBudget,
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

    // Compute one shared dispatch budget BEFORE processing schedules so that
    // schedules + triggers split a single pool of headroom instead of each
    // spending the full amount independently.  Uses the pre-tick active count
    // (captured by the caller before the tick loop iteration).  The schedule
    // hook runs first and claims slots via `TickBudget::try_take`; the
    // trigger hook sees whatever remains.  Without this shared budget, the
    // ProcessManager rejects the over-budget spawns while schedules still get
    // marked attempted and webhook events still get drained from the queue.
    let mut tick_budget = TickBudget::new(schedule_headroom(args.pool_size, mode.active_process_count));
    if context.initial_preparation.schedule_plan.should_process_due_schedules {
        hooks.process_due_schedules(root, tick_time.schedule_at(), &mut tick_budget);
        hooks.process_due_triggers(root, tick_time.schedule_at(), &mut tick_budget);
    }

    let snapshot = hooks.capture_snapshot(root).await?;

    // Re-count active processes AFTER schedule dispatches so that ready-task
    // headroom accounts for any processes spawned by the schedule path.
    let updated_active_count = hooks.active_process_count();
    let preparation = mode.build_preparation(&context, args, now, pool_draining, &snapshot, updated_active_count);
    // Completed-process reaping always runs: it frees pool headroom that the
    // dispatch legs below depend on, so an event wake triggered by a workflow
    // completion can immediately dispatch follow-on work. The heavier
    // reconciliation legs (manual timeouts, zombie workflows, stale
    // in-progress tasks) are housekeeping: the daemon loop runs them on
    // heartbeat cadence only (`mode.housekeeping`), never per-nudge.
    let reconciled_workflows = if mode.housekeeping { hooks.reconcile_manual_timeouts(root).await? } else { 0 };
    let completed_reconciliation = hooks.reconcile_completed_processes(root).await?;
    let reconciled_zombie_workflows = if mode.housekeeping { hooks.reconcile_zombie_workflows(root).await? } else { 0 };
    if mode.housekeeping && args.reconcile_stale {
        hooks.reconcile_stale_in_progress_tasks(root, args.stale_threshold_hours).await?;
    }
    let mut execution_outcome = ProjectTickExecutionOutcome {
        reconciled_workflows: reconciled_workflows + reconciled_zombie_workflows,
        executed_workflow_phases: completed_reconciliation.executed_workflow_phases,
        failed_workflow_phases: completed_reconciliation.failed_workflow_phases,
        workflow_failures: completed_reconciliation.workflow_failures,
        ..Default::default()
    };
    // Recompute the dispatch limits after all reconciliation hooks have run.
    // Completed-process and zombie-workflow reconciliation may free pool capacity
    // that was not yet reflected in the pre-reconciliation active count used by
    // `preparation`.  Requerying here ensures we can dispatch into that headroom.
    let post_reconcile_active_count = hooks.active_process_count();
    let (ready_dispatch_limit, queue_drain_limit) = if post_reconcile_active_count != updated_active_count {
        let recomputed =
            mode.build_preparation(&context, args, now, pool_draining, &snapshot, post_reconcile_active_count);
        (recomputed.ready_dispatch_limit, recomputed.queue_drain_limit)
    } else {
        (preparation.ready_dispatch_limit, preparation.queue_drain_limit)
    };
    if ready_dispatch_limit > 0 || queue_drain_limit > 0 {
        execution_outcome.ready_workflow_starts =
            hooks.dispatch_ready_tasks(root, ready_dispatch_limit, queue_drain_limit).await?;
    }

    let health = hooks.collect_health(root).await?;
    let summary_input =
        snapshot.into_summary_input(root.to_string(), health, execution_outcome, mode.include_phase_execution_events());
    hooks.build_summary(args, summary_input).await
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use chrono::{DateTime, Utc};
    use serde_json::Value;

    use crate::{
        run_project_tick, CompletedProcessReconciliation, DaemonRuntimeOptions, DispatchWorkflowStartSummary,
        ProjectTickHooks, ProjectTickRunMode, ProjectTickSnapshot, ProjectTickSummary, ProjectTickSummaryInput,
        TickBudget,
    };

    #[derive(Default)]
    struct RecordingHooks {
        dispatch_calls: Vec<(usize, usize)>,
        schedule_calls: usize,
        completed_process_calls: usize,
        manual_timeout_calls: usize,
        zombie_calls: usize,
        stale_calls: usize,
    }

    #[async_trait::async_trait(?Send)]
    impl ProjectTickHooks for RecordingHooks {
        fn process_due_schedules(&mut self, _root: &str, _now: DateTime<Utc>, _budget: &mut TickBudget) {
            self.schedule_calls += 1;
        }

        async fn reconcile_zombie_workflows(&mut self, _root: &str) -> Result<usize> {
            self.zombie_calls += 1;
            Ok(0)
        }

        async fn reconcile_manual_timeouts(&mut self, _root: &str) -> Result<usize> {
            self.manual_timeout_calls += 1;
            Ok(0)
        }

        async fn reconcile_stale_in_progress_tasks(
            &mut self,
            _root: &str,
            _stale_threshold_hours: u64,
        ) -> Result<usize> {
            self.stale_calls += 1;
            Ok(0)
        }

        async fn capture_snapshot(&mut self, _root: &str) -> Result<ProjectTickSnapshot> {
            Ok(ProjectTickSnapshot {
                requirements_before: Vec::new(),
                tasks_before: Vec::new(),
                started_daemon: false,
                daemon_health: None,
            })
        }

        async fn reconcile_completed_processes(&mut self, _root: &str) -> Result<CompletedProcessReconciliation> {
            self.completed_process_calls += 1;
            Ok(CompletedProcessReconciliation::default())
        }

        async fn dispatch_ready_tasks(
            &mut self,
            _root: &str,
            limit: usize,
            queue_drain_limit: usize,
        ) -> Result<DispatchWorkflowStartSummary> {
            self.dispatch_calls.push((limit, queue_drain_limit));
            Ok(DispatchWorkflowStartSummary::default())
        }

        async fn collect_health(&mut self, _root: &str) -> Result<Value> {
            Ok(Value::Null)
        }

        async fn build_summary(
            &mut self,
            _args: &DaemonRuntimeOptions,
            input: ProjectTickSummaryInput,
        ) -> Result<ProjectTickSummary> {
            Ok(ProjectTickSummary {
                project_root: input.project_root,
                started_daemon: input.started_daemon,
                health: input.health,
                tasks_total: 0,
                tasks_ready: 0,
                tasks_in_progress: 0,
                tasks_blocked: 0,
                tasks_done: 0,
                stale_in_progress_count: 0,
                stale_in_progress_threshold_hours: 0,
                stale_in_progress_task_ids: Vec::new(),
                workflows_running: 0,
                workflows_completed: 0,
                workflows_failed: 0,
                resumed_workflows: 0,
                cleaned_stale_workflows: 0,
                reconciled_workflows: 0,
                started_ready_workflows: 0,
                executed_workflow_phases: 0,
                failed_workflow_phases: 0,
                task_state_changes: Vec::new(),
                phase_execution_events: Vec::new(),
                workflow_failures: Vec::new(),
            })
        }
    }

    fn run_recorded_tick(options: &DaemonRuntimeOptions, pool_draining: bool, housekeeping: bool) -> RecordingHooks {
        let _env_lock = crate::dispatch::test_env::lock().lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));
        let project = tempfile::tempdir().expect("project tempdir");

        let mut hooks = RecordingHooks::default();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("test runtime");
        runtime
            .block_on(run_project_tick(
                project.path().to_string_lossy().as_ref(),
                options,
                ProjectTickRunMode { active_process_count: 0, housekeeping },
                pool_draining,
                &mut hooks,
            ))
            .expect("tick should succeed");
        hooks
    }

    fn tick_dispatch_calls(options: &DaemonRuntimeOptions, pool_draining: bool) -> Vec<(usize, usize)> {
        run_recorded_tick(options, pool_draining, true).dispatch_calls
    }

    #[test]
    fn tick_drains_dispatch_queue_when_auto_run_ready_is_off() {
        let options = DaemonRuntimeOptions { auto_run_ready: false, ..DaemonRuntimeOptions::default() };
        let calls = tick_dispatch_calls(&options, false);
        assert_eq!(
            calls,
            vec![(0, options.max_tasks_per_tick)],
            "queue drain must run with zero ready-task limit when auto_run_ready is off"
        );
    }

    #[test]
    fn tick_skips_all_dispatch_while_pool_is_draining() {
        let options = DaemonRuntimeOptions { auto_run_ready: false, ..DaemonRuntimeOptions::default() };
        let calls = tick_dispatch_calls(&options, true);
        assert!(calls.is_empty(), "draining pool must not dispatch queue entries or ready tasks");
    }

    #[test]
    fn tick_dispatches_both_limits_when_auto_run_ready_is_on() {
        let options = DaemonRuntimeOptions::default();
        let calls = tick_dispatch_calls(&options, false);
        assert_eq!(calls, vec![(options.max_tasks_per_tick, options.max_tasks_per_tick)]);
    }

    #[test]
    fn housekeeping_tick_runs_full_reconciliation_sweep() {
        let options = DaemonRuntimeOptions { reconcile_stale: true, ..DaemonRuntimeOptions::default() };
        let hooks = run_recorded_tick(&options, false, true);
        assert_eq!(hooks.manual_timeout_calls, 1);
        assert_eq!(hooks.zombie_calls, 1);
        assert_eq!(hooks.stale_calls, 1);
        assert_eq!(hooks.completed_process_calls, 1);
        assert_eq!(hooks.schedule_calls, 1);
        assert_eq!(hooks.dispatch_calls.len(), 1);
    }

    #[test]
    fn event_wake_tick_skips_housekeeping_but_keeps_dispatch_legs() {
        let options = DaemonRuntimeOptions { reconcile_stale: true, ..DaemonRuntimeOptions::default() };
        let hooks = run_recorded_tick(&options, false, false);
        assert_eq!(hooks.manual_timeout_calls, 0, "event wakes must not run manual-timeout reconciliation");
        assert_eq!(hooks.zombie_calls, 0, "event wakes must not run zombie-workflow reconciliation");
        assert_eq!(hooks.stale_calls, 0, "event wakes must not run stale-in-progress reconciliation");
        assert_eq!(hooks.completed_process_calls, 1, "completed-process reaping frees headroom; always runs");
        assert_eq!(hooks.schedule_calls, 1, "schedules are a dispatch leg; they run on event wakes");
        assert_eq!(hooks.dispatch_calls.len(), 1, "dispatch must run on event wakes");
    }
}
