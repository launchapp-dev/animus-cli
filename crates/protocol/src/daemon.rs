use crate::common::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadVisualRequirementsRequest {
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadVisualRequirementsResponse {
    pub requirements: Vec<RequirementNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementNode {
    pub id: RequirementId,
    pub title: String,
    pub description: Option<String>,
    pub r#type: RequirementType,
    pub priority: RequirementPriority,
    pub status: Status,
    pub tags: Vec<String>,
    pub position: NodePosition,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodePosition {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequirementType {
    Functional,
    NonFunctional,
    Technical,
    Product,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateRequirementsRequest {
    pub project_id: ProjectId,
    pub prompt: String,
    pub model: ModelId,
    pub project_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateRequirementsResponse {
    pub run_id: RunId,
    pub requirements: Vec<RequirementNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStatsRequest {
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStatsResponse {
    pub stats: ProjectStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStats {
    pub total_requirements: u32,
    pub total_tasks: u32,
    pub completed_tasks: u32,
    pub active_agents: u32,
    pub total_cost: f64,
    pub recent_activity: Vec<ActivityEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub id: String,
    pub timestamp: Timestamp,
    pub activity_type: ActivityType,
    pub title: String,
    pub description: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityType {
    AgentStarted,
    AgentCompleted,
    AgentFailed,
    TaskCompleted,
    PhaseAdvanced,
    RequirementCreated,
    WorkflowStarted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonEvent {
    AiStreamChunk {
        run_id: RunId,
        text: String,
    },
    AiStreamComplete {
        run_id: RunId,
        cost: Option<f64>,
        tokens: Option<TokenUsage>,
    },
    RequirementUpdated {
        project_id: ProjectId,
        requirement: RequirementNode,
    },
    WorkflowPhaseChanged {
        project_id: ProjectId,
        phase: String,
        status: String,
    },
    AgentStatusChanged {
        agent_id: AgentId,
        status: AgentStatus,
        elapsed_ms: Option<u64>,
    },
    AgentOutputChunk {
        agent_id: AgentId,
        stream_type: OutputStreamType,
        text: String,
    },
    ActivityUpdate {
        project_id: ProjectId,
        activity: ActivityEntry,
    },
    StatsUpdate {
        project_id: ProjectId,
        stats: ProjectStats,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Starting,
    Running,
    Paused,
    Completed,
    Failed,
    Timeout,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputStreamType {
    Stdout,
    Stderr,
    System,
}
