use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_core::services::ServiceHub;

use crate::{ProjectTickDriver, ProjectTickHooks, ProjectTickOperationExecutor};

pub struct HookBackedProjectTickDriver<H> {
    hooks: H,
}

impl<H> HookBackedProjectTickDriver<H> {
    pub fn new(hooks: H) -> Self {
        Self { hooks }
    }
}

impl<H> ProjectTickDriver for HookBackedProjectTickDriver<H>
where
    H: ProjectTickHooks,
{
    type Executor<'a>
        = ProjectTickOperationExecutor<'a, H>
    where
        Self: 'a;

    fn build_hub(&mut self, root: &str) -> Result<Arc<dyn ServiceHub>> {
        self.hooks.build_hub(root)
    }

    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>) {
        self.hooks.process_due_schedules(root, now);
    }

    fn active_process_count(&self) -> usize {
        self.hooks.active_process_count()
    }

    fn emit_notice(&mut self, message: &str) {
        self.hooks.emit_notice(message);
    }

    fn build_executor<'a>(
        &'a mut self,
        options: &'a crate::DaemonRuntimeOptions,
        hub: Arc<dyn ServiceHub>,
        root: &'a str,
    ) -> Self::Executor<'a> {
        ProjectTickOperationExecutor::new(options, &mut self.hooks, hub, root)
    }
}
