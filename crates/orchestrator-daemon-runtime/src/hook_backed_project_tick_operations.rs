use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::services::ServiceHub;

use crate::{ProjectTickHooks, ProjectTickOperations, ReadyTaskWorkflowStartSummary};

pub struct HookBackedProjectTickOperations<'a, H> {
    hooks: &'a mut H,
    hub: Arc<dyn ServiceHub>,
    root: &'a str,
}

impl<'a, H> HookBackedProjectTickOperations<'a, H> {
    pub fn new(hooks: &'a mut H, hub: Arc<dyn ServiceHub>, root: &'a str) -> Self {
        Self { hooks, hub, root }
    }
}

#[async_trait::async_trait(?Send)]
impl<H> ProjectTickOperations for HookBackedProjectTickOperations<'_, H>
where
    H: ProjectTickHooks,
{
    async fn reconcile_completed_processes(&mut self) -> Result<(usize, usize)> {
        self.hooks
            .reconcile_completed_processes(self.hub.clone(), self.root)
            .await
    }

    async fn dispatch_ready_tasks(
        &mut self,
        limit: usize,
    ) -> Result<ReadyTaskWorkflowStartSummary> {
        self.hooks
            .dispatch_ready_tasks(self.hub.clone(), self.root, limit)
            .await
    }
}
