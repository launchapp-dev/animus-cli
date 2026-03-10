use orchestrator_core::{
    CodebaseInsight, ComplexityAssessment, RequirementLinks, RequirementPriority,
    RequirementStatus, RequirementType, VisionDocument,
};
use protocol::default_model_for_tool;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct VisionDraftInputPayload {
    pub(crate) project_name: Option<String>,
    pub(crate) problem_statement: String,
    pub(crate) target_users: Vec<String>,
    pub(crate) goals: Vec<String>,
    pub(crate) constraints: Vec<String>,
    pub(crate) value_proposition: Option<String>,
    pub(crate) use_ai_complexity: bool,
    pub(crate) tool: String,
    pub(crate) model: String,
    pub(crate) timeout_secs: Option<u64>,
    pub(crate) start_runner: bool,
    pub(crate) allow_heuristic_fallback: bool,
}

impl Default for VisionDraftInputPayload {
    fn default() -> Self {
        Self {
            project_name: None,
            problem_statement: String::new(),
            target_users: Vec::new(),
            goals: Vec::new(),
            constraints: Vec::new(),
            value_proposition: None,
            use_ai_complexity: true,
            tool: "codex".to_string(),
            model: default_model_for_tool("codex")
                .expect("default model for codex should be configured")
                .to_string(),
            timeout_secs: Some(1200),
            start_runner: true,
            allow_heuristic_fallback: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct VisionRefineInputPayload {
    pub(crate) focus: Option<String>,
    pub(crate) use_ai: bool,
    pub(crate) tool: String,
    pub(crate) model: String,
    pub(crate) timeout_secs: Option<u64>,
    pub(crate) start_runner: bool,
    pub(crate) allow_heuristic_fallback: bool,
    pub(crate) preserve_core: bool,
}

impl Default for VisionRefineInputPayload {
    fn default() -> Self {
        Self {
            focus: None,
            use_ai: true,
            tool: "codex".to_string(),
            model: default_model_for_tool("codex")
                .expect("default model for codex should be configured")
                .to_string(),
            timeout_secs: Some(1200),
            start_runner: true,
            allow_heuristic_fallback: false,
            preserve_core: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct VisionRefinementProposal {
    #[serde(
        alias = "problem_statement",
        alias = "problem",
        alias = "problem_refinement"
    )]
    pub(super) problem_statement_refinement: Option<String>,
    #[serde(alias = "target_users", alias = "target_user_additions")]
    pub(super) target_users_additions: Vec<String>,
    #[serde(alias = "goals", alias = "goal_additions")]
    pub(super) goals_additions: Vec<String>,
    #[serde(alias = "constraints", alias = "constraint_additions")]
    pub(super) constraints_additions: Vec<String>,
    #[serde(alias = "value_proposition", alias = "value_proposition_refined")]
    pub(super) value_proposition_refinement: Option<String>,
    pub(super) rationale: Option<String>,
    #[serde(alias = "complexity", alias = "complexity_assessment")]
    pub(super) complexity_assessment: Option<ComplexityAssessmentProposal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct ComplexityAssessmentProposal {
    pub(super) tier: Option<String>,
    pub(super) confidence: Option<f32>,
    pub(super) rationale: Option<String>,
    pub(super) recommended_requirement_range: Option<ComplexityRangeProposal>,
    pub(super) task_density: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct ComplexityRangeProposal {
    pub(super) min: Option<usize>,
    pub(super) max: Option<usize>,
}

impl VisionRefinementProposal {
    pub(super) fn has_any_content(&self) -> bool {
        self.problem_statement_refinement
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
            || self
                .value_proposition_refinement
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty())
            || !self.target_users_additions.is_empty()
            || !self.goals_additions.is_empty()
            || !self.constraints_additions.is_empty()
            || self
                .rationale
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty())
            || self.complexity_assessment.is_some()
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct VisionRefinementChanges {
    pub(super) target_users_added: usize,
    pub(super) goals_added: usize,
    pub(super) constraints_added: usize,
    pub(super) problem_statement_enriched: bool,
    pub(super) value_proposition_changed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct VisionRefinementMeta {
    pub(super) mode: String,
    pub(super) focus: Option<String>,
    pub(super) tool: Option<String>,
    pub(super) model: Option<String>,
    pub(super) rationale: Option<String>,
    pub(super) fallback_reason: Option<String>,
    pub(super) complexity_assessment: ComplexityAssessment,
    pub(super) changes: VisionRefinementChanges,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct VisionRefineResultOutput {
    pub(super) updated_vision: VisionDocument,
    pub(super) refinement: VisionRefinementMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RequirementsDraftInputPayload {
    pub(crate) include_codebase_scan: bool,
    pub(crate) append_only: bool,
    pub(crate) max_requirements: usize,
    pub(crate) draft_strategy: String,
    pub(crate) po_parallelism: usize,
    pub(crate) quality_repair_attempts: usize,
    pub(crate) allow_heuristic_complexity: bool,
    pub(crate) tool: String,
    pub(crate) model: String,
    pub(crate) timeout_secs: Option<u64>,
    pub(crate) start_runner: bool,
}

impl Default for RequirementsDraftInputPayload {
    fn default() -> Self {
        Self {
            include_codebase_scan: true,
            append_only: true,
            max_requirements: 0,
            draft_strategy: "single-agent".to_string(),
            po_parallelism: 1,
            quality_repair_attempts: 2,
            allow_heuristic_complexity: false,
            tool: "codex".to_string(),
            model: default_model_for_tool("codex")
                .expect("default model for codex should be configured")
                .to_string(),
            timeout_secs: Some(1800),
            start_runner: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RequirementsRefineInputPayload {
    pub(crate) requirement_ids: Vec<String>,
    pub(crate) focus: Option<String>,
    pub(crate) use_ai: bool,
    pub(crate) tool: String,
    pub(crate) model: String,
    pub(crate) timeout_secs: Option<u64>,
    pub(crate) start_runner: bool,
}

impl Default for RequirementsRefineInputPayload {
    fn default() -> Self {
        Self {
            requirement_ids: Vec::new(),
            focus: None,
            use_ai: true,
            tool: "codex".to_string(),
            model: default_model_for_tool("codex")
                .expect("default model for codex should be configured")
                .to_string(),
            timeout_secs: Some(1200),
            start_runner: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct RequirementsDraftProposal {
    pub(super) requirements: Vec<RequirementDraftCandidate>,
    pub(super) rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct RequirementDraftCandidate {
    pub(super) id: Option<String>,
    pub(super) title: String,
    pub(super) description: String,
    pub(super) category: Option<String>,
    #[serde(rename = "type")]
    pub(super) requirement_type: Option<String>,
    pub(super) priority: Option<String>,
    pub(super) acceptance_criteria: Vec<String>,
    pub(super) tags: Vec<String>,
    pub(super) source: Option<String>,
}

impl RequirementDraftCandidate {
    pub(super) fn is_empty(&self) -> bool {
        self.title.trim().is_empty() && self.description.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(crate) struct RequirementsDraftResultOutput {
    pub(crate) requirements: Vec<orchestrator_core::RequirementItem>,
    pub(crate) appended_count: usize,
    pub(crate) codebase_insight: Option<CodebaseInsight>,
    pub(crate) mode: String,
    pub(crate) draft_strategy: String,
    pub(crate) rationale: Option<String>,
    pub(crate) perspective_summary: PerspectiveSummary,
    pub(crate) quality_issues_detected: usize,
    pub(crate) quality_repair_rounds: usize,
    pub(crate) tool: String,
    pub(crate) model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub(crate) struct PerspectiveSummary {
    pub(crate) product: usize,
    pub(crate) engineering: usize,
    pub(crate) ops: usize,
    pub(crate) unknown: usize,
}

pub(super) fn parse_requirement_type(value: Option<&str>) -> Option<RequirementType> {
    let normalized = value?.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "product" => Some(RequirementType::Product),
        "functional" => Some(RequirementType::Functional),
        "non-functional" | "nonfunctional" | "non_functional" => {
            Some(RequirementType::NonFunctional)
        }
        "technical" => Some(RequirementType::Technical),
        "other" => Some(RequirementType::Other),
        _ => None,
    }
}

pub(super) fn parse_requirement_priority(value: Option<&str>, index: usize) -> RequirementPriority {
    if let Some(raw) = value {
        match raw.trim().to_ascii_lowercase().as_str() {
            "must" => return RequirementPriority::Must,
            "should" => return RequirementPriority::Should,
            "could" => return RequirementPriority::Could,
            "wont" | "won't" | "wont-have" => return RequirementPriority::Wont,
            _ => {}
        }
    }

    match index {
        0 => RequirementPriority::Must,
        1 | 2 => RequirementPriority::Should,
        _ => RequirementPriority::Could,
    }
}

pub(super) fn default_requirement_status() -> RequirementStatus {
    RequirementStatus::Draft
}

pub(super) fn default_requirement_links() -> RequirementLinks {
    RequirementLinks::default()
}
