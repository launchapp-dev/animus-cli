use anyhow::Result;
use workflow_runner::executor::PhaseExecutionEvent;

use crate::{
    DaemonRuntimeOptions, ProjectTickAction, ProjectTickActionEffect, ProjectTickActionExecutor,
    ReadyTaskWorkflowStartSummary,
};

#[async_trait::async_trait(?Send)]
pub trait ProjectTickOperations {
    async fn bootstrap_from_vision(
        &mut self,
        _startup_cleanup: bool,
        _ai_task_generation: bool,
    ) -> Result<()> {
        Ok(())
    }

    async fn ensure_ai_generated_tasks(&mut self) -> Result<()> {
        Ok(())
    }

    async fn resume_interrupted(&mut self) -> Result<(usize, usize)> {
        Ok((0, 0))
    }

    async fn recover_orphaned_running_workflows(&mut self) -> Result<()> {
        Ok(())
    }

    async fn reconcile_stale_tasks(&mut self, _stale_threshold_hours: u64) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_dependency_tasks(&mut self) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_merge_tasks(&mut self) -> Result<usize> {
        Ok(0)
    }

    async fn reconcile_completed_processes(&mut self) -> Result<(usize, usize)> {
        Ok((0, 0))
    }

    async fn retry_failed_task_workflows(&mut self) -> Result<()> {
        Ok(())
    }

    async fn promote_backlog_tasks_to_ready(&mut self) -> Result<()> {
        Ok(())
    }

    async fn dispatch_ready_tasks(&mut self, _limit: usize) -> Result<ReadyTaskWorkflowStartSummary> {
        Ok(ReadyTaskWorkflowStartSummary::default())
    }

    async fn refresh_runtime_binaries(&mut self) -> Result<()> {
        Ok(())
    }

    async fn execute_running_workflow_phases(
        &mut self,
        _limit: usize,
    ) -> Result<(usize, usize, Vec<PhaseExecutionEvent>)> {
        Ok((0, 0, Vec::new()))
    }
}

pub struct ProjectTickOperationExecutor<'a, O> {
    options: &'a DaemonRuntimeOptions,
    operations: &'a mut O,
}

impl<'a, O> ProjectTickOperationExecutor<'a, O> {
    pub fn new(options: &'a DaemonRuntimeOptions, operations: &'a mut O) -> Self {
        Self {
            options,
            operations,
        }
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
            ProjectTickAction::BootstrapFromVision => {
                self.operations
                    .bootstrap_from_vision(
                        self.options.startup_cleanup,
                        self.options.ai_task_generation,
                    )
                    .await?;
                Ok(ProjectTickActionEffect::Noop)
            }
            ProjectTickAction::EnsureAiGeneratedTasks => {
                self.operations.ensure_ai_generated_tasks().await?;
                Ok(ProjectTickActionEffect::Noop)
            }
            ProjectTickAction::ResumeInterrupted => {
                let (cleaned_stale_workflows, resumed_workflows) =
                    self.operations.resume_interrupted().await?;
                Ok(ProjectTickActionEffect::ResumedInterrupted {
                    cleaned_stale_workflows,
                    resumed_workflows,
                })
            }
            ProjectTickAction::RecoverOrphanedRunningWorkflows => {
                self.operations.recover_orphaned_running_workflows().await?;
                Ok(ProjectTickActionEffect::Noop)
            }
            ProjectTickAction::ReconcileStaleTasks => {
                let count = self
                    .operations
                    .reconcile_stale_tasks(self.options.stale_threshold_hours)
                    .await?;
                Ok(ProjectTickActionEffect::ReconciledStaleTasks { count })
            }
            ProjectTickAction::ReconcileDependencyTasks => {
                let count = self.operations.reconcile_dependency_tasks().await?;
                Ok(ProjectTickActionEffect::ReconciledDependencyTasks { count })
            }
            ProjectTickAction::ReconcileMergeTasks => {
                let count = self.operations.reconcile_merge_tasks().await?;
                Ok(ProjectTickActionEffect::ReconciledMergeTasks { count })
            }
            ProjectTickAction::ReconcileCompletedProcesses => {
                let (executed_workflow_phases, failed_workflow_phases) =
                    self.operations.reconcile_completed_processes().await?;
                Ok(ProjectTickActionEffect::ReconciledCompletedProcesses {
                    executed_workflow_phases,
                    failed_workflow_phases,
                })
            }
            ProjectTickAction::RetryFailedTaskWorkflows => {
                self.operations.retry_failed_task_workflows().await?;
                Ok(ProjectTickActionEffect::Noop)
            }
            ProjectTickAction::PromoteBacklogTasksToReady => {
                self.operations.promote_backlog_tasks_to_ready().await?;
                Ok(ProjectTickActionEffect::Noop)
            }
            ProjectTickAction::DispatchReadyTasks { limit } => {
                let summary = self.operations.dispatch_ready_tasks(*limit).await?;
                Ok(ProjectTickActionEffect::ReadyWorkflowStarts { summary })
            }
            ProjectTickAction::RefreshRuntimeBinaries => {
                self.operations.refresh_runtime_binaries().await?;
                Ok(ProjectTickActionEffect::Noop)
            }
            ProjectTickAction::ExecuteRunningWorkflowPhases { limit } => {
                let (executed_workflow_phases, failed_workflow_phases, phase_execution_events) =
                    self.operations.execute_running_workflow_phases(*limit).await?;
                Ok(ProjectTickActionEffect::ExecutedRunningWorkflowPhases {
                    executed_workflow_phases,
                    failed_workflow_phases,
                    phase_execution_events,
                })
            }
        }
    }
}
