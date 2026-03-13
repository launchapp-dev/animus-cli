#![allow(dead_code)]

use async_graphql::{Enum, Object, SimpleObject, ID};
use serde::Deserialize;
use serde_json::Value;

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(rename_items = "SCREAMING_SNAKE_CASE")]
pub enum GqlTaskStatus {
    Backlog,
    Ready,
    InProgress,
    Blocked,
    OnHold,
    Done,
    Cancelled,
}

fn parse_task_status(s: &str) -> GqlTaskStatus {
    match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "backlog" | "todo" => GqlTaskStatus::Backlog,
        "ready" => GqlTaskStatus::Ready,
        "in-progress" | "inprogress" => GqlTaskStatus::InProgress,
        "blocked" => GqlTaskStatus::Blocked,
        "on-hold" | "onhold" => GqlTaskStatus::OnHold,
        "done" | "completed" => GqlTaskStatus::Done,
        "cancelled" => GqlTaskStatus::Cancelled,
        _ => GqlTaskStatus::Backlog,
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(rename_items = "SCREAMING_SNAKE_CASE")]
pub enum GqlTaskType {
    Feature,
    Bugfix,
    Hotfix,
    Refactor,
    Docs,
    Test,
    Chore,
    Experiment,
}

fn parse_task_type(s: &str) -> GqlTaskType {
    match s.trim().to_ascii_lowercase().as_str() {
        "feature" => GqlTaskType::Feature,
        "bugfix" | "bug" => GqlTaskType::Bugfix,
        "hotfix" | "hot-fix" => GqlTaskType::Hotfix,
        "refactor" => GqlTaskType::Refactor,
        "docs" | "documentation" | "doc" => GqlTaskType::Docs,
        "test" | "tests" | "testing" => GqlTaskType::Test,
        "chore" => GqlTaskType::Chore,
        "experiment" => GqlTaskType::Experiment,
        _ => GqlTaskType::Feature,
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(rename_items = "SCREAMING_SNAKE_CASE")]
pub enum GqlPriority {
    Critical,
    High,
    Medium,
    Low,
}

fn parse_priority(s: &str) -> GqlPriority {
    match s.trim().to_ascii_lowercase().as_str() {
        "critical" => GqlPriority::Critical,
        "high" => GqlPriority::High,
        "medium" => GqlPriority::Medium,
        "low" => GqlPriority::Low,
        _ => GqlPriority::Medium,
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(rename_items = "SCREAMING_SNAKE_CASE")]
pub enum GqlWorkflowStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Escalated,
    Cancelled,
}

fn parse_workflow_status(s: &str) -> GqlWorkflowStatus {
    match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "pending" => GqlWorkflowStatus::Pending,
        "running" => GqlWorkflowStatus::Running,
        "paused" => GqlWorkflowStatus::Paused,
        "completed" => GqlWorkflowStatus::Completed,
        "failed" => GqlWorkflowStatus::Failed,
        "escalated" => GqlWorkflowStatus::Escalated,
        "cancelled" => GqlWorkflowStatus::Cancelled,
        _ => GqlWorkflowStatus::Pending,
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(rename_items = "SCREAMING_SNAKE_CASE")]
pub enum GqlRequirementPriority {
    Must,
    Should,
    Could,
    Wont,
}

fn parse_requirement_priority(s: &str) -> GqlRequirementPriority {
    match s.trim().to_ascii_lowercase().as_str() {
        "must" => GqlRequirementPriority::Must,
        "should" => GqlRequirementPriority::Should,
        "could" => GqlRequirementPriority::Could,
        "wont" => GqlRequirementPriority::Wont,
        _ => GqlRequirementPriority::Should,
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(rename_items = "SCREAMING_SNAKE_CASE")]
pub enum GqlRequirementStatus {
    Draft,
    Refined,
    Planned,
    InProgress,
    Done,
    PoReview,
    EmReview,
    NeedsRework,
    Approved,
    Implemented,
    Deprecated,
}

fn parse_requirement_status(s: &str) -> GqlRequirementStatus {
    match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "draft" => GqlRequirementStatus::Draft,
        "refined" => GqlRequirementStatus::Refined,
        "planned" => GqlRequirementStatus::Planned,
        "in-progress" => GqlRequirementStatus::InProgress,
        "done" => GqlRequirementStatus::Done,
        "po-review" => GqlRequirementStatus::PoReview,
        "em-review" => GqlRequirementStatus::EmReview,
        "needs-rework" => GqlRequirementStatus::NeedsRework,
        "approved" => GqlRequirementStatus::Approved,
        "implemented" => GqlRequirementStatus::Implemented,
        "deprecated" => GqlRequirementStatus::Deprecated,
        _ => GqlRequirementStatus::Draft,
    }
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlPhaseExecution {
    pub phase_id: String,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub attempt: i32,
    pub error_message: Option<String>,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlDecision {
    pub timestamp: String,
    pub phase_id: String,
    pub source: String,
    pub decision: String,
    pub target_phase: Option<String>,
    pub reason: String,
    pub confidence: f64,
    pub risk: String,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlDaemonHealth {
    pub healthy: bool,
    pub status: String,
    pub runner_connected: bool,
    pub runner_pid: Option<i32>,
    pub active_agents: i32,
    pub daemon_pid: Option<i32>,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlAgentRun {
    pub run_id: String,
    pub task_id: Option<String>,
    pub task_title: Option<String>,
    pub workflow_id: Option<String>,
    pub phase_id: Option<String>,
    pub status: String,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlChecklist {
    pub id: String,
    pub description: String,
    pub completed: bool,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlDependency {
    pub task_id: String,
    pub dependency_type: String,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlAssignee {
    pub assignee_type: String,
    pub role: Option<String>,
    pub model: Option<String>,
    pub user_id: Option<String>,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlDaemonLog {
    pub timestamp: String,
    pub level: String,
    pub message: String,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlSystemInfo {
    pub platform: String,
    pub arch: String,
    pub version: String,
    pub daemon_running: bool,
    pub daemon_status: String,
    pub project_root: String,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlQueueStats {
    pub depth: i32,
    pub pending: i32,
    pub assigned: i32,
    pub held: i32,
    pub throughput_last_hour: i32,
    pub avg_wait_time_secs: i32,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlTaskStats {
    pub total: i32,
    pub in_progress: i32,
    pub blocked: i32,
    pub completed: i32,
    pub by_status: String,
    pub by_priority: String,
    pub by_type: String,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlWorkflowCheckpoint {
    pub number: i32,
    pub timestamp: String,
    pub reason: String,
    pub phase_id: Option<String>,
    pub status: String,
}

// --- Raw deserialization structs ---

#[derive(Debug, Clone, Deserialize)]
pub struct RawChecklist {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub completed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawDependency {
    pub task_id: String,
    #[serde(default)]
    pub dependency_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawAssignee {
    #[serde(rename = "type", default)]
    pub assignee_type: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawTask {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(rename = "type", default)]
    pub task_type: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub risk: String,
    #[serde(default)]
    pub complexity: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub assignee: Option<RawAssignee>,
    #[serde(default)]
    pub checklist: Vec<RawChecklist>,
    #[serde(default)]
    pub dependencies: Vec<RawDependency>,
    #[serde(default)]
    pub linked_requirements: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: Option<RawTaskMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawTaskMetadata {
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawRequirement {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub status: String,
    #[serde(rename = "type", default)]
    pub requirement_type: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub linked_task_ids: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawWorkflow {
    pub id: String,
    pub task_id: String,
    #[serde(default)]
    pub workflow_ref: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub current_phase: Option<String>,
    #[serde(default)]
    pub current_phase_index: Option<u32>,
    #[serde(default)]
    pub phases: Vec<RawPhaseExecution>,
    #[serde(default)]
    #[allow(dead_code)]
    pub decision_history: Vec<RawDecision>,
    #[serde(default)]
    pub total_reworks: u32,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPhaseExecution {
    pub phase_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawDecision {
    #[serde(default)]
    pub timestamp: String,
    pub phase_id: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub target_phase: Option<String>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub confidence: f32,
    #[serde(default)]
    pub risk: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawProject {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub config: Option<RawProjectConfig>,
    #[serde(default)]
    pub metadata: Option<RawProjectMetadata>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawProjectConfig {
    #[serde(default)]
    pub project_type: Option<String>,
    #[serde(default)]
    pub tech_stack: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawProjectMetadata {
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawVision {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub markdown: Option<String>,
    #[serde(default)]
    pub problem_statement: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawQueueEntry {
    #[serde(default)]
    pub subject_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub assigned_at: Option<String>,
    #[serde(default)]
    pub held_at: Option<String>,
    #[serde(default)]
    pub task: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawDaemonStatus {
    #[serde(default)]
    pub healthy: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub runner_connected: bool,
    #[serde(default)]
    pub active_agents: i64,
    #[serde(default)]
    pub max_agents: Option<i64>,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub daemon_pid: Option<i64>,
}

// --- Gql wrapper types ---

pub struct GqlTask(pub RawTask);
pub struct GqlRequirement(pub RawRequirement);
pub struct GqlWorkflow(pub RawWorkflow);
pub struct GqlProject(pub RawProject);
pub struct GqlVision(pub RawVision);
pub struct GqlQueueEntry(pub RawQueueEntry);
pub struct GqlDaemonStatus(pub RawDaemonStatus);

#[Object]
impl GqlTask {
    async fn id(&self) -> ID {
        ID(self.0.id.clone())
    }
    async fn title(&self) -> &str {
        &self.0.title
    }
    async fn description(&self) -> &str {
        &self.0.description
    }
    async fn task_type(&self) -> GqlTaskType {
        parse_task_type(&self.0.task_type)
    }
    async fn status(&self) -> GqlTaskStatus {
        parse_task_status(&self.0.status)
    }
    async fn priority(&self) -> GqlPriority {
        parse_priority(&self.0.priority)
    }
    async fn risk(&self) -> &str {
        &self.0.risk
    }
    async fn complexity(&self) -> &str {
        &self.0.complexity
    }
    async fn scope(&self) -> &str {
        &self.0.scope
    }
    async fn assignee(&self) -> Option<GqlAssignee> {
        self.0.assignee.as_ref().map(|a| GqlAssignee {
            assignee_type: a.assignee_type.clone(),
            role: a.role.clone(),
            model: a.model.clone(),
            user_id: a.user_id.clone(),
        })
    }
    async fn checklist(&self) -> Vec<GqlChecklist> {
        self.0
            .checklist
            .iter()
            .map(|c| GqlChecklist {
                id: c.id.clone(),
                description: c.description.clone(),
                completed: c.completed,
            })
            .collect()
    }
    async fn dependencies(&self) -> Vec<GqlDependency> {
        self.0
            .dependencies
            .iter()
            .map(|d| GqlDependency {
                task_id: d.task_id.clone(),
                dependency_type: d.dependency_type.clone(),
            })
            .collect()
    }
    async fn tags(&self) -> &[String] {
        &self.0.tags
    }
    async fn linked_requirement_ids(&self) -> &[String] {
        &self.0.linked_requirements
    }
    async fn created_at(&self) -> Option<&str> {
        self.0
            .metadata
            .as_ref()
            .and_then(|m| m.created_at.as_deref())
    }
    async fn updated_at(&self) -> Option<&str> {
        self.0
            .metadata
            .as_ref()
            .and_then(|m| m.updated_at.as_deref())
    }
    async fn requirements(
        &self,
        ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<Vec<GqlRequirement>> {
        let api = ctx.data::<orchestrator_web_api::WebApiService>()?;
        let mut result = Vec::new();
        for req_id in &self.0.linked_requirements {
            if let Ok(val) = api.requirements_get(req_id).await {
                if let Ok(raw) = serde_json::from_value::<RawRequirement>(val) {
                    result.push(GqlRequirement(raw));
                }
            }
        }
        Ok(result)
    }
}

#[Object]
impl GqlRequirement {
    async fn id(&self) -> ID {
        ID(self.0.id.clone())
    }
    async fn title(&self) -> &str {
        &self.0.title
    }
    async fn description(&self) -> &str {
        &self.0.description
    }
    async fn priority(&self) -> GqlRequirementPriority {
        parse_requirement_priority(&self.0.priority)
    }
    async fn status(&self) -> GqlRequirementStatus {
        parse_requirement_status(&self.0.status)
    }
    async fn requirement_type(&self) -> Option<&str> {
        self.0.requirement_type.as_deref()
    }
    async fn category(&self) -> Option<&str> {
        self.0.category.as_deref()
    }
    async fn acceptance_criteria(&self) -> &[String] {
        &self.0.acceptance_criteria
    }
    async fn source(&self) -> Option<&str> {
        self.0.source.as_deref()
    }
    async fn tags(&self) -> &[String] {
        &self.0.tags
    }
    async fn linked_task_ids(&self) -> &[String] {
        &self.0.linked_task_ids
    }
    async fn created_at(&self) -> Option<&str> {
        self.0.created_at.as_deref()
    }
    async fn updated_at(&self) -> Option<&str> {
        self.0.updated_at.as_deref()
    }
}

#[Object]
impl GqlWorkflow {
    async fn id(&self) -> ID {
        ID(self.0.id.clone())
    }
    async fn task_id(&self) -> &str {
        &self.0.task_id
    }
    async fn workflow_ref(&self) -> Option<&str> {
        self.0.workflow_ref.as_deref()
    }
    async fn status(&self) -> GqlWorkflowStatus {
        parse_workflow_status(&self.0.status)
    }
    async fn current_phase(&self) -> Option<&str> {
        self.0.current_phase.as_deref()
    }
    async fn current_phase_index(&self) -> Option<i32> {
        self.0.current_phase_index.map(|v| v as i32)
    }
    async fn total_reworks(&self) -> i32 {
        self.0.total_reworks as i32
    }
    async fn started_at(&self) -> Option<&str> {
        self.0.started_at.as_deref()
    }
    async fn completed_at(&self) -> Option<&str> {
        self.0.completed_at.as_deref()
    }
    async fn phases(&self) -> Vec<GqlPhaseExecution> {
        self.0
            .phases
            .iter()
            .map(|p| GqlPhaseExecution {
                phase_id: p.phase_id.clone(),
                status: p.status.clone(),
                started_at: p.started_at.clone(),
                completed_at: p.completed_at.clone(),
                attempt: p.attempt as i32,
                error_message: p.error_message.clone(),
            })
            .collect()
    }
    async fn decisions(
        &self,
        ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<Vec<GqlDecision>> {
        let api = ctx.data::<orchestrator_web_api::WebApiService>()?;
        match api.workflows_decisions(&self.0.id).await {
            Ok(val) => {
                let decisions: Vec<RawDecision> =
                    serde_json::from_value(val).unwrap_or_default();
                Ok(decisions
                    .into_iter()
                    .map(|d| GqlDecision {
                        timestamp: d.timestamp,
                        phase_id: d.phase_id,
                        source: d.source,
                        decision: d.decision,
                        target_phase: d.target_phase,
                        reason: d.reason,
                        confidence: d.confidence as f64,
                        risk: d.risk,
                    })
                    .collect())
            }
            Err(_) => Ok(vec![]),
        }
    }
}

#[Object]
impl GqlProject {
    async fn id(&self) -> ID {
        ID(self.0.id.clone())
    }
    async fn name(&self) -> &str {
        &self.0.name
    }
    async fn path(&self) -> &str {
        &self.0.path
    }
    async fn description(&self) -> Option<&str> {
        self.0
            .metadata
            .as_ref()
            .and_then(|m| m.description.as_deref())
    }
    async fn project_type(&self) -> Option<&str> {
        self.0
            .config
            .as_ref()
            .and_then(|c| c.project_type.as_deref())
    }
    async fn tech_stack(&self) -> Vec<String> {
        self.0
            .config
            .as_ref()
            .map(|c| c.tech_stack.clone())
            .unwrap_or_default()
    }
    async fn archived(&self) -> bool {
        self.0.archived
    }
    async fn created_at(&self) -> Option<&str> {
        self.0.created_at.as_deref()
    }
    async fn updated_at(&self) -> Option<&str> {
        self.0.updated_at.as_deref()
    }
}

#[Object]
impl GqlVision {
    async fn id(&self) -> Option<&str> {
        self.0.id.as_deref()
    }
    async fn content(&self) -> Option<&str> {
        self.0.markdown.as_deref()
    }
    async fn problem_statement(&self) -> Option<&str> {
        self.0.problem_statement.as_deref()
    }
    async fn created_at(&self) -> Option<&str> {
        self.0.created_at.as_deref()
    }
    async fn updated_at(&self) -> Option<&str> {
        self.0.updated_at.as_deref()
    }
}

#[Object]
impl GqlQueueEntry {
    async fn subject_id(&self) -> Option<&str> {
        self.0.subject_id.as_deref()
    }
    async fn task_id(&self) -> Option<&str> {
        self.0.task_id.as_deref()
    }
    async fn task_title(&self) -> Option<String> {
        self.0
            .task
            .as_ref()
            .and_then(|t| t.get("title"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
    async fn priority(&self) -> Option<String> {
        self.0
            .task
            .as_ref()
            .and_then(|t| t.get("priority"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
    async fn status(&self) -> Option<&str> {
        self.0.status.as_deref()
    }
    async fn workflow_id(&self) -> Option<&str> {
        self.0.workflow_id.as_deref()
    }
    async fn assigned_at(&self) -> Option<&str> {
        self.0.assigned_at.as_deref()
    }
    async fn held_at(&self) -> Option<&str> {
        self.0.held_at.as_deref()
    }
}

#[Object]
impl GqlDaemonStatus {
    async fn healthy(&self) -> bool {
        self.0.healthy
    }
    async fn status(&self) -> &str {
        &self.0.status
    }
    async fn runner_connected(&self) -> bool {
        self.0.runner_connected
    }
    async fn active_agents(&self) -> i32 {
        self.0.active_agents as i32
    }
    async fn max_agents(&self) -> Option<i32> {
        self.0.max_agents.map(|v| v as i32)
    }
    async fn project_root(&self) -> Option<&str> {
        self.0.project_root.as_deref()
    }
    async fn daemon_pid(&self) -> Option<i32> {
        self.0.daemon_pid.map(|v| v as i32)
    }
}
