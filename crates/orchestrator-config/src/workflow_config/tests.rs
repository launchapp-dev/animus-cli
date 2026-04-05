use std::collections::{BTreeMap, HashMap};
use std::fs;

use crate::agent_runtime_config::{CommandCwdMode, PhaseCommandDefinition, PhaseExecutionMode};
use crate::test_support::{env_lock, EnvVarGuard};
use crate::PhaseExecutionDefinition;

use super::builtins::{builtin_workflow_config, builtin_workflow_config_base};
use super::loading::load_workflow_config;
use super::resolution::{resolve_workflow_phase_plan, resolve_workflow_rework_attempts, resolve_workflow_skip_guards};
use super::types::*;
use super::validation::{
    validate_workflow_and_runtime_configs, validate_workflow_and_runtime_configs_with_project_root,
    validate_workflow_config, validate_workflow_config_with_project_root,
};
use super::yaml_compiler::{compile_yaml_workflow_files, merge_yaml_into_config, validate_and_compile_yaml_workflows};
use super::yaml_parser::parse_yaml_workflow_config;

#[test]
fn builtin_workflow_config_is_valid() {
    let config = builtin_workflow_config();
    validate_workflow_config(&config).expect("builtin config should validate");
}

#[test]
fn builtin_workflow_config_includes_planning_workflow_refs() {
    let config = builtin_workflow_config();
    let workflow_ids = config.workflows.iter().map(|workflow| workflow.id.as_str()).collect::<Vec<_>>();

    assert_eq!(config.default_workflow_ref, "standard-workflow");
    assert!(workflow_ids.contains(&"ao.vision/draft"));
    assert!(workflow_ids.contains(&"ao.vision/refine"));
    assert!(workflow_ids.contains(&"standard-workflow"));
    assert!(workflow_ids.contains(&"ui-ux-standard"));
    assert!(workflow_ids.contains(&"builtin/vision-draft"));
    assert!(workflow_ids.contains(&"builtin/vision-refine"));
    assert!(!workflow_ids.contains(&"ao.task/standard"));
    assert!(!workflow_ids.contains(&"ao.task/ui-ux"));
    assert!(!workflow_ids.contains(&"ao.task/quick-fix"));
    assert!(!workflow_ids.contains(&"ao.task/gated"));
    assert!(!workflow_ids.contains(&"ao.task/triage"));
    assert!(!workflow_ids.contains(&"ao.task/refine"));
    assert!(!workflow_ids.contains(&"ao.review/cycle"));
    assert!(!workflow_ids.contains(&"ao.requirement/draft"));
    assert!(!workflow_ids.contains(&"ao.requirement/refine"));
    assert!(!workflow_ids.contains(&"ao.requirement/plan"));
    assert!(!workflow_ids.contains(&"ao.requirement/execute"));
}

#[test]
fn standard_workflow_has_feature_branch_merge_configuration() {
    let config = builtin_workflow_config();
    let standard_workflow =
        config.workflows.iter().find(|w| w.id == "standard-workflow").expect("standard-workflow should exist");

    // Verify post_success is configured for feature branch workflow
    let post_success =
        standard_workflow.post_success.as_ref().expect("standard-workflow should have post_success configured");

    let merge_config = post_success.merge.as_ref().expect("standard-workflow should have merge configuration");

    // Feature branch workflow should create a PR without auto-merging
    assert_eq!(merge_config.target_branch, "main");
    assert!(merge_config.create_pr, "standard-workflow should create PR");
    assert!(!merge_config.auto_merge, "standard-workflow should not auto-merge");
    assert!(merge_config.cleanup_worktree, "standard-workflow should cleanup worktree after merge");
    assert_eq!(merge_config.strategy, MergeStrategy::Merge, "standard-workflow should use merge strategy");
}

#[test]
fn ui_ux_workflow_has_feature_branch_merge_configuration() {
    let config = builtin_workflow_config();
    let ui_ux_workflow =
        config.workflows.iter().find(|w| w.id == "ui-ux-standard").expect("ui-ux-standard should exist");

    // Verify post_success is configured for feature branch workflow
    let post_success =
        ui_ux_workflow.post_success.as_ref().expect("ui-ux-standard should have post_success configured");

    let merge_config = post_success.merge.as_ref().expect("ui-ux-standard should have merge configuration");

    // Feature branch workflow should create a PR without auto-merging
    assert_eq!(merge_config.target_branch, "main");
    assert!(merge_config.create_pr, "ui-ux-standard should create PR");
    assert!(!merge_config.auto_merge, "ui-ux-standard should not auto-merge");
    assert!(merge_config.cleanup_worktree, "ui-ux-standard should cleanup worktree after merge");
}

#[test]
fn missing_v2_file_reports_actionable_error() {
    let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let error = load_workflow_config(temp.path()).expect_err("missing workflow config should fail");
    assert!(error.to_string().contains("workflow config is missing"));
}

#[test]
fn checkpoint_retention_requires_positive_keep_last_per_phase() {
    let mut config = builtin_workflow_config();
    config.checkpoint_retention.keep_last_per_phase = 0;
    let err = validate_workflow_config(&config).expect_err("invalid retention should fail");
    assert!(
        err.to_string().contains("checkpoint_retention.keep_last_per_phase"),
        "validation error should mention checkpoint retention"
    );
}

#[test]
fn validation_rejects_on_verdict_targeting_nonexistent_phase() {
    let mut config = builtin_workflow_config();
    let standard_pipeline =
        config.workflows.iter_mut().find(|p| p.id == "standard-workflow").expect("standard workflow");

    let mut on_verdict = HashMap::new();
    on_verdict.insert(
        "rework".to_string(),
        PhaseTransitionConfig {
            target: "nonexistent-phase".to_string(),
            guard: None,
            allow_agent_target: false,
            allowed_targets: Vec::new(),
        },
    );
    standard_pipeline.phases[0] = WorkflowPhaseEntry::Rich(WorkflowPhaseConfig {
        id: "requirements".to_string(),
        max_rework_attempts: 3,
        on_verdict,
        skip_if: Vec::new(),
    });

    let err = validate_workflow_config(&config).expect_err("on_verdict with nonexistent target should fail validation");
    let message = err.to_string();
    assert!(
        message.contains("targets unknown phase 'nonexistent-phase'"),
        "error should mention the unknown target phase: {}",
        message
    );
}

#[test]
fn validation_rejects_zero_max_rework_attempts() {
    let mut config = builtin_workflow_config();
    let standard_pipeline =
        config.workflows.iter_mut().find(|p| p.id == "standard-workflow").expect("standard workflow");

    standard_pipeline.phases[1] = WorkflowPhaseEntry::Rich(WorkflowPhaseConfig {
        id: "implementation".to_string(),
        max_rework_attempts: 0,
        on_verdict: HashMap::new(),
        skip_if: Vec::new(),
    });

    let err = validate_workflow_config(&config).expect_err("zero max_rework_attempts should fail validation");
    let message = err.to_string();
    assert!(
        message.contains("max_rework_attempts must be greater than 0"),
        "error should mention max_rework_attempts: {message}"
    );
}

#[test]
fn serde_round_trips_simple_string_phases() {
    let config = builtin_workflow_config();
    let json = serde_json::to_string(&config).expect("serialize");
    let deserialized: WorkflowConfig = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(deserialized.workflows.len(), config.workflows.len());
    for (orig, deser) in config.workflows.iter().zip(deserialized.workflows.iter()) {
        let orig_ids: Vec<&str> = orig.phases.iter().map(|e| e.phase_id()).collect();
        let deser_ids: Vec<&str> = deser.phases.iter().map(|e| e.phase_id()).collect();
        assert_eq!(orig_ids, deser_ids);
    }
}

#[test]
fn serde_deserializes_rich_phase_config() {
    let json = r#"{
        "id": "code-review",
        "on_verdict": {
            "rework": { "target": "implementation" }
        }
    }"#;
    let entry: WorkflowPhaseEntry = serde_json::from_str(json).expect("deserialize rich entry");
    assert_eq!(entry.phase_id(), "code-review");
    assert_eq!(entry.max_rework_attempts().unwrap_or_default(), 3);
    let verdicts = entry.on_verdict().expect("should have on_verdict");
    assert!(verdicts.contains_key("rework"));
    assert_eq!(verdicts["rework"].target, "implementation");
}

#[test]
fn serde_deserializes_rich_phase_config_with_custom_max_rework_attempts() {
    let json = r#"{
        "id": "testing",
        "max_rework_attempts": 1,
        "on_verdict": {
            "rework": { "target": "implementation" }
        }
    }"#;
    let entry: WorkflowPhaseEntry = serde_json::from_str(json).expect("deserialize rich entry");
    assert_eq!(entry.phase_id(), "testing");
    assert_eq!(entry.max_rework_attempts().unwrap_or_default(), 1);
    let verdicts = entry.on_verdict().expect("should have on_verdict");
    assert_eq!(verdicts["rework"].target, "implementation");
}

#[test]
fn resolve_workflow_rework_attempts_uses_defaults() {
    let config = builtin_workflow_config();
    let attempts = resolve_workflow_rework_attempts(&config, Some("standard"));
    assert!(attempts.is_empty());
}

#[test]
fn serde_deserializes_simple_string_phase() {
    let json = r#""requirements""#;
    let entry: WorkflowPhaseEntry = serde_json::from_str(json).expect("deserialize simple string");
    assert_eq!(entry.phase_id(), "requirements");
    assert!(entry.on_verdict().is_none());
}

#[test]
fn serde_deserializes_mixed_pipeline_phases() {
    let json = r#"{
        "id": "test-workflow",
        "name": "Test",
        "description": "",
        "phases": [
            "requirements",
            { "id": "implementation", "on_verdict": { "rework": { "target": "requirements" } } },
            "testing"
        ]
    }"#;
    let workflow: WorkflowDefinition = serde_json::from_str(json).expect("deserialize");
    assert_eq!(workflow.phases.len(), 3);
    assert_eq!(workflow.phases[0].phase_id(), "requirements");
    assert!(workflow.phases[0].on_verdict().is_none());
    assert_eq!(workflow.phases[1].phase_id(), "implementation");
    let verdicts = workflow.phases[1].on_verdict().expect("should have verdicts");
    assert_eq!(verdicts["rework"].target, "requirements");
    assert_eq!(workflow.phases[2].phase_id(), "testing");
    assert!(workflow.phases[2].on_verdict().is_none());
}

#[test]
fn pipeline_phase_entry_deserializes_from_string() {
    let json = r#""requirements""#;
    let entry: WorkflowPhaseEntry = serde_json::from_str(json).expect("parse string entry");
    assert_eq!(entry.phase_id(), "requirements");
    assert!(entry.skip_if().is_empty());
}

#[test]
fn pipeline_phase_entry_deserializes_from_object_with_skip_if() {
    let json = r#"{"id": "testing", "skip_if": ["task_type == 'docs'"]}"#;
    let entry: WorkflowPhaseEntry = serde_json::from_str(json).expect("parse config entry");
    assert_eq!(entry.phase_id(), "testing");
    assert_eq!(entry.skip_if(), &["task_type == 'docs'"]);
}

#[test]
fn pipeline_phase_entry_deserializes_from_object_without_skip_if() {
    let json = r#"{"id": "implementation"}"#;
    let entry: WorkflowPhaseEntry = serde_json::from_str(json).expect("parse config entry");
    assert_eq!(entry.phase_id(), "implementation");
    assert!(entry.skip_if().is_empty());
}

#[test]
fn pipeline_definition_deserializes_mixed_phase_entries() {
    let json = r#"{
        "id": "test-workflow",
        "name": "Test",
        "phases": [
            "requirements",
            {"id": "testing", "skip_if": ["task_type == 'docs'"]},
            "implementation"
        ]
    }"#;
    let workflow: WorkflowDefinition = serde_json::from_str(json).expect("parse mixed workflow");
    assert_eq!(workflow.phases.len(), 3);
    assert_eq!(workflow.phases[0].phase_id(), "requirements");
    assert!(workflow.phases[0].skip_if().is_empty());
    assert_eq!(workflow.phases[1].phase_id(), "testing");
    assert_eq!(workflow.phases[1].skip_if(), &["task_type == 'docs'"]);
    assert_eq!(workflow.phases[2].phase_id(), "implementation");
}

#[test]
fn resolve_workflow_skip_guards_extracts_guards_from_config() {
    let mut config = builtin_workflow_config();
    let standard_pipeline =
        config.workflows.iter_mut().find(|p| p.id == "standard-workflow").expect("standard workflow");
    standard_pipeline.phases = vec![
        "requirements".to_string().into(),
        WorkflowPhaseEntry::Rich(WorkflowPhaseConfig {
            id: "testing".to_string(),
            max_rework_attempts: 3,
            on_verdict: HashMap::new(),
            skip_if: vec!["task_type == 'docs'".to_string()],
        }),
        "implementation".to_string().into(),
    ];

    let guards = resolve_workflow_skip_guards(&config, Some("standard-workflow"));
    assert_eq!(guards.len(), 1);
    assert_eq!(guards.get("testing").unwrap(), &vec!["task_type == 'docs'".to_string()]);
}

#[test]
fn yaml_parses_simple_pipeline() {
    let yaml = r#"
workflows:
  - id: standard
    name: Standard Pipeline
    description: Default development workflow
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse simple YAML");
    let standard = config.workflows.iter().find(|p| p.id == "standard").expect("should have standard workflow");
    assert_eq!(standard.name, "Standard Pipeline");
    assert_eq!(standard.phases.len(), 4);
    assert_eq!(standard.phases[0].phase_id(), "requirements");
    assert_eq!(standard.phases[1].phase_id(), "implementation");
    assert_eq!(standard.phases[2].phase_id(), "code-review");
    assert_eq!(standard.phases[3].phase_id(), "testing");
}

#[test]
fn yaml_parses_rich_phase_with_skip_if() {
    let yaml = r#"
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing:
          skip_if:
            - "task_type == 'docs'"
      - code-review
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with skip_if");
    let standard = config.workflows.iter().find(|p| p.id == "standard").expect("should have standard workflow");
    assert_eq!(standard.phases.len(), 4);
    assert_eq!(standard.phases[2].phase_id(), "testing");
    assert_eq!(standard.phases[2].skip_if(), &["task_type == 'docs'"]);
}

#[test]
fn yaml_parses_rich_phase_with_on_verdict() {
    let yaml = r#"
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - code-review:
          on_verdict:
            rework:
              target: implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with on_verdict");
    let standard = config.workflows.iter().find(|p| p.id == "standard").expect("should have standard workflow");
    assert_eq!(standard.phases[2].phase_id(), "code-review");
    let verdicts = standard.phases[2].on_verdict().expect("should have on_verdict");
    assert_eq!(verdicts["rework"].target, "implementation");
    assert_eq!(standard.phases[2].max_rework_attempts().expect("has attempts"), 3);
}

#[test]
fn yaml_parses_rich_phase_with_custom_max_rework_attempts() {
    let yaml = r#"
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - testing:
          max_rework_attempts: 1
          on_verdict:
            rework:
              target: implementation
      - implementation
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with custom max_rework_attempts");
    let standard = config.workflows.iter().find(|p| p.id == "standard").expect("should have standard workflow");
    assert_eq!(standard.phases[1].max_rework_attempts().expect("has attempts"), 1);
}

#[test]
fn yaml_parses_mixed_simple_and_rich_phases() {
    let yaml = r#"
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing:
          skip_if:
            - "task_type == 'docs'"
      - code-review:
          on_verdict:
            rework:
              target: implementation
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse mixed phases");
    let standard = config.workflows.iter().find(|p| p.id == "standard").expect("should have standard workflow");
    assert_eq!(standard.phases.len(), 4);
    assert_eq!(standard.phases[0].phase_id(), "requirements");
    assert!(standard.phases[0].on_verdict().is_none());
    assert!(standard.phases[0].skip_if().is_empty());
    assert_eq!(standard.phases[2].phase_id(), "testing");
    assert_eq!(standard.phases[2].skip_if(), &["task_type == 'docs'"]);
    assert_eq!(standard.phases[3].phase_id(), "code-review");
    let verdicts = standard.phases[3].on_verdict().expect("should have on_verdict");
    assert_eq!(verdicts["rework"].target, "implementation");
}

#[test]
fn yaml_parses_post_success_merge_block() {
    let yaml = r#"
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
    post_success:
      merge:
        strategy: rebase
        target_branch: release
        create_pr: true
        auto_merge: true
        cleanup_worktree: false
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with post_success");
    let standard = config.workflows.iter().find(|p| p.id == "standard").expect("workflow_ref");
    let post_success = standard.post_success.as_ref().expect("post_success should be present");
    let merge = post_success.merge.as_ref().expect("merge config should be present");
    assert_eq!(merge.strategy, MergeStrategy::Rebase);
    assert_eq!(merge.target_branch, "release");
    assert!(merge.create_pr);
    assert!(merge.auto_merge);
    assert!(!merge.cleanup_worktree);
}

#[test]
fn yaml_parses_invalid_merge_strategy() {
    let yaml = r#"
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
    post_success:
      merge:
        strategy: invalid
        target_branch: main
"#;
    let err = parse_yaml_workflow_config(yaml).expect_err("invalid merge strategy should fail parsing");
    let message = err.to_string();
    assert!(message.contains("strategy must be one of"), "error should mention supported strategies: {}", message);
}

#[test]
fn yaml_merge_replaces_pipeline_by_id() {
    let base = builtin_workflow_config();
    let yaml = r#"
workflows:
  - id: standard
    name: Overridden Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let yaml_config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let merged = merge_yaml_into_config(base.clone(), yaml_config);
    let standard = merged.workflows.iter().find(|p| p.id == "standard").expect("standard workflow");
    assert_eq!(standard.name, "Overridden Standard");
    assert_eq!(standard.phases.len(), 3);
    assert!(merged.workflows.iter().any(|p| p.id == "ui-ux-standard"), "non-overridden workflow should be preserved");
}

#[test]
fn yaml_merge_adds_new_pipeline() {
    let base = builtin_workflow_config();
    let base_count = base.workflows.len();
    let yaml = r#"
workflows:
  - id: quick-fix
    name: Quick Fix
    phases:
      - implementation
      - testing
"#;
    let yaml_config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let merged = merge_yaml_into_config(base, yaml_config);
    assert_eq!(merged.workflows.len(), base_count + 1);
    assert!(merged.workflows.iter().any(|p| p.id == "quick-fix"));
}

#[test]
fn yaml_missing_files_returns_none() {
    let temp = tempfile::tempdir().expect("tempdir");
    let result = compile_yaml_workflow_files(temp.path()).expect("should not error");
    assert!(result.is_none());
}

#[test]
fn yaml_invalid_syntax_returns_error() {
    let yaml = "workflows:\n  - id: [invalid";
    let result = parse_yaml_workflow_config(yaml);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("failed to parse YAML"), "error should mention YAML parsing: {}", err);
}

#[test]
fn yaml_pipeline_name_defaults_to_id() {
    let yaml = r#"
workflows:
  - id: quick-fix
    phases:
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse");
    let workflow = config.workflows.iter().find(|p| p.id == "quick-fix").expect("workflow_ref");
    assert_eq!(workflow.name, "quick-fix");
}

#[test]
fn yaml_compile_reads_from_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workflows_dir = temp.path().join(".ao").join("workflows");
    fs::create_dir_all(&workflows_dir).expect("create workflows dir");
    fs::write(
        workflows_dir.join("workflows.yaml"),
        r#"
workflows:
  - id: standard
    name: YAML Standard
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#,
    )
    .expect("write yaml");

    let result = compile_yaml_workflow_files(temp.path()).expect("compile should succeed");
    let config = result.expect("should have config");
    let standard = config.workflows.iter().find(|p| p.id == "standard").expect("standard workflow");
    assert_eq!(standard.name, "YAML Standard");
}

#[test]
fn yaml_compile_reads_single_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ao_dir = temp.path().join(".ao");
    fs::create_dir_all(&ao_dir).expect("create .ao dir");
    fs::write(
        ao_dir.join("workflows.yaml"),
        r#"
workflows:
  - id: standard
    name: Single File Standard
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#,
    )
    .expect("write yaml");

    let result = compile_yaml_workflow_files(temp.path()).expect("compile should succeed");
    let config = result.expect("should have config");
    let standard = config.workflows.iter().find(|p| p.id == "standard").expect("standard workflow");
    assert_eq!(standard.name, "Single File Standard");
}

#[test]
fn yaml_compile_resolves_project_scoped_skills() {
    let temp = tempfile::tempdir().expect("tempdir");
    let skills_dir = temp.path().join(".ao").join("config").join("skill_definitions");
    fs::create_dir_all(&skills_dir).expect("create project skills dir");
    fs::write(
        skills_dir.join("project-skill.yaml"),
        r#"
name: project-skill
description: Project local validation fixture
"#,
    )
    .expect("write project skill");

    let ao_dir = temp.path().join(".ao");
    fs::create_dir_all(&ao_dir).expect("create .ao dir");
    fs::write(
        ao_dir.join("workflows.yaml"),
        r#"
phase_catalog:
  project-phase:
    label: Project Phase
    category: verification
phases:
  project-phase:
    mode: agent
    agent_id: project-agent
agents:
  project-agent:
    description: Project agent
    system_prompt: Project prompt
    skills:
      - project-skill
workflows:
  - id: project-skill-test
    name: Project Skill Test
    phases:
      - project-phase
"#,
    )
    .expect("write workflow yaml");

    let result = compile_yaml_workflow_files(temp.path()).expect("compile should succeed");
    let config = result.expect("should have config");
    assert!(
        config.agent_profiles.get("project-agent").is_some_and(|profile| profile.skills == vec!["project-skill"]),
        "project-local skill reference should remain intact"
    );
}

#[test]
fn validate_and_compile_yaml_validates_and_reloads() {
    let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");

    let workflows_dir = temp.path().join(".ao").join("workflows");
    fs::create_dir_all(&workflows_dir).expect("create workflows dir");
    fs::write(
        workflows_dir.join("workflows.yaml"),
        r#"
workflows:
  - id: standard
    name: Compiled Standard
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#,
    )
    .expect("write yaml");

    let result = validate_and_compile_yaml_workflows(temp.path()).expect("validate and compile should succeed");
    let compile_result = result.expect("should have result");
    assert_eq!(compile_result.source_files.len(), 1);

    let reloaded = load_workflow_config(temp.path()).expect("reload config");
    let standard = reloaded.workflows.iter().find(|p| p.id == "standard").expect("standard workflow");
    assert_eq!(standard.name, "Compiled Standard");
}

fn make_pipeline(id: &str, phases: Vec<WorkflowPhaseEntry>) -> WorkflowDefinition {
    WorkflowDefinition {
        id: id.to_string(),
        name: id.to_string(),
        description: String::new(),
        phases,
        post_success: None,
        variables: Vec::new(),
    }
}

#[test]
fn expand_basic_sub_pipeline() {
    let workflows = vec![
        make_pipeline(
            "review-cycle",
            vec![WorkflowPhaseEntry::Simple("code-review".into()), WorkflowPhaseEntry::Simple("testing".into())],
        ),
        make_pipeline(
            "standard",
            vec![
                WorkflowPhaseEntry::Simple("requirements".into()),
                WorkflowPhaseEntry::Simple("implementation".into()),
                WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "review-cycle".into() }),
                WorkflowPhaseEntry::Simple("merge".into()),
            ],
        ),
    ];

    let expanded = expand_workflow_phases(&workflows, "standard").expect("should expand");
    let ids: Vec<&str> = expanded.iter().map(|e| e.phase_id()).collect();
    assert_eq!(ids, vec!["requirements", "implementation", "code-review", "testing", "merge"]);
}

#[test]
fn expand_nested_sub_pipelines() {
    let workflows = vec![
        make_pipeline("lint", vec![WorkflowPhaseEntry::Simple("code-review".into())]),
        make_pipeline(
            "review-cycle",
            vec![
                WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "lint".into() }),
                WorkflowPhaseEntry::Simple("testing".into()),
            ],
        ),
        make_pipeline(
            "standard",
            vec![
                WorkflowPhaseEntry::Simple("requirements".into()),
                WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "review-cycle".into() }),
            ],
        ),
    ];

    let expanded = expand_workflow_phases(&workflows, "standard").expect("should expand");
    let ids: Vec<&str> = expanded.iter().map(|e| e.phase_id()).collect();
    assert_eq!(ids, vec!["requirements", "code-review", "testing"]);
}

#[test]
fn collect_workflow_refs_tracks_nested_sub_workflows_once() {
    let workflows = vec![
        make_pipeline("lint", vec![WorkflowPhaseEntry::Simple("code-review".into())]),
        make_pipeline(
            "review-cycle",
            vec![
                WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "lint".into() }),
                WorkflowPhaseEntry::Simple("testing".into()),
            ],
        ),
        make_pipeline(
            "standard",
            vec![
                WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "review-cycle".into() }),
                WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "lint".into() }),
            ],
        ),
    ];

    let refs = collect_workflow_refs(&workflows, "standard").expect("should collect refs");
    assert_eq!(refs, vec!["standard", "review-cycle", "lint"]);
}

#[test]
fn expand_detects_circular_reference() {
    let workflows = vec![
        make_pipeline("a", vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "b".into() })]),
        make_pipeline("b", vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "a".into() })]),
    ];

    let err = expand_workflow_phases(&workflows, "a").expect_err("should detect cycle");
    assert!(
        err.to_string().contains("circular sub-workflow reference"),
        "error should mention circular reference: {}",
        err
    );
}

#[test]
fn expand_detects_self_reference() {
    let workflows = vec![make_pipeline(
        "self-ref",
        vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "self-ref".into() })],
    )];

    let err = expand_workflow_phases(&workflows, "self-ref").expect_err("should detect self-ref");
    assert!(
        err.to_string().contains("circular sub-workflow reference"),
        "error should mention circular reference: {}",
        err
    );
}

#[test]
fn expand_errors_on_missing_pipeline_reference() {
    let workflows = vec![make_pipeline(
        "standard",
        vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "nonexistent".into() })],
    )];

    let err = expand_workflow_phases(&workflows, "standard").expect_err("should error on missing ref");
    assert!(
        err.to_string().contains("sub-workflow 'nonexistent' not found"),
        "error should mention missing workflow_ref: {}",
        err
    );
}

#[test]
fn expand_preserves_rich_phase_config() {
    let mut on_verdict = HashMap::new();
    on_verdict.insert(
        "rework".to_string(),
        PhaseTransitionConfig {
            target: "implementation".to_string(),
            guard: None,
            allow_agent_target: false,
            allowed_targets: Vec::new(),
        },
    );

    let workflows = vec![
        make_pipeline(
            "review",
            vec![WorkflowPhaseEntry::Rich(WorkflowPhaseConfig {
                id: "code-review".into(),
                max_rework_attempts: 3,
                on_verdict: on_verdict.clone(),
                skip_if: vec!["task_type == 'docs'".into()],
            })],
        ),
        make_pipeline(
            "standard",
            vec![
                WorkflowPhaseEntry::Simple("implementation".into()),
                WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "review".into() }),
            ],
        ),
    ];

    let expanded = expand_workflow_phases(&workflows, "standard").expect("should expand");
    assert_eq!(expanded.len(), 2);
    assert_eq!(expanded[1].phase_id(), "code-review");
    let verdicts = expanded[1].on_verdict().expect("should have on_verdict");
    assert_eq!(verdicts["rework"].target, "implementation");
    assert_eq!(expanded[1].skip_if(), &["task_type == 'docs'"]);
}

#[test]
fn serde_deserializes_sub_pipeline_ref() {
    let json = r#"{"workflow_ref": "review-cycle"}"#;
    let entry: WorkflowPhaseEntry = serde_json::from_str(json).expect("deserialize sub-workflow");
    assert!(entry.is_sub_workflow());
    assert_eq!(entry.phase_id(), "review-cycle");
}

#[test]
fn serde_round_trips_sub_pipeline_entry() {
    let entry = WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "review-cycle".into() });
    let json = serde_json::to_string(&entry).expect("serialize");
    let deserialized: WorkflowPhaseEntry = serde_json::from_str(&json).expect("deserialize");
    assert!(deserialized.is_sub_workflow());
    assert_eq!(deserialized.phase_id(), "review-cycle");
}

#[test]
fn serde_deserializes_pipeline_with_mixed_entries() {
    let json = r#"{
        "id": "full",
        "name": "Full Pipeline",
        "description": "",
        "phases": [
            "requirements",
            {"workflow_ref": "review-cycle"},
            {"id": "testing", "skip_if": ["task_type == 'docs'"]},
            "merge"
        ]
    }"#;
    let workflow: WorkflowDefinition = serde_json::from_str(json).expect("deserialize");
    assert_eq!(workflow.phases.len(), 4);
    assert!(!workflow.phases[0].is_sub_workflow());
    assert!(workflow.phases[1].is_sub_workflow());
    assert_eq!(workflow.phases[1].phase_id(), "review-cycle");
    assert!(!workflow.phases[2].is_sub_workflow());
    assert_eq!(workflow.phases[2].phase_id(), "testing");
    assert!(!workflow.phases[3].is_sub_workflow());
}

#[test]
fn yaml_parses_sub_pipeline_ref() {
    let yaml = r#"
workflows:
  - id: review-cycle
    name: Review Cycle
    phases:
      - code-review
      - testing
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - workflow_ref: review-cycle
      - merge
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with sub-workflow");
    let standard = config.workflows.iter().find(|p| p.id == "standard").expect("should have standard workflow");
    assert_eq!(standard.phases.len(), 4);
    assert!(standard.phases[2].is_sub_workflow());
    assert_eq!(standard.phases[2].phase_id(), "review-cycle");
}

#[test]
fn resolve_phase_plan_expands_sub_pipelines() {
    let mut config = builtin_workflow_config();
    config.workflows.push(WorkflowDefinition {
        id: "review-cycle".into(),
        name: "Review Cycle".into(),
        description: String::new(),
        phases: vec![WorkflowPhaseEntry::Simple("code-review".into()), WorkflowPhaseEntry::Simple("testing".into())],
        post_success: None,
        variables: Vec::new(),
    });

    let standard = config.workflows.iter_mut().find(|p| p.id == "standard-workflow").expect("standard workflow");
    standard.phases = vec![
        WorkflowPhaseEntry::Simple("requirements".into()),
        WorkflowPhaseEntry::Simple("implementation".into()),
        WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "review-cycle".into() }),
    ];

    let phases = resolve_workflow_phase_plan(&config, Some("standard-workflow")).expect("should resolve");
    assert_eq!(phases, vec!["requirements", "implementation", "code-review", "testing"]);
}

#[test]
fn validate_rejects_missing_sub_pipeline_reference() {
    let mut config = builtin_workflow_config();
    let standard = config.workflows.iter_mut().find(|p| p.id == "standard-workflow").expect("standard workflow");
    standard.phases = vec![
        WorkflowPhaseEntry::Simple("requirements".into()),
        WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "nonexistent".into() }),
    ];

    let err = validate_workflow_config(&config).expect_err("should reject missing sub-workflow ref");
    let message = err.to_string();
    assert!(
        message.contains("references unknown sub-workflow 'nonexistent'"),
        "error should mention missing sub-workflow: {}",
        message
    );
}

#[test]
fn validate_rejects_empty_post_success_target_branch() {
    let mut config = builtin_workflow_config();
    let standard = config.workflows.iter_mut().find(|p| p.id == "standard-workflow").expect("standard workflow");
    standard.post_success = Some(PostSuccessConfig {
        merge: Some(MergeConfig { target_branch: "".to_string(), ..MergeConfig::default() }),
    });

    let err = validate_workflow_config(&config).expect_err("empty post_success target branch should be rejected");
    let message = err.to_string();
    assert!(
        message.contains("post_success.merge.target_branch must not be empty"),
        "error should mention post_success target branch validation: {}",
        message
    );
}

#[test]
fn validate_rejects_circular_sub_pipeline() {
    let mut config = builtin_workflow_config();
    config.workflows = vec![
        WorkflowDefinition {
            id: "standard".into(),
            name: "Standard".into(),
            description: String::new(),
            phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "review".into() })],
            post_success: None,
            variables: Vec::new(),
        },
        WorkflowDefinition {
            id: "review".into(),
            name: "Review".into(),
            description: String::new(),
            phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "standard".into() })],
            post_success: None,
            variables: Vec::new(),
        },
    ];

    let err = validate_workflow_config(&config).expect_err("should reject circular sub-workflow");
    let message = err.to_string();
    assert!(message.contains("sub-workflow expansion failed"), "error should mention expansion failure: {}", message);
}

#[test]
fn expand_pipeline_not_found_at_top_level() {
    let workflows = vec![make_pipeline("standard", vec![WorkflowPhaseEntry::Simple("requirements".into())])];

    let err = expand_workflow_phases(&workflows, "nonexistent").expect_err("should error on missing workflow");
    assert!(
        err.to_string().contains("sub-workflow 'nonexistent' not found"),
        "error should mention missing workflow_ref: {}",
        err
    );
}

#[test]
fn yaml_parses_command_phase() {
    let yaml = r#"
phases:
  build:
    mode: command
    command:
      program: cargo
      args: ["build", "--release"]
      timeout_secs: 300

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - build
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with command phase");
    assert!(config.phase_definitions.contains_key("build"));
    let build = &config.phase_definitions["build"];
    assert_eq!(build.mode, PhaseExecutionMode::Command);
    let cmd = build.command.as_ref().expect("should have command");
    assert_eq!(cmd.program, "cargo");
    assert_eq!(cmd.args, vec!["build", "--release"]);
    assert_eq!(cmd.timeout_secs, Some(300));
    assert_eq!(cmd.cwd_mode, CommandCwdMode::ProjectRoot);
    assert_eq!(cmd.success_exit_codes, vec![0]);
}

#[test]
fn yaml_parses_manual_phase() {
    let yaml = r#"
phases:
  approval:
    mode: manual
    manual:
      instructions: "Review and approve the deployment plan"
      approval_note_required: true
      timeout_secs: 3600

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - approval
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with manual phase");
    assert!(config.phase_definitions.contains_key("approval"));
    let approval = &config.phase_definitions["approval"];
    assert_eq!(approval.mode, PhaseExecutionMode::Manual);
    let manual = approval.manual.as_ref().expect("should have manual");
    assert_eq!(manual.instructions, "Review and approve the deployment plan");
    assert!(manual.approval_note_required);
    assert_eq!(manual.timeout_secs, Some(3600));
}

#[test]
fn yaml_parses_agent_profile() {
    let yaml = r#"
agents:
  researcher:
    system_prompt: "You are a research agent focused on code analysis"
    model: gemini-3.1-pro-preview
    web_search: true
    skills:
      - deep-search
    capabilities:
      code_execution: false

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with agent profile");
    assert!(config.agent_profiles.contains_key("researcher"));
    let researcher = &config.agent_profiles["researcher"];
    assert_eq!(researcher.system_prompt, "You are a research agent focused on code analysis");
    assert_eq!(researcher.model.as_deref(), Some("gemini-3.1-pro-preview"));
    assert_eq!(researcher.web_search, Some(true));
    assert_eq!(researcher.skills, vec!["deep-search"]);
    assert_eq!(researcher.capabilities.get("code_execution"), Some(&false));
}

#[test]
fn yaml_parses_phase_level_skills() {
    let yaml = r#"
phases:
  research:
    mode: agent
    agent: default
    skills:
      - deep-search
      - code-analysis

workflows:
  - id: standard
    name: Standard
    phases:
      - research
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse YAML with phase skills");
    let research = &config.phase_definitions["research"];
    assert_eq!(research.skills, vec!["deep-search", "code-analysis"]);
}

#[test]
fn yaml_phase_skills_roundtrip_through_overlay_writer() {
    let yaml = r#"
phases:
  research:
    mode: agent
    agent: default
    skills:
      - deep-search

workflows:
  - id: standard
    name: Standard
    phases:
      - research
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let temp = tempfile::tempdir().expect("tempdir");
    super::yaml_compiler::write_workflow_yaml_overlay(temp.path(), "roundtrip.yaml", &config).expect("write overlay");
    let written = fs::read_to_string(super::yaml_compiler::yaml_workflows_dir(temp.path()).join("roundtrip.yaml"))
        .expect("read overlay");
    assert!(written.contains("skills:"), "round-tripped yaml should contain skills: {written}");
    let reparsed = parse_yaml_workflow_config(&written).expect("reparse round-tripped yaml");
    assert_eq!(reparsed.phase_definitions["research"].skills, vec!["deep-search"]);
}

#[test]
fn validate_rejects_unknown_phase_skill_for_project_config() {
    let temp = tempfile::tempdir().expect("tempdir");
    let yaml = r#"
phases:
  research:
    mode: agent
    agent: default
    skills:
      - not-a-real-skill

workflows:
  - id: standard
    name: Standard
    phases:
      - research
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let err = validate_workflow_config_with_project_root(&config, Some(temp.path()))
        .expect_err("unknown phase skill should fail validation");
    assert!(err.to_string().contains("phase_definitions['research'].skills"));
    assert!(err.to_string().contains("not-a-real-skill"));
}

#[test]
fn yaml_auto_registers_command_phase_in_catalog() {
    let yaml = r#"
phases:
  cargo-build:
    mode: command
    command:
      program: cargo
      args: ["build"]

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - cargo-build
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse");
    assert!(config.phase_catalog.contains_key("cargo-build"));
    let catalog_entry = &config.phase_catalog["cargo-build"];
    assert_eq!(catalog_entry.label, "Cargo Build");
    assert_eq!(catalog_entry.category, "build");
}

#[test]
fn yaml_collects_tools_allowlist() {
    let yaml = r#"
tools_allowlist:
  - cargo
  - npm

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse");
    assert!(config.tools_allowlist.contains(&"cargo".to_string()));
    assert!(config.tools_allowlist.contains(&"npm".to_string()));
}

#[test]
fn yaml_parses_unified_config_sections() {
    let yaml = r#"
mcp_servers:
  mcp-go:
    command: "node"
    args: ["server.js"]
    transport: "stdio"
    config:
      endpoint: "stdio://local"
    tools:
      - search
      - shell
    env:
      MCP_TOKEN: "token"
tools:
  cli-gpt:
    executable: "gpt-cli"
    supports_mcp: true
    supports_write: false
    context_window: 64000
    base_args: ["--json"]
integrations:
  tasks:
    provider: github
    config:
      scope: "org"
  git:
    provider: github
    auto_pr: true
    auto_merge: false
    base_branch: "main"
    config:
      organization: "acme"
schedules:
  - id: nightly
    cron: "0 2 * * *"
    workflow_ref: standard
    enabled: true
daemon:
  interval_secs: 300
  max_agents: 2
  active_hours: "00:00-06:00"
  auto_run_ready: true
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse unified config sections");
    let server = config.mcp_servers.get("mcp-go").expect("mcp server should be parsed");
    assert_eq!(server.command, "node");
    assert_eq!(server.args, vec!["server.js"]);
    assert_eq!(server.transport.as_deref(), Some("stdio"));
    assert_eq!(server.tools, vec!["search", "shell"]);
    let tool = config.tools.get("cli-gpt").expect("tool definition should be parsed");
    assert_eq!(tool.executable, "gpt-cli");
    assert!(tool.supports_mcp);
    assert_eq!(tool.context_window, Some(64000));
    assert_eq!(tool.base_args, vec!["--json"]);
    let integrations = config.integrations.as_ref().expect("integrations should be parsed");
    let task_integration = integrations.tasks.as_ref().expect("task integration should be parsed");
    assert_eq!(task_integration.provider, "github");
    let git_integration = integrations.git.as_ref().expect("git integration should be parsed");
    assert_eq!(git_integration.provider, "github");
    assert!(git_integration.auto_pr);
    assert!(!git_integration.auto_merge);
    assert_eq!(git_integration.base_branch.as_deref(), Some("main"));
    assert_eq!(config.schedules.len(), 1);
    assert_eq!(config.schedules[0].id, "nightly");
    assert_eq!(config.schedules[0].cron, "0 2 * * *");
    assert_eq!(config.schedules[0].workflow_ref.as_deref(), Some("standard"));
    assert!(config.schedules[0].enabled);
    let daemon = config.daemon.as_ref().expect("daemon config should be parsed");
    assert_eq!(daemon.interval_secs, Some(300));
    assert_eq!(daemon.pool_size, Some(2));
    assert_eq!(daemon.active_hours.as_deref(), Some("00:00-06:00"));
    assert!(daemon.auto_run_ready);
}

#[test]
fn yaml_merge_overrides_new_sections() {
    let base_yaml = r#"
mcp_servers:
  mcp-go:
    command: "node"
    args: ["server.js"]
    tools: ["search"]

tools:
  cli-gpt:
    executable: "gpt-cli"
    context_window: 32000
    base_args: []

schedules:
  - id: nightly
    cron: "0 2 * * *"
    workflow_ref: standard

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let overlay_yaml = r#"
mcp_servers:
  mcp-go:
    command: "bun"
    args: ["run", "server.js"]
    tools: ["search"]

schedules:
  - id: nightly
    cron: "0 3 * * *"
    workflow_ref: ops
  - id: weekly
    cron: "0 4 * * 0"
    workflow_ref: standard

integrations:
  git:
    provider: github
    auto_pr: true
    base_branch: main
"#;
    let base = parse_yaml_workflow_config(base_yaml).expect("parse base");
    let overlay = parse_yaml_workflow_config(overlay_yaml).expect("parse overlay");
    let merged = merge_yaml_into_config(base, overlay);
    let server = merged.mcp_servers.get("mcp-go").expect("mcp server should be merged");
    assert_eq!(server.command, "bun");
    assert_eq!(merged.schedules.len(), 2);
    let nightly = merged.schedules.iter().find(|schedule| schedule.id == "nightly").expect("nightly should be merged");
    assert_eq!(nightly.cron, "0 3 * * *");
    assert!(merged.integrations.is_some());
    assert_eq!(merged.integrations.unwrap().git.as_ref().and_then(|git| git.base_branch.as_deref()), Some("main"));
}

#[test]
fn yaml_parses_top_level_mcp_servers() {
    let yaml = r#"
mcp_servers:
  ao:
    command: "node"
    args: ["server.js"]
    tools:
      - search

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse MCP servers");
    let server = config.mcp_servers.get("ao").expect("MCP server should be parsed");
    assert_eq!(server.command, "node");
    assert_eq!(server.args, vec!["server.js"]);
    assert_eq!(server.tools, vec!["search"]);
}

#[test]
fn validate_rejects_phase_mcp_binding_unknown_server_reference() {
    let yaml = r#"
mcp_servers:
  ao:
    command: "node"
    args: ["server.js"]
phase_mcp_bindings:
  research:
    servers:
      - missing

workflows:
  - id: standard
    name: Standard
    phases:
      - research
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse");
    let err = validate_workflow_config(&config).expect_err("should reject missing MCP reference");
    assert!(
        err.to_string().contains("phase_mcp_bindings['research'].servers references unknown MCP server 'missing'"),
        "validation error should mention the missing MCP server"
    );
}

#[test]
fn yaml_parses_agent_profile_referencing_top_level_mcp_server() {
    let yaml = r#"
mcp_servers:
  ao:
    command: "node"
    args: ["server.js"]
    tools:
      - search
agents:
  researcher:
    system_prompt: "You are a research agent focused on code analysis"
    mcp_servers:
      - ao

default_workflow_ref: standard
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse");
    let profile = &config.agent_profiles["researcher"];
    assert_eq!(profile.mcp_servers, vec!["ao".to_string()]);
    assert!(validate_workflow_config(&config).is_ok());
}

#[test]
fn validate_rejects_agent_profile_unknown_mcp_server_reference() {
    let yaml = r#"
mcp_servers:
  ao:
    command: "node"
    args: ["server.js"]
    tools:
      - search
agents:
  researcher:
    system_prompt: "You are a research agent focused on code analysis"
    mcp_servers:
      - missing

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse");
    let err = validate_workflow_config(&config).expect_err("should reject missing MCP reference");
    let message = err.to_string();
    assert!(
        message.contains("agent_profiles['researcher'].mcp_servers references unknown MCP server 'missing'"),
        "error should mention unknown MCP server reference: {}",
        message
    );
}

#[test]
fn yaml_accepts_agent_mode_phase() {
    let yaml = r#"
phases:
  research:
    mode: agent
    agent: researcher
    directive: Gather implementation evidence

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
    let config = parse_yaml_workflow_config(yaml).expect("agent phases should parse from workflow YAML");
    let research = config.phase_definitions.get("research").expect("research phase should be defined");
    assert_eq!(research.mode, PhaseExecutionMode::Agent);
    assert_eq!(research.agent_id.as_deref(), Some("researcher"));
    assert_eq!(research.directive.as_deref(), Some("Gather implementation evidence"));
}

#[test]
fn yaml_rejects_missing_command_block() {
    let yaml = r#"
phases:
  build:
    mode: command

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
    let err = parse_yaml_workflow_config(yaml).expect_err("should reject command mode without command block");
    let message = format!("{:#}", err);
    assert!(message.contains("requires a command block"), "error should mention missing command block: {}", message);
}

#[test]
fn yaml_rejects_missing_manual_block() {
    let yaml = r#"
phases:
  approval:
    mode: manual

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
    let err = parse_yaml_workflow_config(yaml).expect_err("should reject manual mode without manual block");
    let message = format!("{:#}", err);
    assert!(message.contains("requires a manual block"), "error should mention missing manual block: {}", message);
}

#[test]
fn yaml_merge_combines_phase_definitions() {
    let base_yaml = r#"
phases:
  build:
    mode: command
    command:
      program: cargo
      args: ["build"]

default_workflow_ref: standard
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - build
      - testing
"#;
    let overlay_yaml = r#"
phases:
  lint:
    mode: command
    command:
      program: cargo
      args: ["clippy"]
"#;
    let base = parse_yaml_workflow_config(base_yaml).expect("parse base");
    let overlay = parse_yaml_workflow_config(overlay_yaml).expect("parse overlay");
    let merged = merge_yaml_into_config(base, overlay);
    assert!(merged.phase_definitions.contains_key("build"));
    assert!(merged.phase_definitions.contains_key("lint"));
}

#[test]
fn yaml_merge_combines_agent_profiles() {
    let base_yaml = r#"
agents:
  researcher:
    system_prompt: "Research agent"
    model: gemini-3.1-pro-preview

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - testing
"#;
    let overlay_yaml = r#"
agents:
  implementer:
    system_prompt: "Implementation agent"
    model: claude-sonnet-4-6
"#;
    let base = parse_yaml_workflow_config(base_yaml).expect("parse base");
    let overlay = parse_yaml_workflow_config(overlay_yaml).expect("parse overlay");
    let merged = merge_yaml_into_config(base, overlay);
    assert!(merged.agent_profiles.contains_key("researcher"));
    assert!(merged.agent_profiles.contains_key("implementer"));
}

#[test]
fn yaml_merge_deduplicates_tools_allowlist() {
    let base_yaml = r#"
tools_allowlist:
  - cargo
  - npm

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
    let overlay_yaml = r#"
tools_allowlist:
  - cargo
  - python
"#;
    let base = parse_yaml_workflow_config(base_yaml).expect("parse base");
    let overlay = parse_yaml_workflow_config(overlay_yaml).expect("parse overlay");
    let merged = merge_yaml_into_config(base, overlay);
    assert!(merged.tools_allowlist.contains(&"cargo".to_string()));
    assert!(merged.tools_allowlist.contains(&"npm".to_string()));
    assert!(merged.tools_allowlist.contains(&"python".to_string()));
    let cargo_count = merged.tools_allowlist.iter().filter(|t| *t == "cargo").count();
    assert_eq!(cargo_count, 1, "cargo should appear only once after merge");
}

#[test]
fn cross_validation_accepts_workflow_defined_phases() {
    let yaml = r#"
phases:
  build:
    mode: command
    command:
      program: cargo
      args: ["build"]

default_workflow_ref: standard
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - build
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let runtime = crate::agent_runtime_config::builtin_agent_runtime_config();
    let result = validate_workflow_and_runtime_configs(&config, &runtime);
    assert!(result.is_ok(), "cross-validation should pass for workflow-defined phase: {:?}", result.err());
}

fn write_global_claude_profile_config(config_dir: &std::path::Path, profile_name: &str, config_dir_value: &str) {
    let mut config = protocol::Config::load_from_dir(config_dir).expect("global config should load");
    config.claude_profiles.insert(
        profile_name.to_string(),
        protocol::ClaudeProfileEntry {
            env: BTreeMap::from([("CLAUDE_CONFIG_DIR".to_string(), config_dir_value.to_string())]),
        },
    );
    let config_path = config_dir.join("config.json");
    std::fs::write(config_path, serde_json::to_string_pretty(&config).expect("serialize config"))
        .expect("write global config");
}

#[test]
fn cross_validation_accepts_known_claude_tool_profile() {
    let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let _config_dir = EnvVarGuard::set("AO_CONFIG_DIR", temp.path());
    write_global_claude_profile_config(temp.path(), "overflow", "/Users/test/.claude-overflow");

    let yaml = r#"
agents:
  default:
    tool: claude
    model: claude-sonnet-4-6
    tool_profile: overflow

default_workflow_ref: standard
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let runtime = crate::agent_runtime_config::builtin_agent_runtime_config();
    let result = validate_workflow_and_runtime_configs_with_project_root(&config, &runtime, Some(temp.path()));
    assert!(result.is_ok(), "known Claude profile should validate: {:?}", result.err());
}

#[test]
fn cross_validation_rejects_non_claude_tool_profile_usage() {
    let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let _config_dir = EnvVarGuard::set("AO_CONFIG_DIR", temp.path());
    write_global_claude_profile_config(temp.path(), "overflow", "/Users/test/.claude-overflow");

    let yaml = r#"
agents:
  default:
    tool: codex
    model: gpt-5.4
    tool_profile: overflow

default_workflow_ref: standard
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let runtime = crate::agent_runtime_config::builtin_agent_runtime_config();
    let err = validate_workflow_and_runtime_configs_with_project_root(&config, &runtime, Some(temp.path()))
        .expect_err("non-Claude tool_profile usage should fail");
    assert!(err.to_string().contains("only supported when the effective tool is claude"));
}

#[test]
fn validate_rejects_command_program_not_in_allowlist() {
    let mut config = builtin_workflow_config();
    config.tools_allowlist = vec!["npm".to_string()];
    config.phase_definitions.insert(
        "build".to_string(),
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
            agent_id: None,
            directive: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: Some(PhaseCommandDefinition {
                program: "cargo".to_string(),
                args: vec!["build".to_string()],
                env: BTreeMap::new(),
                cwd_mode: CommandCwdMode::ProjectRoot,
                cwd_path: None,
                timeout_secs: None,
                success_exit_codes: vec![0],
                parse_json_output: false,
                expected_result_kind: None,
                expected_schema: None,
                category: None,
                failure_pattern: None,
                excerpt_max_chars: None,
                on_success_verdict: None,
                on_failure_verdict: None,
                confidence: None,
                failure_risk: None,
            }),
            manual: None,
            system_prompt: None,
            default_tool: None,
        },
    );
    let err = validate_workflow_config(&config).expect_err("should reject program not in allowlist");
    let message = err.to_string();
    assert!(message.contains("not in tools_allowlist"), "error should mention allowlist: {}", message);
}

#[test]
fn validate_rejects_invalid_unified_sections() {
    let mut config = builtin_workflow_config();
    config.schedules.push(WorkflowSchedule {
        id: "nightly".to_string(),
        cron: "".to_string(),
        workflow_ref: None,
        command: None,
        enabled: true,
        input: None,
    });
    config.tools.insert(
        "cli-gpt".to_string(),
        ToolDefinition {
            executable: "".to_string(),
            supports_mcp: true,
            supports_write: false,
            context_window: Some(0),
            base_args: vec!["".to_string()],
            supports_streaming: None,
            supports_tool_use: None,
            supports_vision: None,
            supports_long_context: None,
            read_only_flag: None,
            response_schema_flag: None,
        },
    );
    config.mcp_servers.insert(
        "example".to_string(),
        McpServerDefinition {
            command: "".to_string(),
            args: vec!["".to_string()],
            transport: Some(" ".to_string()),
            url: None,
            config: BTreeMap::new(),
            tools: vec!["".to_string()],
            env: BTreeMap::from([("".to_string(), "value".to_string())]),
        },
    );
    let err = validate_workflow_config(&config).expect_err("invalid unified config should fail");
    let message = err.to_string();
    assert!(
        message.contains("schedules['nightly'] must define workflow_ref"),
        "error should mention missing schedule target: {}",
        message
    );
    assert!(
        message.contains("schedules['nightly'].cron must not be empty"),
        "error should mention empty schedule cron: {}",
        message
    );
    assert!(
        message.contains("tools['cli-gpt'].executable must not be empty"),
        "error should mention invalid tool executable: {}",
        message
    );
    assert!(
        message.contains("tools['cli-gpt'].context_window must be greater than 0 when set"),
        "error should mention tool context window: {}",
        message
    );
    assert!(
        message.contains("tools['cli-gpt'].base_args must not contain empty values"),
        "error should mention tool args: {}",
        message
    );
    assert!(
        message.contains("mcp_servers['example'].command must not be empty"),
        "error should mention MCP command: {}",
        message
    );
}

#[test]
fn validate_rejects_schedule_with_command() {
    let mut config = builtin_workflow_config();
    config.schedules.push(WorkflowSchedule {
        id: "conflicting-schedule".to_string(),
        cron: "0 * * * *".to_string(),
        workflow_ref: Some("standard".to_string()),
        command: Some("echo conflict".to_string()),
        input: None,
        enabled: true,
    });
    let err =
        validate_workflow_config(&config).expect_err("schedules defining both workflow and command should be rejected");
    let message = err.to_string();
    assert!(
        message.contains("command is no longer supported; use workflow_ref"),
        "error should mention unsupported schedule command: {}",
        message
    );
}

#[test]
fn validate_rejects_invalid_cron_expression() {
    let mut config = builtin_workflow_config();
    config.schedules.push(WorkflowSchedule {
        id: "bad-cron".to_string(),
        cron: "0 0 0".to_string(),
        workflow_ref: Some("standard".to_string()),
        command: None,
        input: None,
        enabled: true,
    });
    let err = validate_workflow_config(&config).expect_err("schedules with malformed cron should fail validation");
    let message = err.to_string();
    assert!(
        message.contains("schedules['bad-cron'].cron is not valid"),
        "error should mention invalid cron expression: {}",
        message
    );
}

#[test]
fn workflow_schedule_input_defaults_to_none_and_enabled_defaults_to_true() {
    let yaml = r#"
schedules:
  - id: nightly
    cron: "0 2 * * *"
    workflow_ref: "standard"

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
      - implementation
      - testing
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse");
    let schedule = &config.schedules[0];
    assert!(schedule.enabled);
    assert!(schedule.input.is_none());
}

#[test]
fn yaml_agent_profile_with_all_fields_deserializes() {
    let yaml = r#"
agents:
  full-agent:
    description: "A fully configured agent"
    system_prompt: "You are a specialized agent"
    role: "researcher"
    tool: claude
    model: claude-sonnet-4-6
    fallback_models:
      - claude-haiku-4-5
    reasoning_effort: high
    web_search: true
    network_access: false
    timeout_secs: 600
    max_attempts: 3
    skills:
      - deep-search
      - code-analysis
    capabilities:
      code_execution: true
      file_write: false
    tool_policy:
      allow:
        - Read
        - Grep
      deny:
        - Write

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse full agent profile");
    let agent = &config.agent_profiles["full-agent"];
    assert_eq!(agent.description, "A fully configured agent");
    assert_eq!(agent.system_prompt, "You are a specialized agent");
    assert_eq!(agent.role.as_deref(), Some("researcher"));
    assert_eq!(agent.tool.as_deref(), Some("claude"));
    assert_eq!(agent.model.as_deref(), Some("claude-sonnet-4-6"));
    assert_eq!(agent.fallback_models, vec!["claude-haiku-4-5"]);
    assert_eq!(agent.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(agent.web_search, Some(true));
    assert_eq!(agent.network_access, Some(false));
    assert_eq!(agent.timeout_secs, Some(600));
    assert_eq!(agent.max_attempts, Some(3));
    assert_eq!(agent.skills, vec!["deep-search", "code-analysis"]);
    assert_eq!(agent.capabilities.get("code_execution"), Some(&true));
    assert_eq!(agent.capabilities.get("file_write"), Some(&false));
    assert_eq!(agent.tool_policy.allow, vec!["Read", "Grep"]);
    assert_eq!(agent.tool_policy.deny, vec!["Write"]);
}

#[test]
fn yaml_command_phase_with_all_options() {
    let yaml = r#"
phases:
  custom-build:
    mode: command
    directive: "Build with custom settings"
    command:
      program: make
      args: ["all", "-j4"]
      env:
        CC: gcc
        CFLAGS: "-O2"
      cwd_mode: task_root
      timeout_secs: 600
      success_exit_codes: [0, 2]
      parse_json_output: true

workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse");
    let phase = &config.phase_definitions["custom-build"];
    assert_eq!(phase.directive.as_deref(), Some("Build with custom settings"));
    let cmd = phase.command.as_ref().expect("command");
    assert_eq!(cmd.program, "make");
    assert_eq!(cmd.args, vec!["all", "-j4"]);
    assert_eq!(cmd.env.get("CC"), Some(&"gcc".to_string()));
    assert_eq!(cmd.cwd_mode, CommandCwdMode::TaskRoot);
    assert_eq!(cmd.timeout_secs, Some(600));
    assert_eq!(cmd.success_exit_codes, vec![0, 2]);
    assert!(cmd.parse_json_output);
}

#[test]
fn existing_configs_without_new_fields_deserialize() {
    let json = serde_json::json!({
        "schema": WORKFLOW_CONFIG_SCHEMA_ID,
        "version": WORKFLOW_CONFIG_VERSION,
        "default_workflow_ref": "standard",
        "phase_catalog": {
            "requirements": {
                "label": "Requirements",
                "description": "",
                "category": "planning",
                "visible": true,
                "tags": []
            }
        },
        "workflows": [{
            "id": "standard",
            "name": "Standard",
            "description": "",
            "phases": ["requirements"]
        }]
    });
    let config: WorkflowConfig = serde_json::from_value(json).expect("should deserialize without new fields");
    assert!(config.phase_definitions.is_empty());
    assert!(config.agent_profiles.is_empty());
    assert!(config.tools_allowlist.is_empty());
    assert!(config.mcp_servers.is_empty());
    assert!(config.tools.is_empty());
    assert!(config.schedules.is_empty());
    assert!(config.integrations.is_none());
    assert!(config.daemon.is_none());
}

#[test]
fn new_fields_skip_serializing_when_empty() {
    let config = builtin_workflow_config_base();
    let json = serde_json::to_value(&config).expect("serialize");
    let obj = json.as_object().expect("should be object");
    assert!(!obj.contains_key("phase_definitions"), "empty phase_definitions should not be serialized");
    assert!(!obj.contains_key("agent_profiles"), "empty agent_profiles should not be serialized");
    assert!(!obj.contains_key("tools_allowlist"), "empty tools_allowlist should not be serialized");
    assert!(obj.contains_key("mcp_servers"), "builtin mcp_servers should be serialized when present");
    assert!(!obj.contains_key("tools"), "empty tools should not be serialized");
    assert!(!obj.contains_key("schedules"), "empty schedules should not be serialized");
    assert!(!obj.contains_key("integrations"), "empty integrations should not be serialized");
    assert!(!obj.contains_key("daemon"), "empty daemon should not be serialized");
}

#[test]
fn pipeline_variables_parse_from_yaml() {
    let yaml = r#"
workflows:
  - id: docs
    name: Documentation
    variables:
      - name: AUDIENCE
        description: Target audience
        required: true
      - name: FORMAT
        default: markdown
    phases:
      - implementation
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let workflow = config.workflows.iter().find(|p| p.id == "docs").expect("docs workflow");
    assert_eq!(workflow.variables.len(), 2);
    assert_eq!(workflow.variables[0].name, "AUDIENCE");
    assert_eq!(workflow.variables[0].description.as_deref(), Some("Target audience"));
    assert!(workflow.variables[0].required);
    assert!(workflow.variables[0].default.is_none());
    assert_eq!(workflow.variables[1].name, "FORMAT");
    assert!(!workflow.variables[1].required);
    assert_eq!(workflow.variables[1].default.as_deref(), Some("markdown"));
}

#[test]
fn pipeline_variables_parse_from_json() {
    let json = serde_json::json!({
        "id": "docs",
        "name": "Documentation",
        "phases": ["implementation"],
        "variables": [
            { "name": "AUDIENCE", "required": true, "description": "Target audience" },
            { "name": "FORMAT", "default": "markdown" }
        ]
    });
    let workflow: WorkflowDefinition = serde_json::from_value(json).expect("parse json");
    assert_eq!(workflow.variables.len(), 2);
    assert_eq!(workflow.variables[0].name, "AUDIENCE");
    assert!(workflow.variables[0].required);
    assert_eq!(workflow.variables[1].name, "FORMAT");
    assert_eq!(workflow.variables[1].default.as_deref(), Some("markdown"));
}

#[test]
fn pipeline_variables_empty_when_omitted() {
    let json = serde_json::json!({
        "id": "simple",
        "name": "Simple",
        "phases": ["implementation"]
    });
    let workflow: WorkflowDefinition = serde_json::from_value(json).expect("parse json");
    assert!(workflow.variables.is_empty());
}

#[test]
fn resolve_variables_required_without_default_errors() {
    let definitions =
        vec![WorkflowVariable { name: "REQUIRED_VAR".to_string(), description: None, required: true, default: None }];
    let cli_vars = HashMap::new();
    let err = resolve_workflow_variables(&definitions, &cli_vars).expect_err("should error on missing required var");
    assert!(err.to_string().contains("REQUIRED_VAR"));
}

#[test]
fn resolve_variables_required_multiple_missing() {
    let definitions = vec![
        WorkflowVariable { name: "VAR_B".to_string(), description: None, required: true, default: None },
        WorkflowVariable { name: "VAR_A".to_string(), description: None, required: true, default: None },
    ];
    let cli_vars = HashMap::new();
    let err = resolve_workflow_variables(&definitions, &cli_vars).expect_err("should error on missing required vars");
    let msg = err.to_string();
    assert!(msg.contains("VAR_A"));
    assert!(msg.contains("VAR_B"));
}

#[test]
fn resolve_variables_default_used_when_not_provided() {
    let definitions = vec![WorkflowVariable {
        name: "FORMAT".to_string(),
        description: None,
        required: false,
        default: Some("markdown".to_string()),
    }];
    let cli_vars = HashMap::new();
    let resolved = resolve_workflow_variables(&definitions, &cli_vars).expect("should resolve");
    assert_eq!(resolved.get("FORMAT").map(String::as_str), Some("markdown"));
}

#[test]
fn resolve_variables_cli_overrides_default() {
    let definitions = vec![WorkflowVariable {
        name: "FORMAT".to_string(),
        description: None,
        required: false,
        default: Some("markdown".to_string()),
    }];
    let mut cli_vars = HashMap::new();
    cli_vars.insert("FORMAT".to_string(), "html".to_string());
    let resolved = resolve_workflow_variables(&definitions, &cli_vars).expect("should resolve");
    assert_eq!(resolved.get("FORMAT").map(String::as_str), Some("html"));
}

#[test]
fn resolve_variables_optional_without_default_omitted() {
    let definitions =
        vec![WorkflowVariable { name: "OPTIONAL".to_string(), description: None, required: false, default: None }];
    let cli_vars = HashMap::new();
    let resolved = resolve_workflow_variables(&definitions, &cli_vars).expect("should resolve");
    assert!(!resolved.contains_key("OPTIONAL"));
}

#[test]
fn resolve_variables_unknown_cli_vars_ignored() {
    let definitions =
        vec![WorkflowVariable { name: "KNOWN".to_string(), description: None, required: true, default: None }];
    let mut cli_vars = HashMap::new();
    cli_vars.insert("KNOWN".to_string(), "value".to_string());
    cli_vars.insert("UNKNOWN".to_string(), "extra".to_string());
    let resolved = resolve_workflow_variables(&definitions, &cli_vars).expect("should resolve");
    assert_eq!(resolved.get("KNOWN").map(String::as_str), Some("value"));
}

#[test]
fn expand_variables_replaces_patterns() {
    let mut vars = HashMap::new();
    vars.insert("AUDIENCE".to_string(), "developers".to_string());
    vars.insert("FORMAT".to_string(), "markdown".to_string());
    let text = "Write for {{AUDIENCE}} in {{FORMAT}} format.";
    let result = expand_variables(text, &vars);
    assert_eq!(result, "Write for developers in markdown format.");
}

#[test]
fn expand_variables_leaves_unknown_patterns() {
    let vars = HashMap::new();
    let text = "Hello {{UNKNOWN}} world";
    let result = expand_variables(text, &vars);
    assert_eq!(result, "Hello {{UNKNOWN}} world");
}

#[test]
fn expand_variables_empty_vars_noop() {
    let vars = HashMap::new();
    let text = "No variables here";
    let result = expand_variables(text, &vars);
    assert_eq!(result, "No variables here");
}

#[test]
fn pipeline_variables_not_serialized_when_empty() {
    let workflow = WorkflowDefinition {
        id: "test".to_string(),
        name: "Test".to_string(),
        description: String::new(),
        phases: Vec::new(),
        post_success: None,
        variables: Vec::new(),
    };
    let json = serde_json::to_value(&workflow).expect("serialize");
    let obj = json.as_object().expect("json object");
    assert!(!obj.contains_key("variables"), "empty variables should not be serialized");
}

#[test]
fn repo_requirements_yaml_parses_requirement_workflows() {
    let yaml = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.ao/workflows/requirements.yaml"));

    let config = parse_yaml_workflow_config(yaml).expect("requirements workflow yaml should parse");
    let workflow_ids = config.workflows.iter().map(|workflow| workflow.id.as_str()).collect::<Vec<_>>();

    assert!(workflow_ids.contains(&"req-dispatch"));
    assert!(workflow_ids.contains(&"req-refine"));
    assert!(workflow_ids.contains(&"req-review"));
}
