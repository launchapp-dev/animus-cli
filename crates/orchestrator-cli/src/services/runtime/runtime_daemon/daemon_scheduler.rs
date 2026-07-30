use crate::cli_types::DaemonRunArgs;
use orchestrator_daemon_runtime::DaemonRuntimeOptions;

#[path = "daemon_scheduler_project_tick.rs"]
mod project_tick_ops;

pub(crate) use project_tick_ops::{
    complete_execution, recover_execution_lease, slim_project_tick_driver, SlimProjectTickDriver,
};

pub(super) fn runtime_options_from_cli(args: &DaemonRunArgs, project_root: &str) -> DaemonRuntimeOptions {
    let project_path = std::path::Path::new(project_root);
    let mut options = DaemonRuntimeOptions::default();

    // Load persisted runtime settings as baseline before CLI overrides.
    options.reload_from_project_config(project_path);

    // CLI args always take precedence over persisted config.
    if let Some(v) = args.scheduler.pool_size {
        options.pool_size = Some(v);
    }
    if let Some(v) = args.scheduler.interval_secs {
        options.interval_secs = v;
    }
    options.startup_cleanup = args.scheduler.startup_cleanup;
    options.reconcile_stale = args.scheduler.reconcile_stale;
    if let Some(v) = args.scheduler.stale_threshold_hours {
        options.stale_threshold_hours = v;
    }
    if let Some(v) = args.scheduler.max_tasks_per_tick {
        options.max_tasks_per_tick = v;
    }
    options.phase_timeout_secs = args.scheduler.phase_timeout_secs;
    options.once = args.once;
    options.auto_install_plugins = args.auto_install;
    options.skip_plugin_preflight = args.skip_preflight;

    options
}
