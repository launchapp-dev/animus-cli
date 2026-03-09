use serde::{Deserialize, Serialize};

use crate::{DispatchSelectionSource, SubjectDispatch};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchWorkflowStart {
    pub dispatch: SubjectDispatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    pub selection_source: DispatchSelectionSource,
}

impl DispatchWorkflowStart {
    pub fn task_id(&self) -> Option<&str> {
        self.dispatch.task_id()
    }

    pub fn subject_id(&self) -> &str {
        self.dispatch.subject_id()
    }
}
