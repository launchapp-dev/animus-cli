use protocol::SubjectDispatch;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmWorkQueueEntryStatus {
    Pending,
    Assigned,
    Held,
    #[serde(other)]
    Unknown,
}

impl Default for EmWorkQueueEntryStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmWorkQueueEntry {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<SubjectDispatch>,
    #[serde(default)]
    pub status: EmWorkQueueEntryStatus,
    #[serde(default)]
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub assigned_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EmWorkQueueState {
    #[serde(default)]
    pub entries: Vec<EmWorkQueueEntry>,
}

impl EmWorkQueueEntry {
    pub fn from_dispatch(dispatch: SubjectDispatch) -> Self {
        Self {
            task_id: dispatch.task_id().unwrap_or_default().to_string(),
            dispatch: Some(dispatch),
            status: EmWorkQueueEntryStatus::Pending,
            workflow_id: None,
            assigned_at: None,
        }
    }

    pub fn subject_id(&self) -> &str {
        self.dispatch
            .as_ref()
            .map(|dispatch| dispatch.subject_id())
            .unwrap_or(self.task_id.as_str())
    }

    pub fn task_id(&self) -> Option<&str> {
        self.dispatch
            .as_ref()
            .and_then(SubjectDispatch::task_id)
            .or_else(|| (!self.task_id.trim().is_empty()).then_some(self.task_id.as_str()))
    }
}
