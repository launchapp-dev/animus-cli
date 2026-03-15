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
