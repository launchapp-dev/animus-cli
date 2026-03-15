use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
pub use orchestrator_store::{project_state_dir, read_json_or_default, write_json_atomic, write_json_pretty};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEntityType {
    Requirement,
    Task,
    WorkflowPhase,
    Mockup,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerRole {
    Po,
    Em,
    Qa,
    Architect,
    Agent,
    Human,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approve,
    Reject,
    RequestChanges,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub id: String,
    pub entity_type: ReviewEntityType,
    pub entity_id: String,
    pub reviewer_role: ReviewerRole,
    pub decision: ReviewDecision,
    pub source: String,
    pub rationale: String,
    #[serde(default)]
    pub content_hash: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReviewStore {
    #[serde(default)]
    pub reviews: Vec<ReviewRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityReviewStatus {
    pub entity_type: String,
    pub entity_id: String,
    pub po_approved: bool,
    pub em_approved: bool,
    pub dual_approved: bool,
    pub decisions: Vec<ReviewRecord>,
}

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

pub fn reviews_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("reviews.json")
}

pub fn handoffs_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("handoffs.json")
}

pub fn load_reviews(project_root: &str) -> Result<ReviewStore> {
    read_json_or_default(&reviews_path(project_root))
}

pub fn save_reviews(project_root: &str, store: &ReviewStore) -> Result<()> {
    write_json_pretty(&reviews_path(project_root), store)
}

pub fn load_handoffs(project_root: &str) -> Result<HandoffStore> {
    read_json_or_default(&handoffs_path(project_root))
}

pub fn save_handoffs(project_root: &str, store: &HandoffStore) -> Result<()> {
    write_json_pretty(&handoffs_path(project_root), store)
}

pub fn parse_review_entity_type(value: &str) -> Result<ReviewEntityType> {
    let parsed = match value.trim().to_ascii_lowercase().as_str() {
        "requirement" => ReviewEntityType::Requirement,
        "task" => ReviewEntityType::Task,
        "workflow-phase" | "workflow_phase" => ReviewEntityType::WorkflowPhase,
        "mockup" => ReviewEntityType::Mockup,
        _ => anyhow::bail!("unsupported entity_type: {value}"),
    };
    Ok(parsed)
}

pub fn parse_reviewer_role(value: &str) -> Result<ReviewerRole> {
    let parsed = match value.trim().to_ascii_lowercase().as_str() {
        "po" => ReviewerRole::Po,
        "em" => ReviewerRole::Em,
        "qa" => ReviewerRole::Qa,
        "architect" => ReviewerRole::Architect,
        "agent" => ReviewerRole::Agent,
        "human" => ReviewerRole::Human,
        _ => anyhow::bail!("unsupported reviewer_role: {value}"),
    };
    Ok(parsed)
}

pub fn parse_review_decision(value: &str) -> Result<ReviewDecision> {
    let parsed = match value.trim().to_ascii_lowercase().as_str() {
        "approve" => ReviewDecision::Approve,
        "reject" => ReviewDecision::Reject,
        "request_changes" | "request-changes" | "changes" => ReviewDecision::RequestChanges,
        _ => anyhow::bail!("unsupported decision: {value}"),
    };
    Ok(parsed)
}

pub fn compute_entity_review_status(
    store: &ReviewStore,
    entity_type: ReviewEntityType,
    entity_id: &str,
) -> EntityReviewStatus {
    let mut decisions: Vec<ReviewRecord> = store
        .reviews
        .iter()
        .filter(|review| review.entity_type == entity_type && review.entity_id == entity_id)
        .cloned()
        .collect();
    decisions.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    let latest_po = decisions.iter().rev().find(|review| review.reviewer_role == ReviewerRole::Po);
    let latest_em = decisions.iter().rev().find(|review| review.reviewer_role == ReviewerRole::Em);
    let po_approved = latest_po.map(|review| review.decision == ReviewDecision::Approve).unwrap_or(false);
    let em_approved = latest_em.map(|review| review.decision == ReviewDecision::Approve).unwrap_or(false);

    EntityReviewStatus {
        entity_type: serde_json::to_string(&entity_type)
            .unwrap_or_else(|_| "\"unknown\"".to_string())
            .trim_matches('"')
            .to_string(),
        entity_id: entity_id.to_string(),
        po_approved,
        em_approved,
        dual_approved: po_approved && em_approved,
        decisions,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaGateResultRecord {
    pub gate_id: String,
    pub passed: bool,
    pub reason: String,
    #[serde(default)]
    pub gate_type: Option<String>,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub actual_value: Option<Value>,
    #[serde(default)]
    pub threshold: Option<Value>,
    #[serde(default)]
    pub blocking: Option<bool>,
    #[serde(default)]
    pub evaluated_at: Option<String>,
    #[serde(default)]
    pub confidence_score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaPhaseGateResult {
    pub workflow_id: String,
    pub phase_id: String,
    pub task_id: String,
    pub worktree_path: String,
    pub passed: bool,
    pub gate_results: Vec<QaGateResultRecord>,
    pub metrics: BTreeMap<String, Value>,
    pub metadata: BTreeMap<String, Value>,
    pub evaluated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QaResultsStore {
    #[serde(default)]
    pub results: Vec<QaPhaseGateResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaReviewApprovalRecord {
    pub workflow_id: String,
    pub phase_id: String,
    pub gate_id: String,
    pub approved_by: String,
    pub approved_at: String,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QaReviewApprovalStore {
    #[serde(default)]
    pub approvals: Vec<QaReviewApprovalRecord>,
}

pub fn qa_results_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("qa-results.json")
}

pub fn qa_approvals_path(project_root: &str) -> PathBuf {
    project_state_dir(project_root).join("qa-review-approvals.json")
}

pub fn load_qa_results(project_root: &str) -> Result<QaResultsStore> {
    read_json_or_default(&qa_results_path(project_root))
}

pub fn save_qa_results(project_root: &str, store: &QaResultsStore) -> Result<()> {
    write_json_pretty(&qa_results_path(project_root), store)
}

pub fn load_qa_approvals(project_root: &str) -> Result<QaReviewApprovalStore> {
    read_json_or_default(&qa_approvals_path(project_root))
}

pub fn save_qa_approvals(project_root: &str, store: &QaReviewApprovalStore) -> Result<()> {
    write_json_pretty(&qa_approvals_path(project_root), store)
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
