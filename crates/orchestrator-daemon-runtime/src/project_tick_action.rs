#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectTickAction {
    BootstrapFromVision,
    ResumeInterrupted,
    RecoverOrphanedRunningWorkflows,
    ReconcileStaleTasks,
    ReconcileDependencyTasks,
    ReconcileMergeTasks,
    ReconcileCompletedProcesses,
    RetryFailedTaskWorkflows,
    PromoteBacklogTasksToReady,
    DispatchReadyTasks { limit: usize },
    RefreshRuntimeBinaries,
}
