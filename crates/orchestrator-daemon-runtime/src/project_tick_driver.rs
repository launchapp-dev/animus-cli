use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use orchestrator_core::services::ServiceHub;

use crate::ProjectTickOperations;

pub trait ProjectTickDriver {
    type Operations<'a>: ProjectTickOperations
    where
        Self: 'a;

    fn build_hub(&mut self, root: &str) -> Result<Arc<dyn ServiceHub>>;

    fn process_due_schedules(&mut self, root: &str, now: DateTime<Utc>);

    fn flush_git_outbox(&mut self, root: &str);

    fn emit_notice(&mut self, _message: &str) {}

    fn build_operations<'a>(
        &'a mut self,
        hub: Arc<dyn ServiceHub>,
        root: &'a str,
    ) -> Self::Operations<'a>;
}
