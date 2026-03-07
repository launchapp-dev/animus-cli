use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateTransition {
    pub task_id: String,
    pub from_status: String,
    pub to_status: String,
    pub changed_at: String,
    #[serde(default)]
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub phase_id: Option<String>,
    #[serde(default)]
    pub selection_source: Option<String>,
}
