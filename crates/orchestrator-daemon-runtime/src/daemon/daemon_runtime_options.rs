use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRuntimeOptions {
    pub pool_size: Option<usize>,
    pub interval_secs: u64,
    pub auto_run_ready: bool,
    pub startup_cleanup: bool,
    pub resume_interrupted: bool,
    pub reconcile_stale: bool,
    pub stale_threshold_hours: u64,
    pub max_tasks_per_tick: usize,
    pub phase_timeout_secs: Option<u64>,
    pub idle_timeout_secs: Option<u64>,
    pub once: bool,
}

impl Default for DaemonRuntimeOptions {
    fn default() -> Self {
        Self {
            pool_size: None,
            interval_secs: 5,
            auto_run_ready: true,
            startup_cleanup: true,
            resume_interrupted: true,
            reconcile_stale: true,
            stale_threshold_hours: 24,
            max_tasks_per_tick: 2,
            phase_timeout_secs: None,
            idle_timeout_secs: None,
            once: false,
        }
    }
}

impl DaemonRuntimeOptions {
    /// Reload runtime-reconfigurable fields from the persisted daemon project config.
    ///
    /// Each `Some(..)` field in the project config overrides the corresponding value
    /// in `self`; `None` fields are left untouched so that CLI defaults are preserved
    /// for settings that have never been explicitly configured via `ao.daemon config-set`.
    ///
    /// This is called once per scheduler tick to enable hot-reload without restart.
    pub fn reload_from_project_config(&mut self, project_root: &std::path::Path) {
        let config = match orchestrator_core::load_daemon_project_config(project_root) {
            Ok(c) => c,
            Err(_) => return,
        };

        if let Some(v) = config.pool_size {
            self.pool_size = Some(v);
        }
        if let Some(v) = config.interval_secs {
            self.interval_secs = v;
        }
        if let Some(v) = config.max_tasks_per_tick {
            self.max_tasks_per_tick = v;
        }
        if let Some(v) = config.auto_run_ready {
            self.auto_run_ready = v;
        }
        if let Some(v) = config.stale_threshold_hours {
            self.stale_threshold_hours = v;
        }
        if let Some(v) = config.phase_timeout_secs {
            self.phase_timeout_secs = Some(v);
        }
        if let Some(v) = config.idle_timeout_secs {
            self.idle_timeout_secs = Some(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stable_test_home() -> std::path::PathBuf {
        let home_dir = std::env::temp_dir()
            .join(format!("ao-daemon-rt-test-config-{}", std::process::id()))
            .join("home");
        let _ = std::fs::create_dir_all(&home_dir);
        std::env::set_var("HOME", &home_dir);
        home_dir
    }

    #[test]
    fn reload_from_project_config_applies_pool_size() {
        stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let config = orchestrator_core::DaemonProjectConfig { pool_size: Some(8), ..Default::default() };
        orchestrator_core::write_daemon_project_config(temp.path(), &config).expect("write config");

        let mut options = DaemonRuntimeOptions::default();
        assert_eq!(options.pool_size, None);
        options.reload_from_project_config(temp.path());
        assert_eq!(options.pool_size, Some(8));
    }

    #[test]
    fn reload_from_project_config_applies_interval_secs() {
        stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let config = orchestrator_core::DaemonProjectConfig { interval_secs: Some(30), ..Default::default() };
        orchestrator_core::write_daemon_project_config(temp.path(), &config).expect("write config");

        let mut options = DaemonRuntimeOptions::default();
        assert_eq!(options.interval_secs, 5);
        options.reload_from_project_config(temp.path());
        assert_eq!(options.interval_secs, 30);
    }

    #[test]
    fn reload_from_project_config_applies_max_tasks_per_tick() {
        stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let config =
            orchestrator_core::DaemonProjectConfig { max_tasks_per_tick: Some(10), ..Default::default() };
        orchestrator_core::write_daemon_project_config(temp.path(), &config).expect("write config");

        let mut options = DaemonRuntimeOptions::default();
        assert_eq!(options.max_tasks_per_tick, 2);
        options.reload_from_project_config(temp.path());
        assert_eq!(options.max_tasks_per_tick, 10);
    }

    #[test]
    fn reload_from_project_config_applies_auto_run_ready() {
        stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let config =
            orchestrator_core::DaemonProjectConfig { auto_run_ready: Some(false), ..Default::default() };
        orchestrator_core::write_daemon_project_config(temp.path(), &config).expect("write config");

        let mut options = DaemonRuntimeOptions::default();
        assert!(options.auto_run_ready);
        options.reload_from_project_config(temp.path());
        assert!(!options.auto_run_ready);
    }

    #[test]
    fn reload_from_project_config_applies_stale_threshold_hours() {
        stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let config = orchestrator_core::DaemonProjectConfig {
            stale_threshold_hours: Some(48),
            ..Default::default()
        };
        orchestrator_core::write_daemon_project_config(temp.path(), &config).expect("write config");

        let mut options = DaemonRuntimeOptions::default();
        assert_eq!(options.stale_threshold_hours, 24);
        options.reload_from_project_config(temp.path());
        assert_eq!(options.stale_threshold_hours, 48);
    }

    #[test]
    fn reload_from_project_config_applies_phase_and_idle_timeouts() {
        stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let config = orchestrator_core::DaemonProjectConfig {
            phase_timeout_secs: Some(600),
            idle_timeout_secs: Some(1200),
            ..Default::default()
        };
        orchestrator_core::write_daemon_project_config(temp.path(), &config).expect("write config");

        let mut options = DaemonRuntimeOptions::default();
        assert_eq!(options.phase_timeout_secs, None);
        assert_eq!(options.idle_timeout_secs, None);
        options.reload_from_project_config(temp.path());
        assert_eq!(options.phase_timeout_secs, Some(600));
        assert_eq!(options.idle_timeout_secs, Some(1200));
    }

    #[test]
    fn reload_from_project_config_does_not_touch_none_fields() {
        stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir");
        // Write a config with only pool_size set — other fields should remain at defaults
        let config = orchestrator_core::DaemonProjectConfig { pool_size: Some(4), ..Default::default() };
        orchestrator_core::write_daemon_project_config(temp.path(), &config).expect("write config");

        let mut options = DaemonRuntimeOptions::default();
        let original_interval = options.interval_secs;
        let original_max = options.max_tasks_per_tick;
        let original_stale = options.stale_threshold_hours;
        options.reload_from_project_config(temp.path());
        assert_eq!(options.pool_size, Some(4));
        assert_eq!(options.interval_secs, original_interval);
        assert_eq!(options.max_tasks_per_tick, original_max);
        assert_eq!(options.stale_threshold_hours, original_stale);
    }

    #[test]
    fn reload_from_project_config_handles_missing_config() {
        stable_test_home();
        let temp = tempfile::tempdir().expect("tempdir");
        // No config file written — should be a no-op
        let mut options = DaemonRuntimeOptions::default();
        let snapshot = options.clone();
        options.reload_from_project_config(temp.path());
        assert_eq!(options, snapshot);
    }
}
