use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorEventKind {
    DaemonStatusChanged,
    Log,
    ProjectChanged,
    TaskChanged,
    WorkflowChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorEvent {
    pub kind: OrchestratorEventKind,
    pub payload: serde_json::Value,
}

impl OrchestratorEvent {
    pub fn new(kind: OrchestratorEventKind, payload: serde_json::Value) -> Self {
        Self { kind, payload }
    }
}
