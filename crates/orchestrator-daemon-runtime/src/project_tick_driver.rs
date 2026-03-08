use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_core::services::ServiceHub;

use crate::ProjectTickActionExecutor;

pub trait ProjectTickDriver {
    type Executor<'a>: ProjectTickActionExecutor
    where
        Self: 'a;

    fn build_hub(&mut self, root: &str) -> Result<Arc<dyn ServiceHub>>;

    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>);

    fn active_process_count(&self) -> usize {
        0
    }

    fn emit_notice(&mut self, _message: &str) {}

    fn build_executor<'a>(
        &'a mut self,
        options: &'a crate::DaemonRuntimeOptions,
        hub: Arc<dyn ServiceHub>,
        root: &'a str,
    ) -> Self::Executor<'a>;
}
