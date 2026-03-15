use orchestrator_core::VisionDraftInput;
use serde_json::{json, Value};

use super::{
    parsing::{normalize_optional_string, normalize_vision_input, parse_json_body, refine_vision_heuristically},
    requests::VisionRefineRequest,
    WebApiError, WebApiService,
};

impl WebApiService {
    pub async fn vision_get(&self) -> Result<Value, WebApiError> {
        Ok(json!(self.context.hub.planning().get_vision().await?))
    }

    pub async fn vision_save(&self, body: Value) -> Result<Value, WebApiError> {
        let mut input: VisionDraftInput = parse_json_body(body)?;
        normalize_vision_input(&mut input);

        let vision = self.context.hub.planning().draft_vision(input).await?;
        self.publish_event("vision-save", json!({ "vision_id": vision.id }));
        Ok(json!(vision))
    }

    pub async fn vision_refine(&self, body: Value) -> Result<Value, WebApiError> {
        let request: VisionRefineRequest = parse_json_body(body)?;
        let focus = normalize_optional_string(request.focus);
        let planning = self.context.hub.planning();

        let Some(current) = planning.get_vision().await? else {
            return Err(WebApiError::new("not_found", "vision not found; create a vision before refining", 3));
        };

        let (refined_input, refinement_changes, rationale) = refine_vision_heuristically(&current, focus.as_deref());
        let updated_vision = planning.draft_vision(refined_input).await?;

        self.publish_event(
            "vision-refine",
            json!({
                "vision_id": updated_vision.id,
                "mode": "heuristic",
                "focus": focus,
            }),
        );

        Ok(json!({
            "updated_vision": updated_vision,
            "refinement": {
                "mode": "heuristic",
                "focus": focus,
                "rationale": rationale,
                "changes": refinement_changes,
            }
        }))
    }
}
