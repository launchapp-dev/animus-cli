use std::path::Path;

use chrono::NaiveTime;

use crate::{DaemonRuntimeOptions, ProjectTickPreparation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTickContext {
    pub active_hours: Option<String>,
    pub initial_preparation: ProjectTickPreparation,
}

impl ProjectTickContext {
    pub fn load(
        project_root: &str,
        options: &DaemonRuntimeOptions,
        now: NaiveTime,
        pool_draining: bool,
    ) -> Self {
        let _ = orchestrator_core::ensure_workflow_config_compiled(Path::new(project_root));
        let active_hours = load_active_hours(project_root);
        let initial_preparation = ProjectTickPreparation::build(
            options,
            active_hours.as_deref(),
            now,
            pool_draining,
            None,
            None,
            0,
        );

        Self {
            active_hours,
            initial_preparation,
        }
    }

    pub fn build_preparation(
        &self,
        options: &DaemonRuntimeOptions,
        now: NaiveTime,
        pool_draining: bool,
        daemon_max_agents: Option<usize>,
        daemon_pool_size: Option<usize>,
        active_process_count: usize,
    ) -> ProjectTickPreparation {
        ProjectTickPreparation::build(
            options,
            self.active_hours.as_deref(),
            now,
            pool_draining,
            daemon_max_agents,
            daemon_pool_size,
            active_process_count,
        )
    }

    pub fn active_hours_skip_message(&self) -> Option<String> {
        self.active_hours.as_ref().map(|spec| {
            format!(
                "{}: outside active_hours ({}), skipping schedule dispatch",
                protocol::ACTOR_DAEMON,
                spec
            )
        })
    }
}

fn load_active_hours(project_root: &str) -> Option<String> {
    orchestrator_core::load_workflow_config_or_default(Path::new(project_root))
        .config
        .daemon
        .as_ref()
        .and_then(|daemon| daemon.active_hours.clone())
}
