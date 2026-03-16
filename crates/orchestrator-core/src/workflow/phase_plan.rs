use std::path::Path;

use anyhow::{anyhow, Result};

pub const STANDARD_WORKFLOW_REF: &str = "ao.task/standard";
pub const UI_UX_WORKFLOW_REF: &str = "ao.task/ui-ux";
pub const REQUIREMENT_TASK_GENERATION_WORKFLOW_REF: &str = "ao.requirement/plan";
pub const REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF: &str = "ao.requirement/execute";

const LEGACY_STANDARD_WORKFLOW_REF: &str = "standard";
const LEGACY_UI_UX_WORKFLOW_REF: &str = "ui-ux-standard";
const LEGACY_REQUIREMENT_TASK_GENERATION_WORKFLOW_REF: &str = "requirement-task-generation";
const LEGACY_REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF: &str = "requirement-task-generation-run";

fn standard_phase_plan() -> Vec<String> {
    vec!["requirements".to_string(), "implementation".to_string(), "code-review".to_string(), "testing".to_string()]
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

fn requirement_task_generation_phase_plan() -> Vec<String> {
    vec!["requirement-task-generation".to_string()]
}

fn requirement_task_generation_run_phase_plan() -> Vec<String> {
    vec!["requirement-task-generation".to_string(), "requirement-workflow-bootstrap".to_string()]
}

fn normalize_requested_workflow_ref(workflow_ref: Option<&str>) -> Option<String> {
    let requested = workflow_ref.map(str::trim).filter(|value| !value.is_empty())?;
    let normalized = requested.to_ascii_lowercase();

    match normalized.as_str() {
        STANDARD_WORKFLOW_REF | LEGACY_STANDARD_WORKFLOW_REF | "builtin/task-standard" => {
            Some(STANDARD_WORKFLOW_REF.to_string())
        }
        UI_UX_WORKFLOW_REF
        | LEGACY_UI_UX_WORKFLOW_REF
        | "builtin/task-ui-ux"
        | "ui-ux"
        | "uiux"
        | "frontend"
        | "frontend-ui-ux"
        | "product-ui" => Some(UI_UX_WORKFLOW_REF.to_string()),
        REQUIREMENT_TASK_GENERATION_WORKFLOW_REF
        | LEGACY_REQUIREMENT_TASK_GENERATION_WORKFLOW_REF
        | "builtin/requirement-plan" => Some(REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string()),
        REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF
        | LEGACY_REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF
        | "builtin/requirements-execute" => Some(REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF.to_string()),
        _ => Some(requested.to_string()),
    }
}

fn raw_requested_workflow_ref(workflow_ref: Option<&str>) -> Option<String> {
    workflow_ref.map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned)
}

pub fn phase_plan_for_workflow_ref(workflow_ref: Option<&str>) -> Vec<String> {
    let normalized =
        normalize_requested_workflow_ref(workflow_ref).unwrap_or_else(|| STANDARD_WORKFLOW_REF.to_string());

    match normalized.as_str() {
        STANDARD_WORKFLOW_REF => standard_phase_plan(),
        UI_UX_WORKFLOW_REF => ui_ux_phase_plan(),
        REQUIREMENT_TASK_GENERATION_WORKFLOW_REF => requirement_task_generation_phase_plan(),
        REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF => requirement_task_generation_run_phase_plan(),
        _ => standard_phase_plan(),
    }
}

pub fn resolve_phase_plan_for_workflow_ref(
    project_root: Option<&Path>,
    workflow_ref: Option<&str>,
) -> Result<Vec<String>> {
    let requested_workflow_ref = raw_requested_workflow_ref(workflow_ref);
    let normalized_workflow_ref = normalize_requested_workflow_ref(workflow_ref);

    let Some(root) = project_root else {
        return Ok(phase_plan_for_workflow_ref(normalized_workflow_ref.as_deref()));
    };

    let workflow_config_path = crate::workflow_config_path(root);
    let single_yaml = root.join(".ao").join("workflows.yaml");
    let yaml_dir = crate::yaml_workflows_dir(root);
    let has_yaml_workflows = single_yaml.exists()
        || std::fs::read_dir(&yaml_dir)
            .ok()
            .into_iter()
            .flat_map(|entries| entries.filter_map(|entry| entry.ok()))
            .any(|entry| entry.path().extension().map(|ext| ext == "yaml" || ext == "yml").unwrap_or(false));
    let has_pack_workflows =
        crate::resolve_pack_registry(root).map(|registry| registry.has_pack_overlays()).unwrap_or(false);
    let has_legacy_workflow_config =
        crate::legacy_workflow_config_paths(root).iter().any(|candidate| candidate.exists());
    if !has_yaml_workflows && !has_pack_workflows && !workflow_config_path.exists() && !has_legacy_workflow_config {
        return Ok(phase_plan_for_workflow_ref(normalized_workflow_ref.as_deref()));
    }

    let loaded_workflow = crate::load_workflow_config_with_metadata(root)?;
    let workflow_config = loaded_workflow.config;
    let runtime_config = crate::load_agent_runtime_config_or_default(root);
    crate::validate_workflow_and_runtime_configs(&workflow_config, &runtime_config)?;

    if let Some(phases) = crate::resolve_workflow_phase_plan(&workflow_config, requested_workflow_ref.as_deref()) {
        return Ok(phases);
    }

    if requested_workflow_ref != normalized_workflow_ref {
        if let Some(phases) = crate::resolve_workflow_phase_plan(&workflow_config, normalized_workflow_ref.as_deref()) {
            return Ok(phases);
        }
    }

    let requested = requested_workflow_ref
        .as_deref()
        .or(normalized_workflow_ref.as_deref())
        .unwrap_or(workflow_config.default_workflow_ref.as_str());
    let available =
        workflow_config.workflows.iter().map(|workflow| workflow.id.as_str()).collect::<Vec<_>>().join(", ");
    let available_display = if available.is_empty() { "<none>" } else { available.as_str() };

    Err(anyhow!(
        "workflow '{requested}' not found in workflow config at {} (available: {available_display})",
        loaded_workflow.path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ensure_stable_home() {
        crate::test_env::stable_test_home();
    }

    fn write_pack_fixture(root: &std::path::Path, pack_id: &str, version: &str, workflow_id: &str) {
        fs::create_dir_all(root.join("workflows")).expect("create workflows");
        fs::create_dir_all(root.join("runtime")).expect("create runtime");
        fs::write(
            root.join(crate::PACK_MANIFEST_FILE_NAME),
            format!(
                r#"
schema = "ao.pack.v1"
id = "{pack_id}"
version = "{version}"
kind = "domain-pack"
title = "{pack_id}"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
ao_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["ao.task"]
default_kind = "ao.task"

[workflows]
root = "workflows"
exports = ["{pack_id}/{workflow_id}"]

[runtime]
workflow_overlay = "runtime/workflow-runtime.overlay.yaml"
"#
            ),
        )
        .expect("write manifest");
        fs::write(
            root.join("runtime/workflow-runtime.overlay.yaml"),
            format!(
                r#"
workflows:
  - id: {workflow_id}
    name: "{workflow_id}"
    phases:
      - requirements
      - testing
"#
            ),
        )
        .expect("write workflow overlay");
    }

    #[test]
    fn resolve_phase_plan_falls_back_when_workflow_config_is_missing() {
        ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");

        let phases = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("ui-ux"))
            .expect("missing config should use fallback");

        assert_eq!(phases, ui_ux_phase_plan());
    }

    #[test]
    fn phase_plan_fallback_normalizes_pack_qualified_and_legacy_refs() {
        assert_eq!(phase_plan_for_workflow_ref(Some(STANDARD_WORKFLOW_REF)), standard_phase_plan());
        assert_eq!(phase_plan_for_workflow_ref(Some("standard")), standard_phase_plan());
        assert_eq!(phase_plan_for_workflow_ref(Some("builtin/task-standard")), standard_phase_plan());
        assert_eq!(phase_plan_for_workflow_ref(Some(UI_UX_WORKFLOW_REF)), ui_ux_phase_plan());
        assert_eq!(phase_plan_for_workflow_ref(Some("ui-ux-standard")), ui_ux_phase_plan());
        assert_eq!(
            phase_plan_for_workflow_ref(Some(REQUIREMENT_TASK_GENERATION_WORKFLOW_REF)),
            requirement_task_generation_phase_plan()
        );
        assert_eq!(
            phase_plan_for_workflow_ref(Some(REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF)),
            requirement_task_generation_run_phase_plan()
        );
    }

    #[test]
    fn resolve_phase_plan_errors_when_workflow_config_is_invalid() {
        ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = crate::workflow_config::workflow_config_path(temp.path())
            .parent()
            .expect("config has parent")
            .to_path_buf();
        std::fs::create_dir_all(&state_dir).expect("state dir");
        std::fs::write(state_dir.join(crate::WORKFLOW_CONFIG_FILE_NAME), "{ invalid json")
            .expect("write invalid workflow config");

        let err = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("standard"))
            .expect_err("invalid config should return error");
        let message = err.to_string();
        assert!(message.contains("workflow config JSON is no longer supported"));
        assert!(message.contains(crate::WORKFLOW_CONFIG_FILE_NAME));
    }

    #[test]
    fn resolve_phase_plan_errors_when_legacy_workflow_config_exists_without_v2() {
        ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let legacy_path = crate::legacy_workflow_config_paths(temp.path())[0].clone();
        let parent = legacy_path.parent().expect("legacy parent directory");
        std::fs::create_dir_all(parent).expect("create legacy directory");
        std::fs::write(legacy_path, "{}").expect("write legacy config placeholder");

        let err = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("standard"))
            .expect_err("legacy config should be rejected");
        let message = err.to_string();
        assert!(message.contains("workflow config v2 JSON is no longer supported"));
        assert!(message.contains("found unsupported legacy file"));
    }

    #[test]
    fn resolve_phase_plan_errors_when_pipeline_is_missing_from_config() {
        ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");

        crate::write_workflow_config(temp.path(), &crate::builtin_workflow_config()).expect("write workflow config");

        let err = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("does-not-exist"))
            .expect_err("missing pipeline should return error");
        let message = err.to_string();
        assert!(message.contains("workflow 'does-not-exist' not found"));
        assert!(message.contains("workflow config at"));
    }

    #[test]
    fn resolve_phase_plan_uses_config_phases_for_standard_pipeline() {
        ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = crate::builtin_workflow_config();

        let standard_pipeline = workflow_config
            .workflows
            .iter_mut()
            .find(|pipeline| pipeline.id == STANDARD_WORKFLOW_REF)
            .expect("standard pipeline should exist");
        standard_pipeline.phases =
            vec!["requirements".to_string().into(), "testing".to_string().into(), "implementation".to_string().into()];

        crate::write_workflow_config(temp.path(), &workflow_config).expect("write workflow config");

        let phases = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some(STANDARD_WORKFLOW_REF))
            .expect("resolver should use configured standard pipeline phases");
        assert_eq!(phases, vec!["requirements".to_string(), "testing".to_string(), "implementation".to_string(),]);
        assert_ne!(phases, standard_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_uses_config_default_pipeline_when_none_is_requested() {
        ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = crate::builtin_workflow_config();
        workflow_config.default_workflow_ref = UI_UX_WORKFLOW_REF.to_string();

        crate::write_workflow_config(temp.path(), &workflow_config).expect("write workflow config");

        let phases = resolve_phase_plan_for_workflow_ref(Some(temp.path()), None)
            .expect("resolver should use configured default pipeline");
        assert_eq!(phases, ui_ux_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_prefers_explicit_config_pipeline_before_alias_normalization() {
        ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = crate::builtin_workflow_config();

        let ui_ux_pipeline = workflow_config
            .workflows
            .iter()
            .find(|pipeline| pipeline.id == UI_UX_WORKFLOW_REF)
            .expect("ui-ux pipeline should exist")
            .clone();
        let mut explicit_ui_ux_pipeline = ui_ux_pipeline;
        explicit_ui_ux_pipeline.id = "ui-ux".to_string();
        workflow_config.workflows.push(explicit_ui_ux_pipeline);

        crate::write_workflow_config(temp.path(), &workflow_config).expect("write workflow config");

        let phases = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("ui-ux"))
            .expect("resolver should use explicit configured pipeline id");
        assert_eq!(phases, ui_ux_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_uses_canonical_requirement_workflow_refs_from_builtin_config() {
        ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");

        crate::write_workflow_config(temp.path(), &crate::builtin_workflow_config()).expect("write workflow config");

        let phases =
            resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some(REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF))
                .expect("canonical requirement execute workflow should resolve");
        assert_eq!(phases, requirement_task_generation_run_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_uses_machine_installed_pack_workflows() {
        ensure_stable_home();
        let temp = tempfile::tempdir().expect("project tempdir");

        write_pack_fixture(
            &crate::machine_installed_packs_dir().join("ao.custom").join("0.2.0"),
            "ao.custom",
            "0.2.0",
            "review-pack",
        );

        let phases = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("review-pack"))
            .expect("resolver should use installed pack workflow");
        assert_eq!(phases, vec!["requirements".to_string(), "testing".to_string()]);
    }

    #[test]
    fn resolve_phase_plan_uses_bundled_pack_workflows_without_project_yaml() {
        ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");

        let quick_fix = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("ao.task/quick-fix"))
            .expect("bundled quick-fix workflow should resolve from bundled pack config");
        assert_eq!(quick_fix, vec!["implementation".to_string(), "testing".to_string()]);

        let review_cycle = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("ao.review/cycle"))
            .expect("bundled review workflow should resolve from bundled pack config");
        assert_eq!(review_cycle, vec!["code-review".to_string(), "testing".to_string()]);
    }
}
