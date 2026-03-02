// TODO(TASK-058): Sentinel marker for phase decision contract prompt injection verification; remove after validation.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

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
}

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

/// Task urgency used for task ordering and scheduling (`critical|high|medium|low`).
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
            allowed_models: protocol::default_model_specs()
                .into_iter()
                .map(|(model_id, _tool)| model_id)
                .collect(),
            default_model: protocol::default_model_for_tool("claude").map(str::to_string),
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
    pub custom: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ComplexityTier {
    Simple,
    #[default]
    Medium,
    Complex,
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

/// Requirement-level MoSCoW priority (`must|should|could|wont`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RequirementPriority {
    Must,
    #[default]
    Should,
    Could,
    Wont,
}

impl RequirementPriority {
    #[must_use]
    pub const fn to_task_priority(self) -> Priority {
        match self {
            Self::Must => Priority::High,
            Self::Should => Priority::Medium,
            Self::Could | Self::Wont => Priority::Low,
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequirementType {
    Product,
    Functional,
    NonFunctional,
    Technical,
    Other,
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

/// A single workflow phase decision captured during transition evaluation.
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

/// A phase-level decision emitted by a workflow phase contract.
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

fn default_timestamp_now() -> DateTime<Utc> {
    Utc::now()
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
    #[serde(default)]
    pub resource_requirements: ResourceRequirements,
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

        if self.tags.iter().any(|tag| {
            matches!(
                tag.trim().to_ascii_lowercase().as_str(),
                "frontend"
                    | "ui"
                    | "ux"
                    | "design"
                    | "react"
                    | "web"
                    | "landing-page"
                    | "design-system"
            )
        }) {
            return true;
        }

        let haystack = format!("{} {}", self.title, self.description).to_ascii_lowercase();
        let tokenized: String = haystack
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
            .collect();
        let tokens: std::collections::HashSet<&str> = tokenized.split_whitespace().collect();

        if [
            "frontend",
            "ui",
            "ux",
            "react",
            "tailwind",
            "css",
            "component",
            "storybook",
        ]
        .iter()
        .any(|needle| tokens.contains(*needle))
        {
            return true;
        }

        [
            "user interface",
            "user experience",
            "design system",
            "landing page",
        ]
        .iter()
        .any(|needle| haystack.contains(needle))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorWorkflow {
    pub id: String,
    pub task_id: String,
    pub pipeline_id: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunInput {
    pub task_id: String,
    #[serde(default)]
    pub pipeline_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        PhaseDecision, PhaseDecisionVerdict, PhaseEvidence, PhaseEvidenceKind, Priority,
        RequirementPriority, TaskStatus, TaskType, WorkflowDecisionRisk,
    };
    use serde_json::json;

    #[test]
    fn requirement_priority_to_task_priority_mapping_is_stable() {
        assert_eq!(RequirementPriority::Must.to_task_priority(), Priority::High);
        assert_eq!(
            RequirementPriority::Should.to_task_priority(),
            Priority::Medium
        );
        assert_eq!(RequirementPriority::Could.to_task_priority(), Priority::Low);
        assert_eq!(RequirementPriority::Wont.to_task_priority(), Priority::Low);
    }

    #[test]
    fn phase_decision_deserializes_with_expected_defaults() {
        let input = json!({
            "kind": "phase_decision",
            "phase_id": "testing",
            "verdict": "advance",
            "confidence": 0.96,
            "risk": "low"
        });

        let parsed: PhaseDecision = serde_json::from_value(input)
            .expect("phase decision should parse with optional fields omitted");

        assert_eq!(parsed.kind, "phase_decision");
        assert_eq!(parsed.phase_id, "testing");
        assert_eq!(parsed.verdict, PhaseDecisionVerdict::Advance);
        assert_eq!(parsed.confidence, 0.96);
        assert_eq!(parsed.risk, WorkflowDecisionRisk::Low);
        assert!(parsed.reason.is_empty());
        assert!(parsed.evidence.is_empty());
        assert!(parsed.guardrail_violations.is_empty());
        assert!(parsed.commit_message.is_none());
    }

    #[test]
    fn phase_decision_deserializes_unknown_verdict_with_fallback() {
        let input = json!({
            "kind": "phase_decision",
            "phase_id": "code-review",
            "verdict": "escalate",
            "confidence": 0.51,
            "risk": "medium"
        });

        let parsed: PhaseDecision =
            serde_json::from_value(input).expect("unknown verdict should map to fallback variant");

        assert_eq!(parsed.verdict, PhaseDecisionVerdict::Unknown);
        assert_eq!(parsed.phase_id, "code-review");
        assert_eq!(parsed.risk, WorkflowDecisionRisk::Medium);
    }

    #[test]
    fn phase_decision_serializes_with_evidence_payload() {
        let decision = PhaseDecision {
            kind: "phase_decision".to_string(),
            phase_id: "testing".to_string(),
            verdict: PhaseDecisionVerdict::Advance,
            confidence: 0.99,
            risk: WorkflowDecisionRisk::Low,
            reason: "All required checks passed".to_string(),
            evidence: vec![PhaseEvidence {
                kind: PhaseEvidenceKind::TestsPassed,
                description: "cargo test -p orchestrator-core".to_string(),
                file_path: Some("crates/orchestrator-core/src/types.rs".to_string()),
                value: Some(json!({ "tests": 2 })),
            }],
            guardrail_violations: vec![],
            commit_message: Some("test: validate phase decision contract".to_string()),
            target_phase: None,
        };

        let serialized =
            serde_json::to_value(&decision).expect("phase decision should serialize successfully");

        assert_eq!(serialized["kind"], "phase_decision");
        assert_eq!(serialized["verdict"], "advance");
        assert_eq!(serialized["risk"], "low");
        assert_eq!(serialized["evidence"][0]["kind"], "tests_passed");
        assert_eq!(
            serialized["evidence"][0]["description"],
            "cargo test -p orchestrator-core"
        );
        assert_eq!(serialized["evidence"][0]["value"]["tests"], 2);
        assert_eq!(
            serialized["commit_message"],
            "test: validate phase decision contract"
        );
    }

    #[test]
    fn task_status_deserializes_contract_aliases_and_helpers_stay_consistent() {
        let backlog_alias: TaskStatus =
            serde_json::from_str("\"todo\"").expect("todo should map to backlog");
        let in_progress_kebab: TaskStatus =
            serde_json::from_str("\"in-progress\"").expect("kebab case should parse");
        let in_progress_snake: TaskStatus =
            serde_json::from_str("\"in_progress\"").expect("snake case should parse");
        let on_hold_snake: TaskStatus =
            serde_json::from_str("\"on_hold\"").expect("on_hold alias should parse");
        let done_alias: TaskStatus =
            serde_json::from_str("\"completed\"").expect("completed should map to done");

        assert_eq!(backlog_alias, TaskStatus::Backlog);
        assert_eq!(in_progress_kebab, TaskStatus::InProgress);
        assert_eq!(in_progress_snake, TaskStatus::InProgress);
        assert_eq!(on_hold_snake, TaskStatus::OnHold);
        assert_eq!(done_alias, TaskStatus::Done);

        assert!(TaskStatus::InProgress.is_active());
        assert!(TaskStatus::Done.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(TaskStatus::Blocked.is_blocked());
        assert!(TaskStatus::OnHold.is_blocked());
    }

    #[test]
    fn task_type_as_str_matches_canonical_serialization_and_aliases() {
        let variants = [
            TaskType::Feature,
            TaskType::Bugfix,
            TaskType::Hotfix,
            TaskType::Refactor,
            TaskType::Docs,
            TaskType::Test,
            TaskType::Chore,
            TaskType::Experiment,
        ];

        for task_type in variants {
            let serialized = serde_json::to_string(&task_type)
                .expect("task type should serialize to canonical string");
            assert_eq!(serialized, format!("\"{}\"", task_type.as_str()));
        }

        let bug_alias: TaskType = serde_json::from_str("\"bug\"").expect("bug alias should parse");
        let docs_alias: TaskType =
            serde_json::from_str("\"documentation\"").expect("documentation alias should parse");
        let tests_alias: TaskType =
            serde_json::from_str("\"tests\"").expect("tests alias should parse");

        assert_eq!(bug_alias, TaskType::Bugfix);
        assert_eq!(docs_alias, TaskType::Docs);
        assert_eq!(tests_alias, TaskType::Test);
    }
}
