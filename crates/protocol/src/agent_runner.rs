use crate::common::*;
use crate::daemon::{AgentStatus, OutputStreamType};
use crate::output::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunRequest {
    pub protocol_version: String,
    pub run_id: RunId,
    pub model: ModelId,
    pub context: Value,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentRunEvent {
    Started {
        run_id: RunId,
        timestamp: Timestamp,
    },
    OutputChunk {
        run_id: RunId,
        stream_type: OutputStreamType,
        text: String,
    },
    Metadata {
        run_id: RunId,
        cost: Option<f64>,
        tokens: Option<TokenUsage>,
    },
    Error {
        run_id: RunId,
        error: String,
    },
    Finished {
        run_id: RunId,
        exit_code: Option<i32>,
        duration_ms: u64,
    },
    ToolCall {
        run_id: RunId,
        tool_info: ToolCallInfo,
    },
    ToolResult {
        run_id: RunId,
        result_info: ToolResultInfo,
    },
    Artifact {
        run_id: RunId,
        artifact_info: ArtifactInfo,
    },
    Thinking {
        run_id: RunId,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentControlRequest {
    pub run_id: RunId,
    pub action: AgentControlAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentControlAction {
    Pause,
    Resume,
    Terminate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentControlResponse {
    pub run_id: RunId,
    pub success: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatusRequest {
    pub run_id: RunId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatusResponse {
    pub run_id: RunId,
    pub status: AgentStatus,
    pub elapsed_ms: u64,
    pub started_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatusRequest {
    pub models: Vec<ModelId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerStatusRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerStatusResponse {
    pub active_agents: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatusResponse {
    pub statuses: Vec<ModelStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    pub model: ModelId,
    pub availability: ModelAvailability,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    Available,
    MissingCli,
    MissingApiKey,
    Disabled,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectModelConfig {
    pub project_id: ProjectId,
    pub allowed_models: Vec<ModelId>,
    pub phase_defaults: WorkflowPhaseModelDefaults,
    pub fallback_model: Option<ModelId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowPhaseModelDefaults {
    pub design: Option<ModelId>,
    pub development: Option<ModelId>,
    pub quality_assurance: Option<ModelId>,
    pub review: Option<ModelId>,
    pub deploy: Option<ModelId>,
}
