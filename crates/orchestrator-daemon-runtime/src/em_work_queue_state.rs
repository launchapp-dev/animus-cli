use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmWorkQueueEntryStatus {
    Pending,
    Assigned,
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
