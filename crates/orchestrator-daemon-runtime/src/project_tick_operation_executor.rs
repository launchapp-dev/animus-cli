use std::sync::Arc;

use anyhow::Result;
use orchestrator_core::services::ServiceHub;

use crate::{ProjectTickAction, ProjectTickActionEffect, ProjectTickActionExecutor, ProjectTickHooks};

pub struct ProjectTickOperationExecutor<'a, H> {
    hooks: &'a mut H,
    hub: Arc<dyn ServiceHub>,
    root: &'a str,
}

impl<'a, H> ProjectTickOperationExecutor<'a, H> {
    pub fn new(
        _options: &'a crate::DaemonRuntimeOptions,
        hooks: &'a mut H,
        hub: Arc<dyn ServiceHub>,
        root: &'a str,
    ) -> Self {
        Self { hooks, hub, root }
    }
}

#[async_trait::async_trait(?Send)]
impl<H> ProjectTickActionExecutor for ProjectTickOperationExecutor<'_, H>
where
    H: ProjectTickHooks,
{
    async fn execute_action(
        &mut self,
        action: &ProjectTickAction,
    ) -> Result<ProjectTickActionEffect> {
        match action {
            ProjectTickAction::ReconcileCompletedProcesses => {
                let (executed_workflow_phases, failed_workflow_phases) =
                    self.hooks
                        .reconcile_completed_processes(self.hub.clone(), self.root)
                        .await?;
                Ok(ProjectTickActionEffect::ReconciledCompletedProcesses {
                    executed_workflow_phases,
                    failed_workflow_phases,
                })
            }
            ProjectTickAction::DispatchReadyTasks { limit } => {
                let summary = self
                    .hooks
                    .dispatch_ready_tasks(self.hub.clone(), self.root, *limit)
                    .await?;
                Ok(ProjectTickActionEffect::ReadyWorkflowStarts { summary })
            }
        }
    }
}
