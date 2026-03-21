use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub use crate::common::RequirementPriority;
pub use crate::daemon::RequirementType;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    #[serde(alias = "todo")]
    Backlog,
    Ready,
    #[serde(alias = "in_progress", alias = "inprogress")]
    InProgress,
    Blocked,
    #[serde(alias = "on_hold", alias = "onhold")]
    OnHold,
    #[serde(alias = "completed")]
    Done,
    Cancelled,
}

impl TaskStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::InProgress)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }

    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked | Self::OnHold)
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Backlog => "backlog",
            Self::Ready => "ready",
            Self::InProgress => "in-progress",
            Self::Blocked => "blocked",
            Self::OnHold => "on-hold",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        })
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_lowercase();
        Ok(match normalized.as_str() {
            "todo" | "backlog" => Self::Backlog,
            "ready" => Self::Ready,
            "in_progress" | "in-progress" => Self::InProgress,
            "done" | "completed" => Self::Done,
            "blocked" => Self::Blocked,
            "on_hold" | "on-hold" => Self::OnHold,
            "cancelled" => Self::Cancelled,
            _ => return Err(format!("unknown task status: {s}")),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskType {
    Feature,
    #[serde(alias = "bug")]
    Bugfix,
    #[serde(alias = "hot-fix")]
    Hotfix,
    Refactor,
    #[serde(alias = "documentation", alias = "doc")]
    Docs,
    #[serde(alias = "tests", alias = "testing")]
    Test,
    Chore,
    Experiment,
}

impl TaskType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Feature => "feature",
            Self::Bugfix => "bugfix",
            Self::Hotfix => "hotfix",
            Self::Refactor => "refactor",
            Self::Docs => "docs",
            Self::Test => "test",
            Self::Chore => "chore",
            Self::Experiment => "experiment",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Critical,
    High,
    Medium,
    Low,
}

impl Priority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    High,
    #[default]
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Large,
    #[default]
    Medium,
    Small,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Complexity {
    High,
    #[default]
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImpactArea {
    Frontend,
    Backend,
    Database,
    Api,
    Infrastructure,
    Docs,
    Tests,
    CiCd,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Assignee {
    Agent {
        role: String,
        model: Option<String>,
    },
    Human {
        user_id: String,
    },
    #[default]
    Unassigned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RequirementStatus {
    #[default]
    Draft,
    Refined,
    Planned,
    #[serde(alias = "in_progress")]
    InProgress,
    Done,
    PoReview,
    EmReview,
    NeedsRework,
    Approved,
    Implemented,
    Deprecated,
}

impl std::fmt::Display for RequirementStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Draft => "draft",
            Self::Refined => "refined",
            Self::Planned => "planned",
            Self::InProgress => "in-progress",
            Self::Done => "done",
            Self::PoReview => "po-review",
            Self::EmReview => "em-review",
            Self::NeedsRework => "needs-rework",
            Self::Approved => "approved",
            Self::Implemented => "implemented",
            Self::Deprecated => "deprecated",
        })
    }
}

impl std::str::FromStr for RequirementStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_lowercase().replace('_', "-");
        Ok(match normalized.as_str() {
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
            _ => return Err(format!("unknown requirement status: {s}")),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RequirementLinks {
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub workflows: Vec<String>,
    #[serde(default)]
    pub tests: Vec<String>,
    #[serde(default)]
    pub mockups: Vec<String>,
    #[serde(default)]
    pub flows: Vec<String>,
    #[serde(default)]
    pub related_requirements: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementComment {
    pub author: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub phase: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodebaseInsight {
    #[serde(default)]
    pub detected_stacks: Vec<String>,
    #[serde(default)]
    pub notable_paths: Vec<String>,
    #[serde(default)]
    pub file_count_scanned: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementItem {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub legacy_id: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(rename = "type", default)]
    pub requirement_type: Option<RequirementType>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub priority: RequirementPriority,
    #[serde(default)]
    pub status: RequirementStatus,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub links: RequirementLinks,
    #[serde(default)]
    pub comments: Vec<RequirementComment>,
    #[serde(default)]
    pub relative_path: Option<String>,
    #[serde(default)]
    pub linked_task_ids: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementFilter {
    #[serde(default)]
    pub status: Option<RequirementStatus>,
    #[serde(default)]
    pub priority: Option<RequirementPriority>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(rename = "type", default)]
    pub requirement_type: Option<RequirementType>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub linked_task_id: Option<String>,
    #[serde(default)]
    pub search_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RequirementQuerySort {
    #[default]
    Id,
    UpdatedAt,
    Priority,
    Status,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementQuery {
    #[serde(default)]
    pub filter: RequirementFilter,
    #[serde(default)]
    pub page: ListPageRequest,
    #[serde(default)]
    pub sort: RequirementQuerySort,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementsDraftInput {
    #[serde(default = "default_true")]
    pub include_codebase_scan: bool,
    #[serde(default = "default_true")]
    pub append_only: bool,
    #[serde(default = "default_requirements_limit")]
    pub max_requirements: usize,
}

impl Default for RequirementsDraftInput {
    fn default() -> Self {
        Self { include_codebase_scan: true, append_only: true, max_requirements: default_requirements_limit() }
    }
}

const fn default_true() -> bool {
    true
}

const fn default_requirements_limit() -> usize {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementsDraftResult {
    pub requirements: Vec<RequirementItem>,
    pub appended_count: usize,
    #[serde(default)]
    pub codebase_insight: Option<CodebaseInsight>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RequirementsRefineInput {
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    #[serde(default)]
    pub focus: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RequirementsExecutionInput {
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    #[serde(default)]
    pub start_workflows: bool,
    #[serde(default)]
    pub workflow_ref: Option<String>,
    #[serde(default)]
    pub include_wont: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RequirementsExecutionResult {
    pub requirements_considered: usize,
    #[serde(default)]
    pub task_ids_created: Vec<String>,
    #[serde(default)]
    pub task_ids_reused: Vec<String>,
    #[serde(default)]
    pub workflow_ids_started: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyType {
    BlocksBy,
    BlockedBy,
    RelatedTo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDependency {
    pub task_id: String,
    pub dependency_type: DependencyType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub id: String,
    pub description: String,
    pub completed: bool,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowMetadata {
    pub workflow_id: Option<String>,
    pub requires_design: bool,
    pub requires_architecture: bool,
    pub requires_qa: bool,
    pub requires_staging_deploy: bool,
    pub requires_production_deploy: bool,
}

impl Default for WorkflowMetadata {
    fn default() -> Self {
        Self {
            workflow_id: None,
            requires_design: false,
            requires_architecture: false,
            requires_qa: true,
            requires_staging_deploy: false,
            requires_production_deploy: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRequirements {
    pub max_cpu_percent: Option<f32>,
    pub max_memory_mb: Option<u64>,
    pub requires_network: bool,
}

impl Default for ResourceRequirements {
    fn default() -> Self {
        Self { max_cpu_percent: None, max_memory_mb: None, requires_network: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMetadata {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_by: String,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default = "default_task_version")]
    pub version: u32,
}

const fn default_task_version() -> u32 {
    1
}

pub const MAX_DISPATCH_HISTORY_ENTRIES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchHistoryEntry {
    pub workflow_id: String,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorTask {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(rename = "type")]
    pub task_type: TaskType,
    pub status: TaskStatus,
    #[serde(default)]
    pub blocked_reason: Option<String>,
    #[serde(default)]
    pub blocked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub blocked_phase: Option<String>,
    #[serde(default)]
    pub blocked_by: Option<String>,
    pub priority: Priority,
    #[serde(default)]
    pub risk: RiskLevel,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub complexity: Complexity,
    #[serde(default)]
    pub impact_area: Vec<ImpactArea>,
    #[serde(default)]
    pub assignee: Assignee,
    #[serde(default)]
    pub estimated_effort: Option<String>,
    #[serde(default)]
    pub linked_requirements: Vec<String>,
    #[serde(default)]
    pub linked_architecture_entities: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<TaskDependency>,
    #[serde(default)]
    pub checklist: Vec<ChecklistItem>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub workflow_metadata: WorkflowMetadata,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub branch_name: Option<String>,
    pub metadata: TaskMetadata,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default)]
    pub resource_requirements: ResourceRequirements,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_dispatch_failures: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_dispatch_failure_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatch_history: Vec<DispatchHistoryEntry>,
}

const FRONTEND_TAGS: &[&str] =
    &["frontend", "ui", "ux", "design", "react", "web", "landing-page", "design-system", "nextjs"];

const FRONTEND_TOKENS: &[&str] =
    &["frontend", "ui", "ux", "react", "tailwind", "css", "component", "storybook", "wireframe", "mockup", "nextjs"];

const FRONTEND_PHRASES: &[&str] = &["user interface", "user experience", "design system", "landing page"];

pub fn is_frontend_related_content(tags: &[String], text: &str) -> bool {
    if tags.iter().any(|tag| {
        let normalized = tag.trim().to_ascii_lowercase();
        FRONTEND_TAGS.contains(&normalized.as_str())
    }) {
        return true;
    }

    let haystack = text.to_ascii_lowercase();
    let tokenized: String =
        haystack.chars().map(|character| if character.is_ascii_alphanumeric() { character } else { ' ' }).collect();
    let tokens: HashSet<&str> = tokenized.split_whitespace().collect();

    if FRONTEND_TOKENS.iter().any(|needle| tokens.contains(needle)) {
        return true;
    }

    FRONTEND_PHRASES.iter().any(|needle| haystack.contains(needle))
}

impl OrchestratorTask {
    pub fn is_frontend_related(&self) -> bool {
        if self.workflow_metadata.requires_design {
            return true;
        }

        if self.impact_area.iter().any(|area| matches!(area, ImpactArea::Frontend)) {
            return true;
        }

        let text = format!("{} {}", self.title, self.description);
        is_frontend_related_content(&self.tags, &text)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCreateInput {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub task_type: Option<TaskType>,
    #[serde(default)]
    pub priority: Option<Priority>,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub linked_requirements: Vec<String>,
    #[serde(default)]
    pub linked_architecture_entities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskUpdateInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: Option<Priority>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub updated_by: Option<String>,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub linked_architecture_entities: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFilter {
    pub task_type: Option<TaskType>,
    pub status: Option<TaskStatus>,
    pub priority: Option<Priority>,
    pub risk: Option<RiskLevel>,
    pub assignee_type: Option<String>,
    pub tags: Option<Vec<String>>,
    pub linked_requirement: Option<String>,
    pub linked_architecture_entity: Option<String>,
    pub search_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskQuerySort {
    #[default]
    Priority,
    UpdatedAt,
    CreatedAt,
    Id,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ListPageRequest {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: usize,
}

impl ListPageRequest {
    pub const fn unbounded() -> Self {
        Self { limit: None, offset: 0 }
    }

    pub fn bounds(self, total: usize) -> (usize, usize) {
        let start = self.offset.min(total);
        let remaining = total.saturating_sub(start);
        let limit = self.limit.unwrap_or(remaining).min(remaining);
        (start, start.saturating_add(limit))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListPage<T> {
    pub items: Vec<T>,
    pub total: usize,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: usize,
    pub returned: usize,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

impl<T> ListPage<T> {
    pub fn new(items: Vec<T>, total: usize, request: ListPageRequest) -> Self {
        let offset = request.offset.min(total);
        let returned = items.len();
        let has_more = offset.saturating_add(returned) < total;
        let next_offset = has_more.then_some(offset.saturating_add(returned));

        Self { items, total, limit: request.limit, offset, returned, has_more, next_offset }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskQuery {
    #[serde(default)]
    pub filter: TaskFilter,
    #[serde(default)]
    pub page: ListPageRequest,
    #[serde(default)]
    pub sort: TaskQuerySort,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatistics {
    pub total: usize,
    pub by_status: HashMap<String, usize>,
    pub by_priority: HashMap<String, usize>,
    pub by_type: HashMap<String, usize>,
    pub in_progress: usize,
    pub blocked: usize,
    pub completed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DaemonStatus {
    Starting,
    Running,
    Paused,
    Stopping,
    #[default]
    Stopped,
    Crashed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonHealth {
    pub healthy: bool,
    pub status: DaemonStatus,
    pub runner_connected: bool,
    #[serde(default)]
    pub runner_pid: Option<u32>,
    #[serde(default)]
    pub active_agents: usize,
    #[serde(default, alias = "max_agents")]
    pub pool_size: Option<usize>,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub daemon_pid: Option<u32>,
    #[serde(default)]
    pub process_alive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_utilization_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_tasks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_agents_spawned: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_agents_completed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_agents_failed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_disk_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectType {
    #[serde(alias = "web_app")]
    WebApp,
    #[serde(alias = "mobile_app")]
    MobileApp,
    #[serde(alias = "desktop_app")]
    DesktopApp,
    #[serde(alias = "full_stack_platform")]
    FullStackPlatform,
    Library,
    Infrastructure,
    #[serde(rename = "other", alias = "greenfield", alias = "existing")]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectModelPreferences {
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub phase_overrides: HashMap<String, String>,
}

impl Default for ProjectModelPreferences {
    fn default() -> Self {
        Self {
            allowed_models: crate::default_model_specs().into_iter().map(|(model_id, _tool)| model_id).collect(),
            default_model: crate::default_model_for_tool("claude").map(str::to_string),
            phase_overrides: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConcurrencyLimits {
    pub max_workflows: usize,
    pub max_agents: usize,
}

impl Default for ProjectConcurrencyLimits {
    fn default() -> Self {
        Self { max_workflows: 3, max_agents: 10 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub project_type: ProjectType,
    #[serde(default)]
    pub tech_stack: Vec<String>,
    #[serde(default = "default_auto_commit")]
    pub auto_commit: bool,
    #[serde(default)]
    pub auto_push: bool,
    #[serde(default = "default_branch")]
    pub default_branch: String,
    #[serde(default)]
    pub model_preferences: ProjectModelPreferences,
    #[serde(default)]
    pub concurrency_limits: ProjectConcurrencyLimits,
    #[serde(default = "default_mcp_port")]
    pub mcp_port: u16,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            project_type: ProjectType::Other,
            tech_stack: Vec::new(),
            auto_commit: true,
            auto_push: false,
            default_branch: "main".to_string(),
            model_preferences: ProjectModelPreferences::default(),
            concurrency_limits: ProjectConcurrencyLimits::default(),
            mcp_port: default_mcp_port(),
        }
    }
}

const fn default_auto_commit() -> bool {
    true
}

fn default_branch() -> String {
    "main".to_string()
}

const fn default_mcp_port() -> u16 {
    3101
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectMetadata {
    #[serde(default)]
    pub problem_statement: Option<String>,
    #[serde(default)]
    pub target_users: Vec<String>,
    #[serde(default)]
    pub goals: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(flatten, default)]
    pub custom: HashMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ComplexityTier {
    Simple,
    #[default]
    Medium,
    Complex,
}

impl ComplexityTier {
    pub const fn task_count_range(self) -> (usize, usize) {
        match self {
            Self::Simple => (1, 2),
            Self::Medium => (2, 4),
            Self::Complex => (3, 6),
        }
    }

    pub const fn requirement_range_defaults(self) -> (usize, usize) {
        match self {
            Self::Simple => (4, 8),
            Self::Medium => (8, 14),
            Self::Complex => (14, 30),
        }
    }

    pub const fn dedicated_requirement_limit(self) -> usize {
        match self {
            Self::Simple => 2,
            Self::Medium => 3,
            Self::Complex => usize::MAX,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Medium => "medium",
            Self::Complex => "complex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskDensity {
    Low,
    #[default]
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementRange {
    pub min: usize,
    pub max: usize,
}

impl Default for RequirementRange {
    fn default() -> Self {
        Self { min: 8, max: 16 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityAssessment {
    #[serde(default)]
    pub tier: ComplexityTier,
    #[serde(default = "default_complexity_confidence")]
    pub confidence: f32,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub recommended_requirement_range: RequirementRange,
    #[serde(default)]
    pub task_density: TaskDensity,
    #[serde(default)]
    pub source: Option<String>,
}

impl Default for ComplexityAssessment {
    fn default() -> Self {
        Self {
            tier: ComplexityTier::Medium,
            confidence: default_complexity_confidence(),
            rationale: None,
            recommended_requirement_range: RequirementRange::default(),
            task_density: TaskDensity::Medium,
            source: Some("heuristic".to_string()),
        }
    }
}

fn default_complexity_confidence() -> f32 {
    0.55
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionDraftInput {
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub problem_statement: String,
    #[serde(default)]
    pub target_users: Vec<String>,
    #[serde(default)]
    pub goals: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub value_proposition: Option<String>,
    #[serde(default)]
    pub complexity_assessment: Option<ComplexityAssessment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionDocument {
    pub id: String,
    pub project_root: String,
    pub markdown: String,
    pub problem_statement: String,
    #[serde(default)]
    pub target_users: Vec<String>,
    #[serde(default)]
    pub goals: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub value_proposition: Option<String>,
    #[serde(default)]
    pub complexity_assessment: Option<ComplexityAssessment>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_architecture_schema() -> String {
    "ao.architecture.v1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureEntity {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub code_paths: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub relation: String,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureGraph {
    #[serde(default = "default_architecture_schema")]
    pub schema: String,
    #[serde(default)]
    pub entities: Vec<ArchitectureEntity>,
    #[serde(default)]
    pub edges: Vec<ArchitectureEdge>,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

impl Default for ArchitectureGraph {
    fn default() -> Self {
        Self {
            schema: default_architecture_schema(),
            entities: Vec::new(),
            edges: Vec::new(),
            metadata: HashMap::new(),
        }
    }
}

impl ArchitectureGraph {
    pub fn has_entity(&self, entity_id: &str) -> bool {
        self.entities.iter().any(|entity| entity.id == entity_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandoffTargetRole {
    Em,
    Po,
}

impl HandoffTargetRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Em => "em",
            Self::Po => "po",
        }
    }
}

impl TryFrom<&str> for HandoffTargetRole {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.trim().to_ascii_lowercase().as_str() {
            "em" | "engineering-manager" | "engineering_manager" => Ok(Self::Em),
            "po" | "pm" | "product-owner" | "product_owner" => Ok(Self::Po),
            other => Err(format!("Unsupported handoff target role: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHandoffRequestInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_id: Option<String>,
    pub run_id: String,
    pub target_role: HandoffTargetRole,
    pub question: String,
    #[serde(default)]
    pub context: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHandoffStatus {
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHandoffResult {
    pub handoff_id: String,
    pub run_id: String,
    pub root_run_id: String,
    pub workflow_id: String,
    pub target_role: HandoffTargetRole,
    pub status: AgentHandoffStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: u64,
    pub depth: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Escalated,
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowFilter {
    #[serde(default)]
    pub status: Option<WorkflowStatus>,
    #[serde(default)]
    pub workflow_ref: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub phase_id: Option<String>,
    #[serde(default)]
    pub search_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowQuerySort {
    #[default]
    StartedAt,
    Status,
    WorkflowRef,
    Id,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowQuery {
    #[serde(default)]
    pub filter: WorkflowFilter,
    #[serde(default)]
    pub page: ListPageRequest,
    #[serde(default)]
    pub sort: WorkflowQuerySort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowDecisionSource {
    Llm,
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowDecisionAction {
    Advance,
    Skip,
    Rework,
    Repeat,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowDecisionRisk {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDecisionRecord {
    pub timestamp: DateTime<Utc>,
    pub phase_id: String,
    pub source: WorkflowDecisionSource,
    pub decision: WorkflowDecisionAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_phase: Option<String>,
    pub reason: String,
    pub confidence: f32,
    pub risk: WorkflowDecisionRisk,
    #[serde(default)]
    pub guardrail_violations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PhaseDecisionVerdict {
    Advance,
    Rework,
    Fail,
    Skip,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseEvidenceKind {
    TestsPassed,
    TestsFailed,
    CodeReviewClean,
    CodeReviewIssues,
    FilesModified,
    RequirementsMet,
    ResearchComplete,
    ManualVerification,
    #[serde(other)]
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseEvidence {
    pub kind: PhaseEvidenceKind,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseDecision {
    pub kind: String,
    pub phase_id: String,
    pub verdict: PhaseDecisionVerdict,
    pub confidence: f32,
    pub risk: WorkflowDecisionRisk,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub evidence: Vec<PhaseEvidence>,
    #[serde(default)]
    pub guardrail_violations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_phase: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPhaseStatus {
    Pending,
    Ready,
    Running,
    Success,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowPhaseExecution {
    pub phase_id: String,
    pub status: WorkflowPhaseStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub attempt: u32,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowMachineState {
    #[default]
    Idle,
    EvaluateTransition,
    RunPhase,
    EvaluateGates,
    ApplyTransition,
    Paused,
    Completed,
    MergeConflict,
    Failed,
    HumanEscalated,
    Cancelled,
}

impl WorkflowMachineState {
    pub fn to_workflow_status(&self) -> WorkflowStatus {
        match self {
            Self::Idle => WorkflowStatus::Pending,
            Self::EvaluateTransition
            | Self::RunPhase
            | Self::EvaluateGates
            | Self::ApplyTransition
            | Self::MergeConflict => WorkflowStatus::Running,
            Self::Paused => WorkflowStatus::Paused,
            Self::Completed => WorkflowStatus::Completed,
            Self::Failed => WorkflowStatus::Failed,
            Self::HumanEscalated => WorkflowStatus::Escalated,
            Self::Cancelled => WorkflowStatus::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowMachineEvent {
    Start,
    PhaseStarted,
    PhaseSucceeded,
    PhaseFailed,
    GatesPassed,
    GatesFailed,
    PolicyDecisionReady,
    PolicyDecisionFailed,
    PauseRequested,
    ResumeRequested,
    CancelRequested,
    ReworkBudgetExceeded,
    HumanFeedbackProvided,
    MergeConflictDetected,
    MergeConflictResolved,
    NoMorePhases,
    PhaseSkipped,
    RetryPhaseStarted,
    PhaseTargetSelected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckpointReason {
    Start,
    Resume,
    Pause,
    Cancel,
    StatusChange,
    Recovery,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowCheckpoint {
    pub number: usize,
    pub timestamp: DateTime<Utc>,
    pub reason: CheckpointReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    pub machine_state: WorkflowMachineState,
    pub status: WorkflowStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowCheckpointMetadata {
    pub checkpoint_count: usize,
    pub checkpoints: Vec<WorkflowCheckpoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub message: String,
}

fn default_timestamp_now() -> DateTime<Utc> {
    Utc::now()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorProject {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub config: ProjectConfig,
    #[serde(default)]
    pub metadata: ProjectMetadata,
    #[serde(default = "default_timestamp_now")]
    pub created_at: DateTime<Utc>,
    #[serde(default = "default_timestamp_now")]
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorWorkflow {
    pub id: String,
    pub task_id: String,
    pub workflow_ref: Option<String>,
    #[serde(default = "default_workflow_subject_ref")]
    pub subject: SubjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vars: HashMap<String, String>,
    pub status: WorkflowStatus,
    pub current_phase_index: usize,
    #[serde(default)]
    pub phases: Vec<WorkflowPhaseExecution>,
    #[serde(default)]
    pub machine_state: WorkflowMachineState,
    pub current_phase: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub checkpoint_metadata: WorkflowCheckpointMetadata,
    #[serde(default)]
    pub rework_counts: HashMap<String, u32>,
    #[serde(default)]
    pub total_reworks: u32,
    #[serde(default)]
    pub decision_history: Vec<WorkflowDecisionRecord>,
}

fn default_workflow_subject_ref() -> SubjectRef {
    SubjectRef::task(String::new())
}

impl OrchestratorWorkflow {
    pub fn sync_status(&mut self) {
        self.status = self.machine_state.to_workflow_status();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCreateInput {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub project_type: Option<ProjectType>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tech_stack: Vec<String>,
    #[serde(default)]
    pub metadata: Option<ProjectMetadata>,
}

pub const DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT: u8 = 20;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPriorityDistribution {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPriorityPolicyReport {
    pub high_budget_percent: u8,
    pub high_budget_limit: usize,
    pub total_tasks: usize,
    pub active_tasks: usize,
    pub total_by_priority: TaskPriorityDistribution,
    pub active_by_priority: TaskPriorityDistribution,
    pub high_budget_compliant: bool,
    pub high_budget_overflow: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPriorityRebalanceChange {
    pub task_id: String,
    pub from: Priority,
    pub to: Priority,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPriorityRebalancePlan {
    pub high_budget_percent: u8,
    pub before: TaskPriorityPolicyReport,
    pub after: TaskPriorityPolicyReport,
    #[serde(default)]
    pub changes: Vec<TaskPriorityRebalanceChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPriorityRebalanceOptions {
    #[serde(default = "default_high_priority_budget_percent")]
    pub high_budget_percent: u8,
    #[serde(default)]
    pub essential_task_ids: Vec<String>,
    #[serde(default)]
    pub nice_to_have_task_ids: Vec<String>,
}

impl Default for TaskPriorityRebalanceOptions {
    fn default() -> Self {
        Self {
            high_budget_percent: DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT,
            essential_task_ids: Vec::new(),
            nice_to_have_task_ids: Vec::new(),
        }
    }
}

const fn default_high_priority_budget_percent() -> u8 {
    DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowSubject {
    Task { id: String },
    Requirement { id: String },
    Custom { title: String, description: String },
}

impl WorkflowSubject {
    pub fn id(&self) -> &str {
        match self {
            Self::Task { id } | Self::Requirement { id } => id,
            Self::Custom { title, .. } => title,
        }
    }
}

pub const SUBJECT_KIND_TASK: &str = "ao.task";
pub const SUBJECT_KIND_REQUIREMENT: &str = "ao.requirement";
pub const SUBJECT_KIND_CUSTOM: &str = "custom";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SubjectRefData {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default = "default_subject_metadata", skip_serializing_if = "subject_metadata_is_empty")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum SubjectRefWire {
    Legacy(WorkflowSubject),
    Generic(SubjectRefData),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubjectRef {
    pub kind: String,
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub labels: Vec<String>,
    pub metadata: Value,
}

impl SubjectRef {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            title: None,
            description: None,
            labels: Vec::new(),
            metadata: default_subject_metadata(),
        }
    }

    pub fn task(task_id: impl Into<String>) -> Self {
        Self::new(SUBJECT_KIND_TASK, task_id)
    }

    pub fn requirement(requirement_id: impl Into<String>) -> Self {
        Self::new(SUBJECT_KIND_REQUIREMENT, requirement_id)
    }

    pub fn custom(title: impl Into<String>, description: impl Into<String>) -> Self {
        let title = title.into();
        Self {
            kind: SUBJECT_KIND_CUSTOM.to_string(),
            id: title.clone(),
            title: Some(title),
            description: Some(description.into()),
            labels: Vec::new(),
            metadata: default_subject_metadata(),
        }
    }

    pub fn kind(&self) -> &str {
        self.kind.as_str()
    }

    pub fn id(&self) -> &str {
        self.id.as_str()
    }

    pub fn task_id(&self) -> Option<&str> {
        self.kind.eq_ignore_ascii_case(SUBJECT_KIND_TASK).then_some(self.id.as_str())
    }

    pub fn requirement_id(&self) -> Option<&str> {
        self.kind.eq_ignore_ascii_case(SUBJECT_KIND_REQUIREMENT).then_some(self.id.as_str())
    }

    pub fn schedule_id(&self) -> Option<&str> {
        self.kind.eq_ignore_ascii_case(SUBJECT_KIND_CUSTOM).then(|| self.id.strip_prefix("schedule:")).flatten()
    }

    pub fn subject_key(&self) -> String {
        if self.kind.eq_ignore_ascii_case(SUBJECT_KIND_TASK)
            || self.kind.eq_ignore_ascii_case(SUBJECT_KIND_REQUIREMENT)
            || self.kind.eq_ignore_ascii_case(SUBJECT_KIND_CUSTOM)
        {
            return self.id.clone();
        }

        format!("{}::{}", self.kind, self.id)
    }

    pub fn to_workflow_subject(&self) -> WorkflowSubject {
        if self.kind.eq_ignore_ascii_case(SUBJECT_KIND_TASK) {
            return WorkflowSubject::Task { id: self.id.clone() };
        }
        if self.kind.eq_ignore_ascii_case(SUBJECT_KIND_REQUIREMENT) {
            return WorkflowSubject::Requirement { id: self.id.clone() };
        }

        WorkflowSubject::Custom {
            title: self.title.clone().unwrap_or_else(|| self.id.clone()),
            description: self.description.clone().unwrap_or_default(),
        }
    }

    fn from_workflow_subject(subject: WorkflowSubject) -> Self {
        match subject {
            WorkflowSubject::Task { id } => Self::task(id),
            WorkflowSubject::Requirement { id } => Self::requirement(id),
            WorkflowSubject::Custom { title, description } => Self::custom(title, description),
        }
    }

    fn can_serialize_as_legacy(&self) -> bool {
        self.labels.is_empty()
            && subject_metadata_is_empty(&self.metadata)
            && (self.kind.eq_ignore_ascii_case(SUBJECT_KIND_TASK)
                || self.kind.eq_ignore_ascii_case(SUBJECT_KIND_REQUIREMENT)
                || self.kind.eq_ignore_ascii_case(SUBJECT_KIND_CUSTOM))
    }

    fn into_wire(self) -> SubjectRefWire {
        if self.can_serialize_as_legacy() {
            SubjectRefWire::Legacy(self.to_workflow_subject())
        } else {
            SubjectRefWire::Generic(SubjectRefData {
                kind: self.kind,
                id: self.id,
                title: self.title,
                description: self.description,
                labels: self.labels,
                metadata: self.metadata,
            })
        }
    }
}

impl Serialize for SubjectRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.clone().into_wire().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SubjectRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SubjectRefWire::deserialize(deserializer)?;
        Ok(match wire {
            SubjectRefWire::Legacy(subject) => SubjectRef::from_workflow_subject(subject),
            SubjectRefWire::Generic(data) => SubjectRef {
                kind: data.kind,
                id: data.id,
                title: data.title,
                description: data.description,
                labels: data.labels,
                metadata: data.metadata,
            },
        })
    }
}

fn default_subject_metadata() -> Value {
    Value::Object(serde_json::Map::new())
}

fn subject_metadata_is_empty(value: &Value) -> bool {
    matches!(value, Value::Null) || value.as_object().is_some_and(|object| object.is_empty())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubjectDispatch {
    pub subject: SubjectRef,
    pub workflow_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vars: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    pub trigger_source: String,
    pub requested_at: DateTime<Utc>,
}

impl SubjectDispatch {
    pub fn for_subject_with_metadata(
        subject: SubjectRef,
        workflow_ref: impl Into<String>,
        trigger_source: impl Into<String>,
        requested_at: DateTime<Utc>,
    ) -> Self {
        Self {
            subject,
            workflow_ref: workflow_ref.into(),
            input: None,
            vars: HashMap::new(),
            priority: None,
            trigger_source: trigger_source.into(),
            requested_at,
        }
    }

    pub fn for_task(task_id: impl Into<String>, workflow_ref: impl Into<String>) -> Self {
        Self::for_task_with_metadata(task_id, workflow_ref, "ready-queue", Utc::now())
    }

    pub fn for_task_with_metadata(
        task_id: impl Into<String>,
        workflow_ref: impl Into<String>,
        trigger_source: impl Into<String>,
        requested_at: DateTime<Utc>,
    ) -> Self {
        Self::for_subject_with_metadata(SubjectRef::task(task_id), workflow_ref, trigger_source, requested_at)
    }

    pub fn for_requirement(
        requirement_id: impl Into<String>,
        workflow_ref: impl Into<String>,
        trigger_source: impl Into<String>,
    ) -> Self {
        Self::for_subject_with_metadata(
            SubjectRef::requirement(requirement_id),
            workflow_ref,
            trigger_source,
            Utc::now(),
        )
    }

    pub fn for_custom(
        title: impl Into<String>,
        description: impl Into<String>,
        workflow_ref: impl Into<String>,
        input: Option<Value>,
        trigger_source: impl Into<String>,
    ) -> Self {
        let mut dispatch = Self::for_subject_with_metadata(
            SubjectRef::custom(title, description),
            workflow_ref,
            trigger_source,
            Utc::now(),
        );
        dispatch.input = input;
        dispatch
    }

    pub fn subject_id(&self) -> &str {
        self.subject.id()
    }

    pub fn subject_kind(&self) -> &str {
        self.subject.kind()
    }

    pub fn subject_key(&self) -> String {
        self.subject.subject_key()
    }

    pub fn task_id(&self) -> Option<&str> {
        self.subject.task_id()
    }

    pub fn requirement_id(&self) -> Option<&str> {
        self.subject.requirement_id()
    }

    pub fn schedule_id(&self) -> Option<&str> {
        self.subject.schedule_id()
    }

    pub fn with_input(mut self, input: Option<Value>) -> Self {
        self.input = input;
        self
    }

    pub fn with_vars(mut self, vars: HashMap<String, String>) -> Self {
        self.vars = vars;
        self
    }

    pub fn to_workflow_run_input(&self) -> WorkflowRunInput {
        let mut input = match self.subject.kind() {
            SUBJECT_KIND_TASK => WorkflowRunInput::for_task(self.subject.id.clone(), Some(self.workflow_ref.clone())),
            SUBJECT_KIND_REQUIREMENT => {
                WorkflowRunInput::for_requirement(self.subject.id.clone(), Some(self.workflow_ref.clone()))
            }
            _ => WorkflowRunInput::for_custom(
                self.subject.title.clone().unwrap_or_else(|| self.subject.id.clone()),
                self.subject.description.clone().unwrap_or_default(),
                Some(self.workflow_ref.clone()),
            ),
        };
        input.subject = self.subject.clone();
        input.with_input(self.input.clone()).with_vars(self.vars.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerEvent {
    pub event: String,
    #[serde(default)]
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub workflow_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_status: Option<WorkflowStatus>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectExecutionFact {
    pub subject_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_status: Option<WorkflowStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub runner_events: Vec<RunnerEvent>,
}

impl SubjectExecutionFact {
    pub fn completion_status(&self) -> &str {
        if let Some(status) = self.workflow_status {
            match status {
                WorkflowStatus::Pending => "pending",
                WorkflowStatus::Running => "running",
                WorkflowStatus::Paused => "paused",
                WorkflowStatus::Completed => "completed",
                WorkflowStatus::Failed | WorkflowStatus::Escalated => "failed",
                WorkflowStatus::Cancelled => "cancelled",
            }
        } else if self.success {
            "completed"
        } else {
            "failed"
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunInput {
    #[serde(default = "default_workflow_subject_ref")]
    pub subject: SubjectRef,
    #[serde(default)]
    pub workflow_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "input_json")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vars: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirement_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl WorkflowRunInput {
    pub fn for_subject(subject: SubjectRef, workflow_ref: Option<String>) -> Self {
        let task_id = subject.task_id().unwrap_or_default().to_string();
        let requirement_id = subject.requirement_id().map(ToOwned::to_owned);
        let title = subject.title.clone();
        let description = subject.description.clone();
        Self { subject, task_id, workflow_ref, input: None, vars: HashMap::new(), requirement_id, title, description }
    }

    pub fn for_task(task_id: String, workflow_ref: Option<String>) -> Self {
        Self::for_subject(SubjectRef::task(task_id), workflow_ref)
    }

    pub fn for_requirement(requirement_id: String, workflow_ref: Option<String>) -> Self {
        Self::for_subject(SubjectRef::requirement(requirement_id), workflow_ref)
    }

    pub fn for_custom(title: String, description: String, workflow_ref: Option<String>) -> Self {
        Self::for_subject(SubjectRef::custom(title, description), workflow_ref)
    }

    pub fn subject(&self) -> &SubjectRef {
        &self.subject
    }

    pub fn subject_id(&self) -> &str {
        self.subject.id()
    }

    pub fn subject_kind(&self) -> &str {
        self.subject.kind()
    }

    pub fn workflow_ref(&self) -> Option<&str> {
        self.workflow_ref.as_deref()
    }

    pub fn with_workflow_ref(mut self, workflow_ref: String) -> Self {
        self.workflow_ref = Some(workflow_ref);
        self
    }

    pub fn with_input(mut self, input: Option<Value>) -> Self {
        self.input = input;
        self
    }

    pub fn with_vars(mut self, vars: HashMap<String, String>) -> Self {
        self.vars = vars;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn list_page_request_bounds_clamp_to_available_items() {
        let request = ListPageRequest { limit: Some(5), offset: 8 };
        assert_eq!(request.bounds(10), (8, 10));

        let request = ListPageRequest { limit: None, offset: 3 };
        assert_eq!(request.bounds(10), (3, 10));

        let request = ListPageRequest { limit: Some(5), offset: 20 };
        assert_eq!(request.bounds(10), (10, 10));
    }

    #[test]
    fn list_page_metadata_tracks_next_offset() {
        let page = ListPage::new(vec![1, 2, 3], 10, ListPageRequest { limit: Some(3), offset: 3 });
        assert_eq!(page.returned, 3);
        assert_eq!(page.offset, 3);
        assert!(page.has_more);
        assert_eq!(page.next_offset, Some(6));

        let final_page = ListPage::new(vec![7, 8], 8, ListPageRequest { limit: Some(3), offset: 6 });
        assert_eq!(final_page.returned, 2);
        assert!(!final_page.has_more);
        assert_eq!(final_page.next_offset, None);
    }

    #[test]
    fn query_models_serialize_with_stable_field_names() {
        let requirement_query = RequirementQuery {
            filter: RequirementFilter {
                status: Some(RequirementStatus::InProgress),
                priority: Some(RequirementPriority::Must),
                category: Some("integration".to_string()),
                requirement_type: Some(RequirementType::Technical),
                tags: Some(vec!["graphql".to_string()]),
                linked_task_id: Some("TASK-590".to_string()),
                search_text: Some("query".to_string()),
            },
            page: ListPageRequest { limit: Some(25), offset: 50 },
            sort: RequirementQuerySort::UpdatedAt,
        };

        let workflow_query = WorkflowQuery {
            filter: WorkflowFilter {
                status: Some(WorkflowStatus::Running),
                workflow_ref: Some("standard".to_string()),
                task_id: Some("TASK-590".to_string()),
                phase_id: Some("implementation".to_string()),
                search_text: Some("graphql".to_string()),
            },
            page: ListPageRequest::unbounded(),
            sort: WorkflowQuerySort::Status,
        };

        let requirement_value = serde_json::to_value(&requirement_query).expect("requirement query should serialize");
        assert_eq!(requirement_value["filter"]["status"], "in-progress");
        assert_eq!(requirement_value["filter"]["priority"], "must");
        assert_eq!(requirement_value["filter"]["type"], "technical");
        assert_eq!(requirement_value["sort"], "updated_at");

        let workflow_value = serde_json::to_value(&workflow_query).expect("workflow query should serialize");
        assert_eq!(workflow_value["filter"]["status"], "running");
        assert_eq!(workflow_value["filter"]["workflow_ref"], "standard");
        assert_eq!(workflow_value["sort"], "status");
    }

    #[test]
    fn task_query_defaults_are_canonical() {
        let query = TaskQuery::default();
        assert!(query.filter.search_text.is_none());
        assert_eq!(query.page, ListPageRequest::unbounded());
        assert_eq!(query.sort, TaskQuerySort::Priority);
    }

    #[test]
    fn list_page_handles_zero_total() {
        let page: ListPage<String> = ListPage::new(Vec::new(), 0, ListPageRequest { limit: Some(10), offset: 5 });
        assert_eq!(page.offset, 0);
        assert_eq!(page.returned, 0);
        assert!(!page.has_more);
        assert_eq!(page.next_offset, None);
    }

    #[test]
    fn requirement_items_can_be_embedded_in_paginated_results() {
        let requirement = RequirementItem {
            id: "REQ-001".to_string(),
            title: "Shared query contracts".to_string(),
            description: "Normalize list/filter behavior.".to_string(),
            body: None,
            legacy_id: None,
            category: Some("integration".to_string()),
            requirement_type: Some(RequirementType::Technical),
            acceptance_criteria: vec!["Single source of truth".to_string()],
            priority: RequirementPriority::Must,
            status: RequirementStatus::Draft,
            source: "test".to_string(),
            tags: vec!["query".to_string()],
            links: RequirementLinks::default(),
            comments: Vec::new(),
            relative_path: None,
            linked_task_ids: vec!["TASK-590".to_string()],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let page = ListPage::new(vec![requirement], 1, ListPageRequest::unbounded());
        let serialized = serde_json::to_value(&page).expect("page should serialize");
        assert_eq!(serialized["total"], 1);
        assert_eq!(serialized["items"][0]["status"], "draft");
    }

    #[test]
    fn subject_dispatch_serializes_builtin_subjects_with_legacy_subject_shape() {
        let serialized = serde_json::to_value(SubjectDispatch::for_task("TASK-9", "standard"))
            .expect("task dispatch should serialize");

        assert_eq!(serialized["subject"]["Task"]["id"], "TASK-9");
    }

    #[test]
    fn generic_subject_dispatch_uses_kind_qualified_identity_and_custom_adapter() {
        let dispatch = SubjectDispatch::for_subject_with_metadata(
            SubjectRef::new("pack.review", "REV-1"),
            "review",
            "manual",
            Utc.with_ymd_and_hms(2026, 3, 10, 9, 30, 0).unwrap(),
        );

        let serialized = serde_json::to_value(&dispatch).expect("generic dispatch should serialize");
        assert_eq!(serialized["subject"]["kind"], "pack.review");
        assert_eq!(serialized["subject"]["id"], "REV-1");
        assert_eq!(dispatch.subject_kind(), "pack.review");
        assert_eq!(dispatch.subject_key(), "pack.review::REV-1");
        assert_eq!(dispatch.to_workflow_run_input().subject(), &SubjectRef::new("pack.review", "REV-1"));
    }

    #[test]
    fn subject_dispatch_deserializes_legacy_subjects_into_builtin_subject_refs() {
        let dispatch: SubjectDispatch = serde_json::from_value(serde_json::json!({
            "subject": {
                "Requirement": {
                    "id": "REQ-39"
                }
            },
            "workflow_ref": "planning",
            "trigger_source": "manual",
            "requested_at": "2026-03-10T09:30:00Z"
        }))
        .expect("legacy requirement dispatch should deserialize");

        assert_eq!(dispatch.subject_kind(), SUBJECT_KIND_REQUIREMENT);
        assert_eq!(dispatch.subject_id(), "REQ-39");
        assert_eq!(dispatch.subject_key(), "REQ-39");
    }
}
