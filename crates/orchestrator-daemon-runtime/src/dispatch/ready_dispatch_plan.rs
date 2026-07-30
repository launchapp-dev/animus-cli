use animus_execution_protocol::ExecutionFence;
use protocol::SubjectDispatch;

use crate::DispatchSelectionSource;

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedDispatchStart {
    pub dispatch: SubjectDispatch,
    /// Kernel-selected durable workflow id. Queue-backed dispatches populate
    /// this before leasing so the queue row, workflow journal, runner, and
    /// environment broker all use one identity. `None` preserves the legacy
    /// ad-hoc fresh-dispatch path.
    pub workflow_id: Option<String>,
    /// Exact queue/subject/repository authority inherited by the runner.
    pub execution_fence: Option<ExecutionFence>,
    pub selection_source: DispatchSelectionSource,
}

impl PlannedDispatchStart {
    pub fn task_id(&self) -> Option<&str> {
        self.dispatch.task_id()
    }
}
