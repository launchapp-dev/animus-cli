use protocol::SubjectDispatch;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum DispatchQueueEntryStatus {
    #[default]
    Pending,
    Assigned,
    Held,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchQueueEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<SubjectDispatch>,
    #[serde(default)]
    pub status: DispatchQueueEntryStatus,
    #[serde(default)]
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub assigned_at: Option<String>,
    #[serde(default)]
    pub held_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DispatchQueueState {
    #[serde(default)]
    pub entries: Vec<DispatchQueueEntry>,
}

impl DispatchQueueEntry {
    pub fn from_dispatch(dispatch: SubjectDispatch) -> Self {
        Self {
            subject_id: Some(dispatch.subject_id().to_string()),
            task_id: dispatch.task_id().unwrap_or_default().to_string(),
            dispatch: Some(dispatch),
            status: DispatchQueueEntryStatus::Pending,
            workflow_id: None,
            assigned_at: None,
            held_at: None,
        }
    }

    pub fn subject_id(&self) -> &str {
        if let Some(subject_id) = self.subject_id.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
            return subject_id;
        }
        if let Some(dispatch) = &self.dispatch {
            return dispatch.subject_id();
        }
        self.task_id.as_str()
    }

    pub fn task_id(&self) -> Option<&str> {
        self.dispatch
            .as_ref()
            .and_then(SubjectDispatch::task_id)
            .or_else(|| (!self.task_id.trim().is_empty()).then_some(self.task_id.as_str()))
    }
}
