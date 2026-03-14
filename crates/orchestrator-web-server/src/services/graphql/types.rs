use async_graphql::{Enum, Object, SimpleObject, ID};
use orchestrator_core::{FileServiceHub, ServiceHub};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Enum types
// ---------------------------------------------------------------------------

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GqlTaskStatus {
    Backlog,
    Ready,
    InProgress,
    Blocked,
    OnHold,
    Done,
    Cancelled,
}

impl GqlTaskStatus {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "backlog" | "todo" => Self::Backlog,
            "ready" => Self::Ready,
            "in-progress" | "inprogress" => Self::InProgress,
            "blocked" => Self::Blocked,
            "on-hold" | "onhold" => Self::OnHold,
            "done" | "completed" => Self::Done,
            "cancelled" => Self::Cancelled,
            _ => Self::Backlog,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
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

impl GqlTaskType {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "feature" => Self::Feature,
            "bugfix" | "bug" => Self::Bugfix,
            "hotfix" | "hot-fix" => Self::Hotfix,
            "refactor" => Self::Refactor,
            "docs" | "documentation" | "doc" => Self::Docs,
            "test" | "tests" | "testing" => Self::Test,
            "chore" => Self::Chore,
            "experiment" => Self::Experiment,
            _ => Self::Feature,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GqlPriority {
    Critical,
    High,
    Medium,
    Low,
}

impl GqlPriority {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "critical" => Self::Critical,
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::Medium,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GqlWorkflowStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Escalated,
    Cancelled,
}

impl GqlWorkflowStatus {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "escalated" => Self::Escalated,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GqlDaemonStatusValue {
    Starting,
    Running,
    Paused,
    Stopping,
    Stopped,
    Crashed,
}

impl GqlDaemonStatusValue {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "starting" => Self::Starting,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "stopping" => Self::Stopping,
            "stopped" => Self::Stopped,
            "crashed" => Self::Crashed,
            _ => Self::Stopped,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GqlRequirementPriority {
    Must,
    Should,
    Could,
    Wont,
}

impl GqlRequirementPriority {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "must" => Self::Must,
            "should" => Self::Should,
            "could" => Self::Could,
            "wont" => Self::Wont,
            _ => Self::Should,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
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

impl GqlRequirementStatus {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "draft" => Self::Draft,
            "refined" => Self::Refined,
            "planned" => Self::Planned,
            "in-progress" => Self::InProgress,
            "done" => Self::Done,
            "po-review" => Self::PoReview,
            "em-review" => Self::EmReview,
            "needs-rework" => Self::NeedsRework,
            "approved" => Self::Approved,
            "implemented" => Self::Implemented,
            "deprecated" => Self::Deprecated,
            _ => Self::Draft,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GqlRequirementType {
    Product,
    Functional,
    NonFunctional,
    Technical,
    Other,
}

impl GqlRequirementType {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "product" => Self::Product,
            "functional" => Self::Functional,
            "non-functional" | "nonfunctional" => Self::NonFunctional,
            "technical" => Self::Technical,
            "other" => Self::Other,
            _ => Self::Other,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GqlRiskLevel {
    High,
    Medium,
    Low,
}

impl GqlRiskLevel {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::Medium,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GqlScope {
    Large,
    Medium,
    Small,
}

impl GqlScope {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "large" => Self::Large,
            "medium" => Self::Medium,
            "small" => Self::Small,
            _ => Self::Medium,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GqlComplexity {
    High,
    Medium,
    Low,
}

impl GqlComplexity {
    pub fn from_str_val(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::Medium,
        }
    }
}

// ---------------------------------------------------------------------------
// SimpleObject types (flat structs without nested resolvers)
// ---------------------------------------------------------------------------

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlWorkflowDefinition {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub phases: Vec<String>,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlPhaseOutput {
    pub lines: Vec<String>,
    pub phase_id: String,
    pub has_more: bool,
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
    #[graphql(name = "type")]
    pub dependency_type: String,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlWorkflowCheckpoint {
    pub id: String,
    pub phase: String,
    pub timestamp: Option<String>,
    pub data: Option<String>,
}

#[derive(SimpleObject)]
pub struct GqlTaskConnection {
    pub items: Vec<GqlTask>,
    pub total_count: i32,
}

#[derive(SimpleObject)]
pub struct GqlRequirementConnection {
    pub items: Vec<GqlRequirement>,
    pub total_count: i32,
}

#[derive(SimpleObject)]
pub struct GqlWorkflowConnection {
    pub items: Vec<GqlWorkflow>,
    pub total_count: i32,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlDaemonLog {
    pub timestamp: Option<String>,
    pub level: Option<String>,
    pub message: Option<String>,
    pub fields: Option<String>,
}

#[derive(SimpleObject, Debug, Clone)]
pub struct GqlSystemInfo {
    pub platform: Option<String>,
    pub arch: Option<String>,
    pub version: Option<String>,
    pub daemon_status: Option<String>,
    pub project_root: Option<String>,
}

// ---------------------------------------------------------------------------
// Raw deserialization types
// ---------------------------------------------------------------------------

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
    pub scope: String,
    #[serde(default)]
    pub complexity: String,
    #[serde(default)]
    pub linked_requirements: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub checklist: Vec<RawChecklist>,
    #[serde(default)]
    pub dependencies: Vec<RawDependency>,
}

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
    #[serde(default)]
    pub task_id: String,
    #[serde(default, rename = "dependency_type")]
    pub dependency_type: String,
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
    pub linked_task_ids: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
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
    pub phases: Vec<RawPhaseExecution>,
    #[serde(default)]
    pub decision_history: Vec<RawDecision>,
    #[serde(default)]
    pub total_reworks: u32,
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

// ---------------------------------------------------------------------------
// Value-wrapping GQL object types
// ---------------------------------------------------------------------------

pub struct GqlTask(pub RawTask);
pub struct GqlRequirement(pub RawRequirement);
pub struct GqlWorkflow(pub RawWorkflow);

pub struct GqlProject(pub serde_json::Value);
pub struct GqlVision(pub serde_json::Value);
pub struct GqlQueueEntry(pub serde_json::Value);
pub struct GqlQueueStats(pub serde_json::Value);
pub struct GqlTaskStats(pub serde_json::Value);
pub struct GqlDaemonStatus(pub serde_json::Value);

// ---------------------------------------------------------------------------
// GqlTask Object impl
// ---------------------------------------------------------------------------

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
        GqlTaskType::from_str_val(&self.0.task_type)
    }
    async fn task_type_raw(&self) -> &str {
        &self.0.task_type
    }
    async fn status(&self) -> GqlTaskStatus {
        GqlTaskStatus::from_str_val(&self.0.status)
    }
    async fn status_raw(&self) -> &str {
        &self.0.status
    }
    async fn priority(&self) -> GqlPriority {
        GqlPriority::from_str_val(&self.0.priority)
    }
    async fn priority_raw(&self) -> &str {
        &self.0.priority
    }
    async fn risk(&self) -> GqlRiskLevel {
        GqlRiskLevel::from_str_val(&self.0.risk)
    }
    async fn scope(&self) -> GqlScope {
        GqlScope::from_str_val(&self.0.scope)
    }
    async fn complexity(&self) -> GqlComplexity {
        GqlComplexity::from_str_val(&self.0.complexity)
    }
    async fn tags(&self) -> &[String] {
        &self.0.tags
    }
    async fn linked_requirement_ids(&self) -> &[String] {
        &self.0.linked_requirements
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
    async fn requirements(
        &self,
        ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<Vec<GqlRequirement>> {
        if self.0.linked_requirements.is_empty() {
            return Ok(vec![]);
        }
        let api = ctx.data::<orchestrator_web_api::WebApiService>()?;
        let all_val = api.requirements_list().await?;
        let all_reqs: Vec<RawRequirement> =
            serde_json::from_value(all_val).unwrap_or_default();
        let linked: std::collections::HashSet<&str> = self
            .0
            .linked_requirements
            .iter()
            .map(|s| s.as_str())
            .collect();
        Ok(all_reqs
            .into_iter()
            .filter(|r| linked.contains(r.id.as_str()))
            .map(GqlRequirement)
            .collect())
    }
}

// ---------------------------------------------------------------------------
// GqlRequirement Object impl
// ---------------------------------------------------------------------------

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
        GqlRequirementPriority::from_str_val(&self.0.priority)
    }
    async fn priority_raw(&self) -> &str {
        &self.0.priority
    }
    async fn status(&self) -> GqlRequirementStatus {
        GqlRequirementStatus::from_str_val(&self.0.status)
    }
    async fn status_raw(&self) -> &str {
        &self.0.status
    }
    async fn requirement_type(&self) -> Option<GqlRequirementType> {
        self.0
            .requirement_type
            .as_deref()
            .map(GqlRequirementType::from_str_val)
    }
    async fn tags(&self) -> &[String] {
        &self.0.tags
    }
    async fn linked_task_ids(&self) -> &[String] {
        &self.0.linked_task_ids
    }
    async fn acceptance_criteria(&self) -> &[String] {
        &self.0.acceptance_criteria
    }
}

// ---------------------------------------------------------------------------
// GqlWorkflow Object impl
// ---------------------------------------------------------------------------

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
        GqlWorkflowStatus::from_str_val(&self.0.status)
    }
    async fn status_raw(&self) -> &str {
        &self.0.status
    }
    async fn current_phase(&self) -> Option<&str> {
        self.0.current_phase.as_deref()
    }
    async fn total_reworks(&self) -> i32 {
        self.0.total_reworks as i32
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

// ---------------------------------------------------------------------------
// GqlProject Object impl (wraps serde_json::Value)
// ---------------------------------------------------------------------------

#[Object]
impl GqlProject {
    async fn id(&self) -> ID {
        ID(self.0.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string())
    }
    async fn name(&self) -> Option<String> {
        self.0.get("name").and_then(|v| v.as_str()).map(String::from)
    }
    async fn path(&self) -> Option<String> {
        self.0.get("path").and_then(|v| v.as_str()).map(String::from)
    }
    async fn description(&self) -> Option<String> {
        self.0
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from)
    }
    #[graphql(name = "type")]
    async fn project_type(&self) -> Option<String> {
        self.0.get("type").and_then(|v| v.as_str()).map(String::from)
    }
    async fn tech_stack(&self) -> Vec<String> {
        self.0
            .get("tech_stack")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
    async fn archived(&self) -> bool {
        self.0
            .get("archived")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
    async fn metadata(&self) -> Option<String> {
        self.0.get("metadata").map(|v| v.to_string())
    }
    async fn tasks(
        &self,
        _ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<Vec<GqlTask>> {
        let path = match self.0.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return Ok(vec![]),
        };
        let hub = FileServiceHub::new(path)?;
        let tasks = match hub.tasks().list().await {
            Ok(t) => t,
            Err(_) => return Ok(vec![]),
        };
        let val = serde_json::to_value(&tasks).unwrap_or_default();
        let raw: Vec<RawTask> = serde_json::from_value(val).unwrap_or_default();
        Ok(raw.into_iter().map(GqlTask).collect())
    }
    async fn workflows(
        &self,
        _ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<Vec<GqlWorkflow>> {
        let path = match self.0.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return Ok(vec![]),
        };
        let hub = FileServiceHub::new(path)?;
        let workflows = match hub.workflows().list().await {
            Ok(w) => w,
            Err(_) => return Ok(vec![]),
        };
        let val = serde_json::to_value(&workflows).unwrap_or_default();
        let raw: Vec<RawWorkflow> = serde_json::from_value(val).unwrap_or_default();
        Ok(raw.into_iter().map(GqlWorkflow).collect())
    }
    async fn requirements(
        &self,
        _ctx: &async_graphql::Context<'_>,
    ) -> async_graphql::Result<Vec<GqlRequirement>> {
        let path = match self.0.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return Ok(vec![]),
        };
        let hub = FileServiceHub::new(path)?;
        let reqs = match hub.planning().list_requirements().await {
            Ok(r) => r,
            Err(_) => return Ok(vec![]),
        };
        let val = serde_json::to_value(&reqs).unwrap_or_default();
        let raw: Vec<RawRequirement> = serde_json::from_value(val).unwrap_or_default();
        Ok(raw.into_iter().map(GqlRequirement).collect())
    }
}

// ---------------------------------------------------------------------------
// GqlVision Object impl (wraps serde_json::Value)
// ---------------------------------------------------------------------------

#[Object]
impl GqlVision {
    async fn title(&self) -> Option<String> {
        self.0.get("title").and_then(|v| v.as_str()).map(String::from)
    }
    async fn summary(&self) -> Option<String> {
        self.0
            .get("summary")
            .and_then(|v| v.as_str())
            .map(String::from)
    }
    async fn goals(&self) -> Vec<String> {
        self.0
            .get("goals")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
    async fn target_audience(&self) -> Option<String> {
        self.0
            .get("target_audience")
            .and_then(|v| v.as_str())
            .map(String::from)
    }
    async fn success_criteria(&self) -> Vec<String> {
        self.0
            .get("success_criteria")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
    async fn constraints(&self) -> Vec<String> {
        self.0
            .get("constraints")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
    async fn raw(&self) -> String {
        self.0.to_string()
    }
}

// ---------------------------------------------------------------------------
// GqlQueueEntry Object impl (wraps serde_json::Value)
// ---------------------------------------------------------------------------

#[Object]
impl GqlQueueEntry {
    async fn task_id(&self) -> String {
        self.0
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }
    async fn title(&self) -> Option<String> {
        self.0
            .get("title")
            .and_then(|v| v.as_str())
            .map(String::from)
    }
    async fn priority(&self) -> Option<GqlPriority> {
        self.0
            .get("priority")
            .and_then(|v| v.as_str())
            .map(GqlPriority::from_str_val)
    }
    async fn status(&self) -> Option<GqlTaskStatus> {
        self.0
            .get("status")
            .and_then(|v| v.as_str())
            .map(GqlTaskStatus::from_str_val)
    }
    async fn wait_time(&self) -> Option<f64> {
        self.0.get("wait_time").and_then(|v| v.as_f64())
    }
    async fn position(&self) -> Option<i32> {
        self.0
            .get("position")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32)
    }
}

// ---------------------------------------------------------------------------
// GqlQueueStats Object impl (wraps serde_json::Value)
// ---------------------------------------------------------------------------

#[Object]
impl GqlQueueStats {
    async fn depth(&self) -> i32 {
        self.0
            .get("depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32
    }
    async fn ready_count(&self) -> i32 {
        self.0
            .get("ready_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32
    }
    async fn held_count(&self) -> i32 {
        self.0
            .get("held_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32
    }
    async fn avg_wait(&self) -> Option<f64> {
        self.0.get("avg_wait").and_then(|v| v.as_f64())
    }
    async fn throughput(&self) -> Option<f64> {
        self.0.get("throughput").and_then(|v| v.as_f64())
    }
}

// ---------------------------------------------------------------------------
// GqlTaskStats Object impl (wraps serde_json::Value)
// ---------------------------------------------------------------------------

#[Object]
impl GqlTaskStats {
    async fn total(&self) -> i32 {
        self.0
            .get("total")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32
    }
    async fn by_status(&self) -> Option<String> {
        self.0.get("by_status").map(|v| v.to_string())
    }
    async fn by_priority(&self) -> Option<String> {
        self.0.get("by_priority").map(|v| v.to_string())
    }
    async fn by_type(&self) -> Option<String> {
        self.0.get("by_type").map(|v| v.to_string())
    }
    async fn raw(&self) -> String {
        self.0.to_string()
    }
}

// ---------------------------------------------------------------------------
// GqlDaemonStatus Object impl (wraps serde_json::Value)
// ---------------------------------------------------------------------------

#[Object]
impl GqlDaemonStatus {
    async fn healthy(&self) -> bool {
        self.0
            .get("healthy")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
    async fn status(&self) -> GqlDaemonStatusValue {
        self.0
            .get("status")
            .and_then(|v| v.as_str())
            .map(GqlDaemonStatusValue::from_str_val)
            .unwrap_or(GqlDaemonStatusValue::Stopped)
    }
    async fn status_raw(&self) -> Option<String> {
        self.0
            .get("status")
            .and_then(|v| v.as_str())
            .map(String::from)
    }
    async fn runner_connected(&self) -> bool {
        self.0
            .get("runner_connected")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
    async fn active_agents(&self) -> i32 {
        self.0
            .get("active_agents")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32
    }
    async fn max_agents(&self) -> Option<i32> {
        self.0
            .get("max_agents")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32)
    }
    async fn project_root(&self) -> Option<String> {
        self.0
            .get("project_root")
            .and_then(|v| v.as_str())
            .map(String::from)
    }
}
