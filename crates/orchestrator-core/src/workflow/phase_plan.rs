pub const STANDARD_PIPELINE_ID: &str = "standard";
pub const UI_UX_PIPELINE_ID: &str = "ui-ux-standard";

fn standard_phase_plan() -> Vec<String> {
    vec![
        "requirements".to_string(),
        "implementation".to_string(),
        "code-review".to_string(),
        "testing".to_string(),
    ]
}

fn ui_ux_phase_plan() -> Vec<String> {
    vec![
        "requirements".to_string(),
        "ux-research".to_string(),
        "wireframe".to_string(),
        "mockup-review".to_string(),
        "implementation".to_string(),
        "code-review".to_string(),
        "testing".to_string(),
    ]
}

pub fn phase_plan_for_pipeline_id(pipeline_id: Option<&str>) -> Vec<String> {
    let normalized = pipeline_id
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| STANDARD_PIPELINE_ID.to_string());

    match normalized.as_str() {
        "standard" => standard_phase_plan(),
        "ui-ux-standard" | "ui-ux" | "uiux" | "frontend" | "frontend-ui-ux" | "product-ui" => {
            ui_ux_phase_plan()
        }
        _ => standard_phase_plan(),
    }
}
