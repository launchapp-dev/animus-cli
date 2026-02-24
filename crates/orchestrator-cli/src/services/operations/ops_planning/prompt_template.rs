use orchestrator_core::VisionDocument;

use super::types::VisionRefineInputPayload;

const VISION_REFINEMENT_PROMPT_TEMPLATE: &str =
    include_str!("../../../../prompts/ops_planning/vision_refine.prompt");

pub(super) fn build_vision_refinement_prompt(
    vision: &VisionDocument,
    input: &VisionRefineInputPayload,
) -> String {
    let focus_line = input
        .focus
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("Refinement focus: {value}"))
        .unwrap_or_else(|| {
            "Refinement focus: improve strategic clarity and delivery readiness.".to_string()
        });
    let vision_json = serde_json::to_string_pretty(vision).unwrap_or_else(|_| "{}".to_string());

    VISION_REFINEMENT_PROMPT_TEMPLATE
        .replace("{focus_line}", &focus_line)
        .replace("{vision_json}", &vision_json)
}
