use std::path::PathBuf;

use anyhow::Result;
pub use orchestrator_store::{project_state_dir, read_json_or_default, write_json_atomic, write_json_pretty};
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
