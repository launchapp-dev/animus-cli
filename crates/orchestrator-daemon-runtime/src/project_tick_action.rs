#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectTickAction {
    ReconcileCompletedProcesses,
    DispatchReadyTasks { limit: usize },
}
