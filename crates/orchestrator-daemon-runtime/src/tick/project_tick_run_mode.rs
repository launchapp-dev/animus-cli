use chrono::NaiveTime;

use crate::{DaemonRuntimeOptions, ProjectTickContext, ProjectTickPreparation, ProjectTickSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectTickRunMode {
    pub active_process_count: usize,
    /// When `true` the tick runs the full reconciliation sweep
    /// (manual-timeout, zombie-workflow, and stale-in-progress legs) in
    /// addition to the dispatch legs. The daemon loop sets this on
    /// heartbeat-cadence passes only; event wakes (nudge, cron deadline,
    /// completion) run the cheaper dispatch-focused tick so a nudge storm
    /// cannot multiply the heavy state scans. Completed-process reaping
    /// always runs — it frees pool headroom the dispatch legs depend on.
    pub housekeeping: bool,
}

impl Default for ProjectTickRunMode {
    fn default() -> Self {
        Self { active_process_count: 0, housekeeping: true }
    }
}

impl ProjectTickRunMode {
    pub fn load_context(
        self,
        project_root: &str,
        options: &DaemonRuntimeOptions,
        now: NaiveTime,
        pool_draining: bool,
    ) -> ProjectTickContext {
        ProjectTickContext::load(project_root, options, now, pool_draining)
    }

    pub fn build_preparation(
        self,
        context: &ProjectTickContext,
        options: &DaemonRuntimeOptions,
        now: NaiveTime,
        pool_draining: bool,
        snapshot: &ProjectTickSnapshot,
        active_process_count: usize,
    ) -> ProjectTickPreparation {
        context.build_preparation(
            options,
            now,
            pool_draining,
            snapshot.daemon_health.as_ref().and_then(|health| health.pool_size),
            active_process_count,
        )
    }

    pub fn include_phase_execution_events(self) -> bool {
        let _ = self;
        false
    }
}
