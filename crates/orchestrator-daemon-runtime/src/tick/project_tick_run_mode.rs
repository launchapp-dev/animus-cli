use chrono::NaiveTime;

use crate::{
    DaemonRuntimeOptions, ProjectTickContext, ProjectTickPreparation, ProjectTickSnapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectTickRunMode {
    pub active_process_count: usize,
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
    ) -> ProjectTickPreparation {
        context.build_preparation(
            options,
            now,
            pool_draining,
            snapshot
                .daemon_health
                .as_ref()
                .and_then(|health| health.pool_size),
            self.active_process_count,
        )
    }

    pub fn include_phase_execution_events(self) -> bool {
        let _ = self;
        false
    }
}
