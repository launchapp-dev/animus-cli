use crate::cli_types::DaemonRunArgs;
use orchestrator_core::load_workflow_config_or_default;
use orchestrator_daemon_runtime::DaemonRuntimeOptions;

#[path = "daemon_scheduler_project_tick.rs"]
mod project_tick_ops;

pub(crate) use project_tick_ops::{slim_project_tick_driver, SlimProjectTickDriver};

pub(super) fn runtime_options_from_cli(args: &DaemonRunArgs, project_root: &str) -> DaemonRuntimeOptions {
    let project_path = std::path::Path::new(project_root);
    let mut options = DaemonRuntimeOptions::default();
    let persisted_config = orchestrator_core::load_daemon_project_config(project_path).ok();
    let persisted_auto_run_ready = persisted_config.as_ref().and_then(|config| config.auto_run_ready);
    // Load persisted runtime settings as baseline before CLI overrides.
    // Uses the already-loaded config to avoid a redundant file read.
    if let Some(ref config) = persisted_config {
        options.reload_from_config(config);
    }

    // CLI args always take precedence over persisted config.
    if let Some(v) = args.scheduler.pool_size {
        options.pool_size = Some(v);
    }
    if let Some(v) = args.scheduler.interval_secs {
        options.interval_secs = v;
    }
    options.startup_cleanup = args.scheduler.startup_cleanup;
    options.resume_interrupted = args.scheduler.resume_interrupted;
    options.reconcile_stale = args.scheduler.reconcile_stale;
    if let Some(v) = args.scheduler.stale_threshold_hours {
        options.stale_threshold_hours = v;
    }
    if let Some(v) = args.scheduler.max_tasks_per_tick {
        options.max_tasks_per_tick = v;
    }
    options.phase_timeout_secs = args.scheduler.phase_timeout_secs;
    options.idle_timeout_secs = args.scheduler.idle_timeout_secs;
    options.once = args.once;

    if persisted_auto_run_ready.is_none() {
        if let Some(v) =
            load_workflow_config_or_default(project_path).config.daemon.as_ref().map(|daemon| daemon.auto_run_ready)
        {
            options.auto_run_ready = v;
        }
    }

    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_env_lock;
    use protocol::test_utils::EnvVarGuard;

    #[test]
    fn runtime_options_use_workflow_daemon_auto_run_ready_when_pm_config_missing() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());

        let home_root = tempfile::TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let project_root = tempfile::TempDir::new().expect("project temp dir");
        let mut workflow = orchestrator_core::builtin_workflow_config();
        workflow.daemon = Some(orchestrator_core::workflow_config::DaemonConfig {
            interval_secs: None,
            pool_size: None,
            active_hours: None,
            auto_run_ready: false,
            max_task_retries: None,
            retry_cooldown_secs: None,
            auto_merge: None,
            auto_pr: None,
            auto_commit_before_merge: None,
            auto_prune_worktrees: None,
            phase_routing: None,
            mcp: None,
        });
        orchestrator_core::write_workflow_config(project_root.path(), &workflow).expect("write workflow config");

        let args = DaemonRunArgs {
            scheduler: crate::DaemonSchedulerArgs {
                pool_size: None,
                interval_secs: Some(5),
                startup_cleanup: true,
                resume_interrupted: true,
                reconcile_stale: true,
                stale_threshold_hours: Some(24),
                max_tasks_per_tick: Some(2),
                phase_timeout_secs: None,
                idle_timeout_secs: None,
            },
            skip_runner: true,
            runner_scope: None,
            once: false,
        };

        let options = runtime_options_from_cli(&args, project_root.path().to_string_lossy().as_ref());
        assert!(!options.auto_run_ready);
    }

    #[test]
    fn runtime_options_keep_persisted_auto_run_ready_over_workflow_yaml() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());

        let home_root = tempfile::TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home_root.path().to_string_lossy().as_ref()));
        let _legacy_guard = EnvVarGuard::set("AGENT_ORCHESTRATOR_CONFIG_DIR", None);

        let project_root = tempfile::TempDir::new().expect("project temp dir");
        let mut workflow = orchestrator_core::builtin_workflow_config();
        workflow.daemon = Some(orchestrator_core::workflow_config::DaemonConfig {
            interval_secs: None,
            pool_size: None,
            active_hours: None,
            auto_run_ready: true,
            max_task_retries: None,
            retry_cooldown_secs: None,
            auto_merge: None,
            auto_pr: None,
            auto_commit_before_merge: None,
            auto_prune_worktrees: None,
            phase_routing: None,
            mcp: None,
        });
        orchestrator_core::write_workflow_config(project_root.path(), &workflow).expect("write workflow config");

        let persisted = orchestrator_core::DaemonProjectConfig { auto_run_ready: Some(false), ..Default::default() };
        orchestrator_core::write_daemon_project_config(project_root.path(), &persisted).expect("write daemon config");

        let args = DaemonRunArgs {
            scheduler: crate::DaemonSchedulerArgs {
                pool_size: None,
                interval_secs: None,
                startup_cleanup: true,
                resume_interrupted: true,
                reconcile_stale: true,
                stale_threshold_hours: None,
                max_tasks_per_tick: None,
                phase_timeout_secs: None,
                idle_timeout_secs: None,
            },
            skip_runner: true,
            runner_scope: None,
            once: false,
        };

        let options = runtime_options_from_cli(&args, project_root.path().to_string_lossy().as_ref());
        assert!(!options.auto_run_ready);
    }
}
