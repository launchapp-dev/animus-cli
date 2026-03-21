use crate::cli_types::DaemonRunArgs;
use orchestrator_daemon_runtime::DaemonRuntimeOptions;

#[path = "daemon_scheduler_project_tick.rs"]
mod project_tick_ops;

pub(crate) use project_tick_ops::{slim_project_tick_driver, SlimProjectTickDriver};

pub(super) fn runtime_options_from_cli(args: &DaemonRunArgs, project_root: &str) -> DaemonRuntimeOptions {
    let project_path = std::path::Path::new(project_root);
    let mut options = DaemonRuntimeOptions::default();

    // Load persisted runtime settings as baseline before CLI overrides.
    options.reload_from_project_config(project_path);

    // CLI args always take precedence over persisted config.
    if let Some(v) = args.scheduler.pool_size {
        options.pool_size = Some(v);
    }
    options.interval_secs = args.scheduler.interval_secs;
    options.auto_run_ready = args.scheduler.auto_run_ready;
    options.startup_cleanup = args.scheduler.startup_cleanup;
    options.resume_interrupted = args.scheduler.resume_interrupted;
    options.reconcile_stale = args.scheduler.reconcile_stale;
    options.stale_threshold_hours = args.scheduler.stale_threshold_hours;
    options.max_tasks_per_tick = args.scheduler.max_tasks_per_tick;
    options.phase_timeout_secs = args.scheduler.phase_timeout_secs;
    options.idle_timeout_secs = args.scheduler.idle_timeout_secs;
    options.once = args.once;

    options
}
