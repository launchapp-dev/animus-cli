use protocol::SubjectDispatch;

use crate::DispatchSelectionSource;

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedDispatchStart {
    pub dispatch: SubjectDispatch,
    pub selection_source: DispatchSelectionSource,
}

impl PlannedDispatchStart {
    pub fn task_id(&self) -> Option<&str> {
        self.dispatch.task_id()
    }
}
