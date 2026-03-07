use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementLifecycleTransition {
    pub requirement_id: String,
    pub requirement_title: String,
    pub phase: String,
    pub status: String,
    pub transition_at: String,
    #[serde(default)]
    pub comment: Option<String>,
}
