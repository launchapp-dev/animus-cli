use chrono::NaiveTime;

use crate::{
    DaemonRuntimeOptions, ProjectTickContext, ProjectTickPreparation, ProjectTickSnapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectTickRunMode {
    Full,
    Slim { active_process_count: usize },
}

impl ProjectTickRunMode {
    pub fn load_context(
        self,
        project_root: &str,
        options: &DaemonRuntimeOptions,
        now: NaiveTime,
        pool_draining: bool,
    ) -> ProjectTickContext {
        match self {
            Self::Full => {
                ProjectTickContext::load_for_project_tick(project_root, options, now, pool_draining)
            }
            Self::Slim { .. } => {
                ProjectTickContext::load_for_slim_tick(project_root, options, now, pool_draining)
            }
        }
    }

    pub fn build_preparation(
        self,
        context: &ProjectTickContext,
        options: &DaemonRuntimeOptions,
        now: NaiveTime,
        pool_draining: bool,
        snapshot: &ProjectTickSnapshot,
    ) -> ProjectTickPreparation {
        match self {
            Self::Full => context.build_project_tick_preparation(
                options,
                now,
                pool_draining,
                snapshot.daemon_health.as_ref(),
            ),
            Self::Slim {
                active_process_count,
            } => context.build_slim_tick_preparation(
                options,
                now,
                pool_draining,
                snapshot
                    .daemon_health
                    .as_ref()
                    .and_then(|health| health.max_agents),
                snapshot
                    .daemon_health
                    .as_ref()
                    .and_then(|health| health.pool_size.map(|value| value as usize)),
                active_process_count,
            ),
        }
    }

    pub fn include_phase_execution_events(self) -> bool {
        matches!(self, Self::Full)
    }
}
