use std::path::Path;

use anyhow::{anyhow, Result};

pub const STANDARD_WORKFLOW_REF: &str = "standard-workflow";
pub const UI_UX_WORKFLOW_REF: &str = "ui-ux-standard";

const LEGACY_STANDARD_WORKFLOW_REF: &str = "standard";

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

fn normalize_requested_workflow_ref(workflow_ref: Option<&str>) -> Option<String> {
    let requested = workflow_ref.map(str::trim).filter(|value| !value.is_empty())?;
    let normalized = requested.to_ascii_lowercase();

    match normalized.as_str() {
        STANDARD_WORKFLOW_REF | LEGACY_STANDARD_WORKFLOW_REF => Some(STANDARD_WORKFLOW_REF.to_string()),
        UI_UX_WORKFLOW_REF | "ui-ux" | "uiux" | "frontend" | "frontend-ui-ux" | "product-ui" => {
            Some(UI_UX_WORKFLOW_REF.to_string())
        }
        _ => Some(requested.to_string()),
    }
}

fn raw_requested_workflow_ref(workflow_ref: Option<&str>) -> Option<String> {
    workflow_ref.map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned)
}

fn fallback_phase_plan_for_workflow_ref(workflow_ref: Option<&str>) -> Option<Vec<String>> {
    let normalized = normalize_requested_workflow_ref(workflow_ref);

    match normalized.as_deref() {
        None => Some(standard_phase_plan()),
        Some(STANDARD_WORKFLOW_REF) => Some(standard_phase_plan()),
        Some(UI_UX_WORKFLOW_REF) => Some(ui_ux_phase_plan()),
        Some(_) => None,
    }
}

pub fn phase_plan_for_workflow_ref(workflow_ref: Option<&str>) -> Vec<String> {
    fallback_phase_plan_for_workflow_ref(workflow_ref).unwrap_or_else(standard_phase_plan)
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
    let single_yaml = root.join(".animus").join("workflows.yaml");
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
    // An installed `config_source` plugin (e.g. config-postgres reading team_*,
    // or config-yaml reading author YAML) is an authoritative config source even
    // when no `.animus/workflows/*.yaml`, pack, or compiled cache exists on disk.
    // Mirror `load_workflow_config`'s own gate (loading.rs) so config_source-only
    // projects (no on-disk YAML) can still resolve workflows.
    let has_config_source = orchestrator_config::config_source_client::config_source_installed(root);
    if !has_config_source
        && !has_yaml_workflows
        && !has_pack_workflows
        && !workflow_config_path.exists()
        && !has_legacy_workflow_config
    {
        let requested = requested_workflow_ref.as_deref().or(normalized_workflow_ref.as_deref()).unwrap_or("<default>");
        return Err(anyhow!(
            "workflow '{requested}' is not available until the project defines workflows in .animus/workflows.yaml or .animus/workflows/*.yaml, installs a pack, or installs a config_source plugin"
        ));
    }

    let loaded_workflow = crate::load_workflow_config_with_metadata(root, None)?;
    let workflow_config = loaded_workflow.config;
    let runtime_config = crate::load_agent_runtime_config_or_default(root);
    crate::validate_workflow_and_runtime_configs_with_project_root(&workflow_config, &runtime_config, Some(root))?;

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

    use crate::test_env::pack_fixture_lock;

    fn test_workflow_config_with_standard_pipeline() -> orchestrator_config::WorkflowConfig {
        use orchestrator_config::{
            AgentProfileOverlay, PhaseExecutionDefinition, PhaseExecutionMode, WorkflowDefinition, WorkflowPhaseEntry,
        };
        let mut config = crate::builtin_workflow_config();
        config.default_workflow_ref = STANDARD_WORKFLOW_REF.to_string();
        // v0.6 kernel-purification: the kernel bakes no agents/phases. The
        // fixture defines everything the pipelines reference so the config
        // validates.
        config.tools_allowlist = vec!["cargo".to_string()];
        config.agent_profiles.insert(
            "default".to_string(),
            AgentProfileOverlay { description: Some("Default".to_string()), ..Default::default() },
        );
        for phase_id in
            ["requirements", "ux-research", "wireframe", "mockup-review", "implementation", "code-review", "testing"]
        {
            config.phase_definitions.insert(
                phase_id.to_string(),
                PhaseExecutionDefinition {
                    mode: PhaseExecutionMode::Agent,
                    agent_id: Some("default".to_string()),
                    directive: None,
                    system_prompt: None,
                    runtime: None,
                    capabilities: None,
                    output_contract: None,
                    output_json_schema: None,
                    decision_contract: None,
                    retry: None,
                    skills: Vec::new(),
                    command: None,
                    manual: None,
                    default_tool: None,
                    idempotency: orchestrator_config::Idempotency::Unknown,
                    worktree: None,
                    evals: None,
                },
            );
        }
        config.workflows = vec![
            WorkflowDefinition {
                id: STANDARD_WORKFLOW_REF.to_string(),
                name: "Standard Workflow".to_string(),
                description: "Test fixture pipeline.".to_string(),
                phases: standard_phase_plan().into_iter().map(WorkflowPhaseEntry::Simple).collect(),
                variables: Vec::new(),
                worktree: None,
                budget: None,
            },
            WorkflowDefinition {
                id: UI_UX_WORKFLOW_REF.to_string(),
                name: "UI UX Standard".to_string(),
                description: "Test fixture frontend pipeline.".to_string(),
                phases: ui_ux_phase_plan().into_iter().map(WorkflowPhaseEntry::Simple).collect(),
                variables: Vec::new(),
                worktree: None,
                budget: None,
            },
        ];
        config
    }

    /// Holds the pack fixture lock for the test's duration and clears the
    /// shared machine packs dir both on entry and on drop, so pack fixtures
    /// never leak into tests that resolve workflow refs without the lock.
    struct PackFixtureHome {
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for PackFixtureHome {
        fn drop(&mut self) {
            let machine_packs_dir = crate::machine_installed_packs_dir();
            if machine_packs_dir.exists() {
                let _ = std::fs::remove_dir_all(&machine_packs_dir);
            }
        }
    }

    fn ensure_stable_home() -> PackFixtureHome {
        let guard = pack_fixture_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::test_env::stable_test_home();
        let global_config_dir = protocol::Config::global_config_dir();
        std::fs::create_dir_all(&global_config_dir).expect("create global config dir");
        let config_tmp = global_config_dir.join("config.json.tmp");
        std::fs::write(&config_tmp, "{\n  \"agent_runner_token\": null\n}\n").expect("write valid global config");
        std::fs::rename(&config_tmp, global_config_dir.join("config.json")).expect("publish valid global config");
        let machine_packs_dir = crate::machine_installed_packs_dir();
        if machine_packs_dir.exists() {
            std::fs::remove_dir_all(&machine_packs_dir).expect("clear shared machine pack fixtures");
        }
        PackFixtureHome { _guard: guard }
    }

    fn write_pack_fixture(root: &std::path::Path, pack_id: &str, version: &str, workflow_id: &str) {
        fs::create_dir_all(root.join("workflows")).expect("create workflows");
        fs::create_dir_all(root.join("runtime")).expect("create runtime");
        fs::write(
            root.join(crate::PACK_MANIFEST_FILE_NAME),
            format!(
                r#"
schema = "animus.pack.v1"
id = "{pack_id}"
version = "{version}"
kind = "domain-pack"
title = "{pack_id}"
description = "Fixture"

[ownership]
mode = "bundled"

[compatibility]
animus_core = ">=0.1.0"
workflow_schema = "v2"
subject_schema = "v2"

[subjects]
kinds = ["animus.task"]
default_kind = "animus.task"

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
    fn resolve_phase_plan_errors_when_workflow_config_is_missing() {
        let _home_lock = ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");

        let error = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("ui-ux"))
            .expect_err("missing config should require authored or installed workflows");
        assert!(error.to_string().contains("is not available until the project defines workflows"));
    }

    #[test]
    fn phase_plan_fallback_normalizes_legacy_refs() {
        assert_eq!(phase_plan_for_workflow_ref(Some(STANDARD_WORKFLOW_REF)), standard_phase_plan());
        assert_eq!(phase_plan_for_workflow_ref(Some("standard")), standard_phase_plan());
        assert_eq!(phase_plan_for_workflow_ref(Some(UI_UX_WORKFLOW_REF)), ui_ux_phase_plan());
        assert_eq!(phase_plan_for_workflow_ref(Some("ui-ux-standard")), ui_ux_phase_plan());
        assert_eq!(phase_plan_for_workflow_ref(Some("ui-ux")), ui_ux_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_errors_when_workflow_config_is_invalid() {
        let _home_lock = ensure_stable_home();
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
        let _home_lock = ensure_stable_home();
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
        let _home_lock = ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");

        crate::write_workflow_config(temp.path(), &crate::builtin_workflow_config()).expect("write workflow config");
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let err = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("does-not-exist"))
            .expect_err("missing pipeline should return error");
        let message = err.to_string();
        assert!(message.contains("workflow 'does-not-exist' not found"));
        assert!(message.contains("workflow config at"));
    }

    #[test]
    fn resolve_phase_plan_uses_config_phases_for_standard_pipeline() {
        let _home_lock = ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = test_workflow_config_with_standard_pipeline();

        let standard_pipeline = workflow_config
            .workflows
            .iter_mut()
            .find(|pipeline| pipeline.id == STANDARD_WORKFLOW_REF)
            .expect("standard pipeline should exist");
        standard_pipeline.phases =
            vec!["requirements".to_string().into(), "testing".to_string().into(), "implementation".to_string().into()];

        crate::write_workflow_config(temp.path(), &workflow_config).expect("write workflow config");
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let phases = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some(STANDARD_WORKFLOW_REF))
            .expect("resolver should use configured standard pipeline phases");
        assert_eq!(phases, vec!["requirements".to_string(), "testing".to_string(), "implementation".to_string(),]);
        assert_ne!(phases, standard_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_uses_config_default_pipeline_when_none_is_requested() {
        let _home_lock = ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = test_workflow_config_with_standard_pipeline();
        workflow_config.default_workflow_ref = UI_UX_WORKFLOW_REF.to_string();

        crate::write_workflow_config(temp.path(), &workflow_config).expect("write workflow config");
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let phases = resolve_phase_plan_for_workflow_ref(Some(temp.path()), None)
            .expect("resolver should use configured default pipeline");
        assert_eq!(phases, ui_ux_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_prefers_explicit_config_pipeline_before_alias_normalization() {
        let _home_lock = ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut workflow_config = test_workflow_config_with_standard_pipeline();

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
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let phases = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("ui-ux"))
            .expect("resolver should use explicit configured pipeline id");
        assert_eq!(phases, ui_ux_phase_plan());
    }

    #[test]
    fn resolve_phase_plan_requires_pack_install_for_canonical_requirement_workflow_refs() {
        let _home_lock = ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");

        crate::write_workflow_config(temp.path(), &crate::builtin_workflow_config()).expect("write workflow config");
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let error = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("animus.requirement/execute"))
            .expect_err("requirement pack workflow should not resolve until the pack is installed");
        assert!(error.to_string().contains("workflow 'animus.requirement/execute' not found"));
    }

    #[test]
    fn resolve_phase_plan_uses_machine_installed_pack_workflows() {
        let _home_lock = ensure_stable_home();
        let temp = tempfile::tempdir().expect("project tempdir");

        write_pack_fixture(
            &crate::machine_installed_packs_dir().join("animus.custom").join("0.2.0"),
            "animus.custom",
            "0.2.0",
            "review-pack",
        );
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let phases = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("review-pack"))
            .expect("resolver should use installed pack workflow");
        assert_eq!(phases, vec!["requirements".to_string(), "testing".to_string()]);
    }

    #[test]
    fn resolve_phase_plan_requires_pack_install_for_optional_pack_workflows() {
        let _home_lock = ensure_stable_home();
        let temp = tempfile::tempdir().expect("tempdir");

        let quick_fix_error = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("animus.task/quick-fix"))
            .expect_err("pack workflow should not resolve until the pack is installed");
        assert!(quick_fix_error.to_string().contains("is not available until the project defines workflows"));

        let review_cycle_error = resolve_phase_plan_for_workflow_ref(Some(temp.path()), Some("animus.review/cycle"))
            .expect_err("review pack workflow should not resolve until the pack is installed");
        assert!(review_cycle_error.to_string().contains("is not available until the project defines workflows"));
    }
}
