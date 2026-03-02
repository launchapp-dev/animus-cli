use std::path::Path;

use anyhow::{anyhow, Result};

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

fn normalize_requested_pipeline_id(pipeline_id: Option<&str>) -> Option<String> {
    let requested = pipeline_id
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let normalized = requested.to_ascii_lowercase();

    match normalized.as_str() {
        STANDARD_PIPELINE_ID => Some(STANDARD_PIPELINE_ID.to_string()),
        UI_UX_PIPELINE_ID | "ui-ux" | "uiux" | "frontend" | "frontend-ui-ux" | "product-ui" => {
            Some(UI_UX_PIPELINE_ID.to_string())
        }
        _ => Some(requested.to_string()),
    }
}

fn raw_requested_pipeline_id(pipeline_id: Option<&str>) -> Option<String> {
    pipeline_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub fn phase_plan_for_pipeline_id(pipeline_id: Option<&str>) -> Vec<String> {
    let normalized = normalize_requested_pipeline_id(pipeline_id)
        .unwrap_or_else(|| STANDARD_PIPELINE_ID.to_string());

    match normalized.as_str() {
        STANDARD_PIPELINE_ID => standard_phase_plan(),
        UI_UX_PIPELINE_ID => ui_ux_phase_plan(),
        _ => standard_phase_plan(),
    }
}

pub fn resolve_phase_plan_for_pipeline(
    project_root: Option<&Path>,
    pipeline_id: Option<&str>,
) -> Result<Vec<String>> {
    let requested_pipeline_id = raw_requested_pipeline_id(pipeline_id);
    let normalized_pipeline_id = normalize_requested_pipeline_id(pipeline_id);

    let Some(root) = project_root else {
        return Ok(phase_plan_for_pipeline_id(
            normalized_pipeline_id.as_deref(),
        ));
    };

    let workflow_config_path = crate::workflow_config_path(root);
    let has_legacy_workflow_config = crate::legacy_workflow_config_paths(root)
        .iter()
        .any(|candidate| candidate.exists());
    if !workflow_config_path.exists() && !has_legacy_workflow_config {
        return Ok(phase_plan_for_pipeline_id(
            normalized_pipeline_id.as_deref(),
        ));
    }

    let workflow_config = crate::load_workflow_config(root)?;
    let runtime_config = crate::load_agent_runtime_config(root)?;
    crate::validate_workflow_and_runtime_configs(&workflow_config, &runtime_config)?;

    if let Some(phases) =
        crate::resolve_pipeline_phase_plan(&workflow_config, requested_pipeline_id.as_deref())
    {
        return Ok(phases);
    }

    if requested_pipeline_id != normalized_pipeline_id {
        if let Some(phases) =
            crate::resolve_pipeline_phase_plan(&workflow_config, normalized_pipeline_id.as_deref())
        {
            return Ok(phases);
        }
    }

    let requested = requested_pipeline_id
        .as_deref()
        .or(normalized_pipeline_id.as_deref())
        .unwrap_or(workflow_config.default_pipeline_id.as_str());
    let available = workflow_config
        .pipelines
        .iter()
        .map(|pipeline| pipeline.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let available_display = if available.is_empty() {
        "<none>"
    } else {
        available.as_str()
    };

    Err(anyhow!(
        "pipeline '{requested}' not found in workflow config at {} (available: {available_display})",
        workflow_config_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_phase_plan_falls_back_when_workflow_config_is_missing() {
        let temp = tempfile::tempdir().expect("tempdir");

        let phases = resolve_phase_plan_for_pipeline(Some(temp.path()), Some("ui-ux"))
            .expect("missing config should use fallback");

        assert_eq!(phases, ui_ux_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_errors_when_workflow_config_is_invalid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join(".ao").join("state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        std::fs::write(
            state_dir.join(crate::WORKFLOW_CONFIG_FILE_NAME),
            "{ invalid json",
        )
        .expect("write invalid workflow config");

        let err = resolve_phase_plan_for_pipeline(Some(temp.path()), Some("standard"))
            .expect_err("invalid config should return error");
        let message = err.to_string();
        assert!(message.contains("invalid workflow config JSON"));
        assert!(message.contains(crate::WORKFLOW_CONFIG_FILE_NAME));
    }

    #[test]
    fn resolve_phase_plan_errors_when_legacy_workflow_config_exists_without_v2() {
        let temp = tempfile::tempdir().expect("tempdir");
        let legacy_path = crate::legacy_workflow_config_paths(temp.path())[0].clone();
        let parent = legacy_path.parent().expect("legacy parent directory");
        std::fs::create_dir_all(parent).expect("create legacy directory");
        std::fs::write(legacy_path, "{}").expect("write legacy config placeholder");

        let err = resolve_phase_plan_for_pipeline(Some(temp.path()), Some("standard"))
            .expect_err("legacy config should return migration guidance");
        let message = err.to_string();
        assert!(message.contains("workflow config v2 is required"));
        assert!(message.contains("migrate-v2"));
    }

    #[test]
    fn resolve_phase_plan_errors_when_pipeline_is_missing_from_config() {
        let temp = tempfile::tempdir().expect("tempdir");

        crate::write_workflow_config(temp.path(), &crate::builtin_workflow_config())
            .expect("write workflow config");
        crate::write_agent_runtime_config(temp.path(), &crate::builtin_agent_runtime_config())
            .expect("write runtime config");

        let err = resolve_phase_plan_for_pipeline(Some(temp.path()), Some("does-not-exist"))
            .expect_err("missing pipeline should return error");
        let message = err.to_string();
        assert!(message.contains("pipeline 'does-not-exist' not found"));
        assert!(message.contains(crate::WORKFLOW_CONFIG_FILE_NAME));
    }

    #[test]
    fn resolve_phase_plan_uses_config_phases_for_standard_pipeline() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = crate::builtin_workflow_config();

        let standard_pipeline = workflow_config
            .pipelines
            .iter_mut()
            .find(|pipeline| pipeline.id == STANDARD_PIPELINE_ID)
            .expect("standard pipeline should exist");
        standard_pipeline.phases = vec![
            "requirements".to_string().into(),
            "testing".to_string().into(),
            "implementation".to_string().into(),
        ];

        crate::write_workflow_config(temp.path(), &workflow_config).expect("write workflow config");
        crate::write_agent_runtime_config(temp.path(), &crate::builtin_agent_runtime_config())
            .expect("write runtime config");

        let phases = resolve_phase_plan_for_pipeline(Some(temp.path()), Some(STANDARD_PIPELINE_ID))
            .expect("resolver should use configured standard pipeline phases");
        assert_eq!(
            phases,
            vec![
                "requirements".to_string(),
                "testing".to_string(),
                "implementation".to_string(),
            ]
        );
        assert_ne!(phases, standard_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_uses_config_default_pipeline_when_none_is_requested() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = crate::builtin_workflow_config();
        workflow_config.default_pipeline_id = UI_UX_PIPELINE_ID.to_string();

        crate::write_workflow_config(temp.path(), &workflow_config).expect("write workflow config");
        crate::write_agent_runtime_config(temp.path(), &crate::builtin_agent_runtime_config())
            .expect("write runtime config");

        let phases = resolve_phase_plan_for_pipeline(Some(temp.path()), None)
            .expect("resolver should use configured default pipeline");
        assert_eq!(phases, ui_ux_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_prefers_explicit_config_pipeline_before_alias_normalization() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = crate::builtin_workflow_config();

        let ui_ux_pipeline = workflow_config
            .pipelines
            .iter_mut()
            .find(|pipeline| pipeline.id == UI_UX_PIPELINE_ID)
            .expect("ui-ux pipeline should exist");
        ui_ux_pipeline.id = "ui-ux".to_string();

        crate::write_workflow_config(temp.path(), &workflow_config).expect("write workflow config");
        crate::write_agent_runtime_config(temp.path(), &crate::builtin_agent_runtime_config())
            .expect("write runtime config");

        let phases = resolve_phase_plan_for_pipeline(Some(temp.path()), Some("ui-ux"))
            .expect("resolver should use explicit configured pipeline id");
        assert_eq!(phases, ui_ux_phase_plan());
    }
}
