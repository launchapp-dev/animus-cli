use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
        Self {
            include_codebase_scan: true,
            append_only: true,
            max_requirements: default_requirements_limit(),
        }
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
    pub pipeline_id: Option<String>,
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
        Self {
            max_cpu_percent: None,
            max_memory_mb: None,
            requires_network: true,
        }
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

const FRONTEND_TAGS: &[&str] = &[
    "frontend",
    "ui",
    "ux",
    "design",
    "react",
    "web",
    "landing-page",
    "design-system",
    "nextjs",
];

const FRONTEND_TOKENS: &[&str] = &[
    "frontend",
    "ui",
    "ux",
    "react",
    "tailwind",
    "css",
    "component",
    "storybook",
    "wireframe",
    "mockup",
    "nextjs",
];

const FRONTEND_PHRASES: &[&str] = &[
    "user interface",
    "user experience",
    "design system",
    "landing page",
];

pub fn is_frontend_related_content(tags: &[String], text: &str) -> bool {
    if tags.iter().any(|tag| {
        let normalized = tag.trim().to_ascii_lowercase();
        FRONTEND_TAGS.contains(&normalized.as_str())
    }) {
        return true;
    }

    let haystack = text.to_ascii_lowercase();
    let tokenized: String = haystack
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect();
    let tokens: HashSet<&str> = tokenized.split_whitespace().collect();

    if FRONTEND_TOKENS.iter().any(|needle| tokens.contains(needle)) {
        return true;
    }

    FRONTEND_PHRASES
        .iter()
        .any(|needle| haystack.contains(needle))
}

impl OrchestratorTask {
    pub fn is_frontend_related(&self) -> bool {
        if self.workflow_metadata.requires_design {
            return true;
        }

        if self
            .impact_area
            .iter()
            .any(|area| matches!(area, ImpactArea::Frontend))
        {
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    #[serde(default)]
    pub max_agents: Option<usize>,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub daemon_pid: Option<u32>,
    #[serde(default)]
    pub process_alive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_size: Option<u32>,
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
            allowed_models: crate::default_model_specs()
                .into_iter()
                .map(|(model_id, _tool)| model_id)
                .collect(),
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
        Self {
            max_workflows: 3,
            max_agents: 10,
        }
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
    pub pipeline_id: Option<String>,
    #[serde(default = "default_workflow_subject")]
    pub subject: WorkflowSubject,
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

fn default_workflow_subject() -> WorkflowSubject {
    WorkflowSubject::Task { id: String::new() }
}

impl OrchestratorWorkflow {
    pub fn sync_status(&mut self) {
        self.status = self.machine_state.to_workflow_status();
    }

    pub fn backfill_subject(&mut self) {
        if matches!(&self.subject, WorkflowSubject::Task { id } if id.is_empty())
            && !self.task_id.is_empty()
        {
            self.subject = WorkflowSubject::Task {
                id: self.task_id.clone(),
            };
        }
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

    pub fn subject_type(&self) -> &str {
        match self {
            Self::Task { .. } => "task",
            Self::Requirement { .. } => "requirement",
            Self::Custom { .. } => "custom",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubjectDispatch {
    pub subject: WorkflowSubject,
    pub pipeline_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    pub trigger_source: String,
    pub requested_at: DateTime<Utc>,
}

impl SubjectDispatch {
    pub fn for_task(task_id: impl Into<String>, pipeline_id: impl Into<String>) -> Self {
        Self::for_task_with_metadata(task_id, pipeline_id, "ready-queue", Utc::now())
    }

    pub fn for_task_with_metadata(
        task_id: impl Into<String>,
        pipeline_id: impl Into<String>,
        trigger_source: impl Into<String>,
        requested_at: DateTime<Utc>,
    ) -> Self {
        Self {
            subject: WorkflowSubject::Task { id: task_id.into() },
            pipeline_id: pipeline_id.into(),
            input: None,
            priority: None,
            trigger_source: trigger_source.into(),
            requested_at,
        }
    }

    pub fn for_requirement(
        requirement_id: impl Into<String>,
        pipeline_id: impl Into<String>,
        trigger_source: impl Into<String>,
    ) -> Self {
        Self {
            subject: WorkflowSubject::Requirement {
                id: requirement_id.into(),
            },
            pipeline_id: pipeline_id.into(),
            input: None,
            priority: None,
            trigger_source: trigger_source.into(),
            requested_at: Utc::now(),
        }
    }

    pub fn for_custom(
        title: impl Into<String>,
        description: impl Into<String>,
        pipeline_id: impl Into<String>,
        input: Option<Value>,
        trigger_source: impl Into<String>,
    ) -> Self {
        Self {
            subject: WorkflowSubject::Custom {
                title: title.into(),
                description: description.into(),
            },
            pipeline_id: pipeline_id.into(),
            input,
            priority: None,
            trigger_source: trigger_source.into(),
            requested_at: Utc::now(),
        }
    }

    pub fn subject_id(&self) -> &str {
        self.subject.id()
    }

    pub fn task_id(&self) -> Option<&str> {
        match &self.subject {
            WorkflowSubject::Task { id } => Some(id),
            _ => None,
        }
    }

    pub fn schedule_id(&self) -> Option<&str> {
        match &self.subject {
            WorkflowSubject::Custom { title, .. } => title.strip_prefix("schedule:"),
            _ => None,
        }
    }

    pub fn with_input(mut self, input: Option<Value>) -> Self {
        self.input = input;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunInput {
    pub subject: WorkflowSubject,
    #[serde(default)]
    pub pipeline_id: Option<String>,
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
    pub fn for_task(task_id: String, pipeline_id: Option<String>) -> Self {
        Self {
            subject: WorkflowSubject::Task {
                id: task_id.clone(),
            },
            task_id,
            pipeline_id,
            requirement_id: None,
            title: None,
            description: None,
        }
    }

    pub fn for_requirement(requirement_id: String, pipeline_id: Option<String>) -> Self {
        Self {
            subject: WorkflowSubject::Requirement {
                id: requirement_id.clone(),
            },
            task_id: String::new(),
            pipeline_id,
            requirement_id: Some(requirement_id),
            title: None,
            description: None,
        }
    }

    pub fn for_custom(title: String, description: String, pipeline_id: Option<String>) -> Self {
        Self {
            subject: WorkflowSubject::Custom {
                title: title.clone(),
                description: description.clone(),
            },
            task_id: String::new(),
            pipeline_id,
            requirement_id: None,
            title: Some(title),
            description: Some(description),
        }
    }

    pub fn subject(&self) -> &WorkflowSubject {
        &self.subject
    }

    pub fn subject_id(&self) -> &str {
        self.subject.id()
    }

    pub fn with_pipeline(mut self, pipeline_id: String) -> Self {
        self.pipeline_id = Some(pipeline_id);
        self
    }
}
