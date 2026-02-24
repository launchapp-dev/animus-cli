use crate::common::{RunId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutput {
    pub id: String,
    pub execution_id: String,
    pub run_id: RunId,
    pub timestamp: Timestamp,
    pub event_type: AgentEventType,
    pub content: String,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventType {
    Output,
    ToolCall,
    ToolResult,
    Artifact,
    Thinking,
    Completion,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallInfo {
    pub tool_name: String,
    pub parameters: Value,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultInfo {
    pub tool_name: String,
    pub result: Value,
    pub duration_ms: u64,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInfo {
    pub artifact_id: String,
    pub artifact_type: ArtifactType,
    pub file_path: Option<String>,
    pub size_bytes: Option<u64>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    File,
    Code,
    Image,
    Document,
    Data,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionInfo {
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub success: bool,
    pub total_cost: Option<f64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub error_type: String,
    pub message: String,
    pub stacktrace: Option<String>,
}
