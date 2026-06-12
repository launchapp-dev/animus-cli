use std::path::PathBuf;

pub use crate::store::{project_state_dir, read_json_or_default, write_json_atomic, write_json_pretty};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffRecord {
    pub handoff_id: String,
    pub run_id: String,
    pub target_role: String,
    pub question: String,
    pub context: Value,
    pub status: String,
    pub response: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HandoffStore {
    #[serde(default)]
    pub handoffs: Vec<HandoffRecord>,
}

pub fn handoffs_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("handoffs.json")
}

pub fn load_handoffs(project_root: &str) -> Result<HandoffStore> {
    read_json_or_default(&handoffs_path(project_root))
}

pub fn save_handoffs(project_root: &str, store: &HandoffStore) -> Result<()> {
    write_json_pretty(&handoffs_path(project_root), store)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryExecutionRecord {
    pub execution_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub workflow_id: Option<String>,
    /// Latest per-phase agent run id for this execution, when known.
    /// Pivots `history search` into `animus output read --run-id <id>`.
    /// Additive (serde-default): records written before this field
    /// existed load as `None` and the CLI resolves it at read time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryStore {
    #[serde(default)]
    pub entries: Vec<HistoryExecutionRecord>,
}

pub fn history_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("history.json")
}

pub fn load_history_store(project_root: &str) -> Result<HistoryStore> {
    read_json_or_default(&history_path(project_root))
}

pub fn save_history_store(project_root: &str, store: &HistoryStore) -> Result<()> {
    write_json_pretty(&history_path(project_root), store)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorRecord {
    pub id: String,
    pub category: String,
    pub severity: String,
    pub message: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub workflow_id: Option<String>,
    pub recoverable: bool,
    pub recovered: bool,
    pub created_at: String,
    #[serde(default)]
    pub source_event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ErrorStore {
    #[serde(default)]
    pub errors: Vec<ErrorRecord>,
}

pub fn errors_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("errors.json")
}

pub fn load_errors(project_root: &str) -> Result<ErrorStore> {
    read_json_or_default(&errors_path(project_root))
}

pub fn save_errors(project_root: &str, store: &ErrorStore) -> Result<()> {
    write_json_pretty(&errors_path(project_root), store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_record_round_trips_run_id() {
        let record = HistoryExecutionRecord {
            execution_id: "exec-1".to_string(),
            task_id: Some("TASK-1".to_string()),
            workflow_id: Some("wf-1".to_string()),
            run_id: Some("run-abc".to_string()),
            status: "completed".to_string(),
            started_at: Some("2026-06-01T00:00:00Z".to_string()),
            completed_at: Some("2026-06-01T01:00:00Z".to_string()),
            details: serde_json::json!({}),
        };
        let serialized = serde_json::to_string(&record).expect("serialize");
        assert!(serialized.contains("\"run_id\":\"run-abc\""));
        let parsed: HistoryExecutionRecord = serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(parsed.run_id.as_deref(), Some("run-abc"));
    }

    #[test]
    fn history_record_without_run_id_still_loads_and_omits_field() {
        // Old records on disk predate `run_id`; they must keep loading.
        let legacy = r#"{
            "execution_id": "exec-legacy",
            "task_id": "TASK-9",
            "workflow_id": "wf-9",
            "status": "failed",
            "started_at": "2025-01-01T00:00:00Z",
            "completed_at": null,
            "details": {}
        }"#;
        let parsed: HistoryExecutionRecord = serde_json::from_str(legacy).expect("legacy record must load");
        assert_eq!(parsed.run_id, None);
        // And `None` is omitted on the way back out so legacy readers
        // never see an unexpected null field.
        let serialized = serde_json::to_string(&parsed).expect("serialize");
        assert!(!serialized.contains("run_id"));
    }
}
