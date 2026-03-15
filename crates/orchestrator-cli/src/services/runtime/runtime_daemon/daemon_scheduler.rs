use crate::cli_types::DaemonRunArgs;
use orchestrator_daemon_runtime::DaemonRuntimeOptions;

#[path = "daemon_scheduler_project_tick.rs"]
mod project_tick_ops;

pub(crate) use project_tick_ops::{slim_project_tick_driver, SlimProjectTickDriver};

pub(super) fn runtime_options_from_cli(args: &DaemonRunArgs) -> DaemonRuntimeOptions {
    DaemonRuntimeOptions {
        pool_size: args.scheduler.pool_size,
        interval_secs: args.scheduler.interval_secs,
        auto_run_ready: args.scheduler.auto_run_ready,
        startup_cleanup: args.scheduler.startup_cleanup,
        resume_interrupted: args.scheduler.resume_interrupted,
        reconcile_stale: args.scheduler.reconcile_stale,
        stale_threshold_hours: args.scheduler.stale_threshold_hours,
        max_tasks_per_tick: args.scheduler.max_tasks_per_tick,
        phase_timeout_secs: args.scheduler.phase_timeout_secs,
        idle_timeout_secs: args.scheduler.idle_timeout_secs,
        once: args.once,
    }
}
