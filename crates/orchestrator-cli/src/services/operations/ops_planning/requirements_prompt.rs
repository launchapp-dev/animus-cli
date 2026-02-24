use orchestrator_core::VisionDocument;

use super::types::{
    RequirementDraftCandidate, RequirementsDraftInputPayload, RequirementsRefineInputPayload,
};

const REQUIREMENTS_DRAFT_PROMPT_TEMPLATE: &str =
    include_str!("../../../../prompts/ops_planning/requirements_draft.prompt");
const REQUIREMENT_PO_DRAFT_PROMPT_TEMPLATE: &str =
    include_str!("../../../../prompts/ops_planning/requirements_po_draft.prompt");
const REQUIREMENTS_REPAIR_PROMPT_TEMPLATE: &str =
    include_str!("../../../../prompts/ops_planning/requirements_repair.prompt");
const REQUIREMENTS_REFINE_PROMPT_TEMPLATE: &str =
    include_str!("../../../../prompts/ops_planning/requirements_refine.prompt");

pub(super) fn build_requirements_draft_prompt(
    vision: &VisionDocument,
    existing_requirements_json: &str,
    input: &RequirementsDraftInputPayload,
) -> String {
    let target_requirements_line = if input.max_requirements > 0 {
        format!("Target requirement count: {}", input.max_requirements)
    } else {
        "Target requirement count: infer from complexity and vision scope.".to_string()
    };
    let append_mode_line = if input.append_only {
        "Append mode: true (do not duplicate semantically equivalent requirements).".to_string()
    } else {
        "Append mode: false (you may replace current draft set with a better scoped set)."
            .to_string()
    };
    let codebase_scan_line = if input.include_codebase_scan {
        "Codebase scan expectation: inspect relevant files before drafting if repository exists."
            .to_string()
    } else {
        "Codebase scan expectation: optional; prioritize vision-driven drafting.".to_string()
    };

    let vision_json = serde_json::to_string_pretty(vision).unwrap_or_else(|_| "{}".to_string());

    REQUIREMENTS_DRAFT_PROMPT_TEMPLATE
        .replace("{target_requirements_line}", &target_requirements_line)
        .replace("{append_mode_line}", &append_mode_line)
        .replace("{codebase_scan_line}", &codebase_scan_line)
        .replace("{vision_json}", &vision_json)
        .replace("{existing_requirements_json}", existing_requirements_json)
}

pub(super) fn build_requirements_repair_prompt(
    vision: &VisionDocument,
    candidates_json: &str,
    quality_issues_json: &str,
    input: &RequirementsDraftInputPayload,
    attempt: usize,
) -> String {
    let target_requirements_line = if input.max_requirements > 0 {
        format!("Target requirement count: {}", input.max_requirements)
    } else {
        "Target requirement count: infer from complexity and vision scope.".to_string()
    };
    let repair_attempt_line = format!("Repair attempt: {attempt}");
    let vision_json = serde_json::to_string_pretty(vision).unwrap_or_else(|_| "{}".to_string());
    REQUIREMENTS_REPAIR_PROMPT_TEMPLATE
        .replace("{target_requirements_line}", &target_requirements_line)
        .replace("{repair_attempt_line}", &repair_attempt_line)
        .replace("{vision_json}", &vision_json)
        .replace("{candidate_requirements_json}", candidates_json)
        .replace("{quality_issues_json}", quality_issues_json)
}

pub(super) fn build_requirement_po_draft_prompt(
    vision: &VisionDocument,
    seed: &RequirementDraftCandidate,
    index: usize,
    total: usize,
) -> String {
    let vision_json = serde_json::to_string_pretty(vision).unwrap_or_else(|_| "{}".to_string());
    let seed_json = serde_json::to_string_pretty(seed).unwrap_or_else(|_| "{}".to_string());
    REQUIREMENT_PO_DRAFT_PROMPT_TEMPLATE
        .replace("{requirement_index}", &(index + 1).to_string())
        .replace("{requirement_total}", &total.to_string())
        .replace("{vision_json}", &vision_json)
        .replace("{seed_requirement_json}", &seed_json)
}

pub(super) fn build_requirements_refine_prompt(
    vision: &VisionDocument,
    selected_requirements_json: &str,
    input: &RequirementsRefineInputPayload,
) -> String {
    let focus_line = input
        .focus
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|focus| format!("Refinement focus: {focus}"))
        .unwrap_or_else(|| {
            "Refinement focus: improve clarity, remove overlap, and tighten acceptance criteria."
                .to_string()
        });
    let vision_json = serde_json::to_string_pretty(vision).unwrap_or_else(|_| "{}".to_string());
    REQUIREMENTS_REFINE_PROMPT_TEMPLATE
        .replace("{focus_line}", &focus_line)
        .replace("{vision_json}", &vision_json)
        .replace("{selected_requirements_json}", selected_requirements_json)
}
