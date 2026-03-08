use crate::{
    ProjectTickAction, ProjectTickActionEffect, ProjectTickActionExecutor,
    ReadyTaskWorkflowStartSummary,
};
use anyhow::Result;

#[async_trait::async_trait(?Send)]
pub trait ProjectTickOperations {
    async fn reconcile_completed_processes(&mut self) -> Result<(usize, usize)> {
        Ok((0, 0))
    }

    async fn dispatch_ready_tasks(
        &mut self,
        _limit: usize,
    ) -> Result<ReadyTaskWorkflowStartSummary> {
        Ok(ReadyTaskWorkflowStartSummary::default())
    }
}

pub struct ProjectTickOperationExecutor<'a, O> {
    operations: &'a mut O,
}

impl<'a, O> ProjectTickOperationExecutor<'a, O> {
    pub fn new(_options: &'a crate::DaemonRuntimeOptions, operations: &'a mut O) -> Self {
        Self { operations }
    }
}

#[async_trait::async_trait(?Send)]
impl<O> ProjectTickActionExecutor for ProjectTickOperationExecutor<'_, O>
where
    O: ProjectTickOperations,
{
    async fn execute_action(
        &mut self,
        action: &ProjectTickAction,
    ) -> Result<ProjectTickActionEffect> {
        match action {
            ProjectTickAction::ReconcileCompletedProcesses => {
                let (executed_workflow_phases, failed_workflow_phases) =
                    self.operations.reconcile_completed_processes().await?;
                Ok(ProjectTickActionEffect::ReconciledCompletedProcesses {
                    executed_workflow_phases,
                    failed_workflow_phases,
                })
            }
            ProjectTickAction::DispatchReadyTasks { limit } => {
                let summary = self.operations.dispatch_ready_tasks(*limit).await?;
                Ok(ProjectTickActionEffect::ReadyWorkflowStarts { summary })
            }
        }
    }
}
