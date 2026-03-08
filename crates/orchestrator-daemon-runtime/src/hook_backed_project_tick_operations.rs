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
    async fn bootstrap_from_vision(
        &mut self,
        startup_cleanup: bool,
        ai_task_generation: bool,
    ) -> Result<()> {
        self.hooks
            .bootstrap_from_vision(
                self.hub.clone(),
                self.root,
                startup_cleanup,
                ai_task_generation,
            )
            .await
    }

    async fn resume_interrupted(&mut self) -> Result<(usize, usize)> {
        self.hooks
            .resume_interrupted(self.hub.clone(), self.root)
            .await
    }

    async fn recover_orphaned_running_workflows(&mut self) -> Result<()> {
        self.hooks
            .recover_orphaned_running_workflows(self.hub.clone(), self.root)
            .await
    }

    async fn reconcile_stale_tasks(&mut self, stale_threshold_hours: u64) -> Result<usize> {
        self.hooks
            .reconcile_stale_tasks(self.hub.clone(), self.root, stale_threshold_hours)
            .await
    }

    async fn reconcile_dependency_tasks(&mut self) -> Result<usize> {
        self.hooks
            .reconcile_dependency_tasks(self.hub.clone(), self.root)
            .await
    }

    async fn reconcile_merge_tasks(&mut self) -> Result<usize> {
        self.hooks
            .reconcile_merge_tasks(self.hub.clone(), self.root)
            .await
    }

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

    async fn refresh_runtime_binaries(&mut self) -> Result<()> {
        self.hooks
            .refresh_runtime_binaries(self.hub.clone(), self.root)
            .await
    }
}
