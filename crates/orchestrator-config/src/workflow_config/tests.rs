use std::collections::{BTreeMap, HashMap};
use std::fs;

use crate::agent_runtime_config::{CommandCwdMode, Idempotency, PhaseCommandDefinition, PhaseExecutionMode};
use crate::test_support::{env_lock, EnvVarGuard};
use crate::PhaseExecutionDefinition;

use super::builtins::{builtin_workflow_config, builtin_workflow_config_base};
use super::loading::load_workflow_config;
use super::resolution::{resolve_workflow_phase_plan, resolve_workflow_rework_attempts, resolve_workflow_skip_guards};
use super::types::*;
use super::validation::{
    validate_workflow_and_runtime_configs, validate_workflow_and_runtime_configs_with_project_root,
    validate_workflow_config,
};
use super::yaml_compiler::{compile_yaml_workflow_files, merge_yaml_into_config, validate_and_compile_yaml_workflows};
use super::yaml_parser::{parse_yaml_workflow_config, parse_yaml_workflow_config_with_base_and_source};

fn test_workflow_config_with_standard_pipeline() -> WorkflowConfig {
    let mut config = builtin_workflow_config();
    config.default_workflow_ref = "standard-workflow".to_string();
    config.workflows = vec![
        WorkflowDefinition {
            id: "standard-workflow".to_string(),
            name: "Standard Workflow".to_string(),
            description: "Test fixture pipeline.".to_string(),
            phases: vec![
                "requirements".to_string().into(),
                "implementation".to_string().into(),
                "code-review".to_string().into(),
                "testing".to_string().into(),
            ],
            post_success: Some(PostSuccessConfig {
                merge: Some(MergeConfig {
                    strategy: MergeStrategy::Merge,
                    target_branch: "main".to_string(),
                    create_pr: true,
                    auto_merge: false,
                    cleanup_worktree: true,
                }),
            }),
            variables: Vec::new(),
            worktree: None,
            budget: None,
        },
        WorkflowDefinition {
            id: "ui-ux-standard".to_string(),
            name: "UI UX Standard".to_string(),
            description: "Test fixture frontend pipeline.".to_string(),
            phases: vec![
                "requirements".to_string().into(),
                "ux-research".to_string().into(),
                "wireframe".to_string().into(),
                "mockup-review".to_string().into(),
                "implementation".to_string().into(),
                "code-review".to_string().into(),
                "testing".to_string().into(),
            ],
            post_success: Some(PostSuccessConfig {
                merge: Some(MergeConfig {
                    strategy: MergeStrategy::Merge,
                    target_branch: "main".to_string(),
                    create_pr: true,
                    auto_merge: false,
                    cleanup_worktree: true,
                }),
            }),
            variables: Vec::new(),
            worktree: None,
            budget: None,
        },
    ];
    config
}

#[test]
fn builtin_workflow_config_is_valid() {
    let config = builtin_workflow_config();
    validate_workflow_config(&config).expect("builtin config should validate");
}

#[test]
fn missing_v2_file_reports_actionable_error() {
    let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let _home_guard = crate::test_support::EnvVarGuard::set("HOME", temp.path());
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
    let mut config = test_workflow_config_with_standard_pipeline();
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
        budget: None,
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
    let mut config = test_workflow_config_with_standard_pipeline();
    let standard_pipeline =
        config.workflows.iter_mut().find(|p| p.id == "standard-workflow").expect("standard workflow");

    standard_pipeline.phases[1] = WorkflowPhaseEntry::Rich(WorkflowPhaseConfig {
        id: "implementation".to_string(),
        max_rework_attempts: 0,
        on_verdict: HashMap::new(),
        skip_if: Vec::new(),
        budget: None,
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
    let mut config = test_workflow_config_with_standard_pipeline();
    let standard_pipeline =
        config.workflows.iter_mut().find(|p| p.id == "standard-workflow").expect("standard workflow");
    standard_pipeline.phases = vec![
        "requirements".to_string().into(),
        WorkflowPhaseEntry::Rich(WorkflowPhaseConfig {
            id: "testing".to_string(),
            max_rework_attempts: 3,
            on_verdict: HashMap::new(),
            skip_if: vec!["task_type == 'docs'".to_string()],
            budget: None,
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
    let base = test_workflow_config_with_standard_pipeline();
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
    assert!(
        err.starts_with("error: ") || err.contains("invalid type"),
        "error should be a rustc-style YAML diagnostic: {}",
        err
    );
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
    let workflows_dir = temp.path().join(".animus").join("workflows");
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
    let ao_dir = temp.path().join(".animus");
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
    let skills_dir = temp.path().join(".animus").join("config").join("skill_definitions");
    fs::create_dir_all(&skills_dir).expect("create project skills dir");
    fs::write(
        skills_dir.join("project-skill.yaml"),
        r#"
name: project-skill
description: Project local validation fixture
"#,
    )
    .expect("write project skill");

    let ao_dir = temp.path().join(".animus");
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
        config
            .agent_profiles
            .get("project-agent")
            .is_some_and(|profile| profile.skills.as_deref().unwrap_or_default() == ["project-skill"]),
        "project-local skill reference should remain intact"
    );
}

#[test]
fn yaml_compile_resolves_project_markdown_skills() {
    let temp = tempfile::tempdir().expect("tempdir");
    let skills_dir = temp.path().join(".animus").join("skills").join("project-markdown-skill");
    fs::create_dir_all(&skills_dir).expect("create project markdown skills dir");
    fs::write(
        skills_dir.join("SKILL.md"),
        r#"---
name: project-markdown-skill
description: Project markdown validation fixture
---

# Project Markdown Skill

Use this when reviewing Rust changes.
"#,
    )
    .expect("write project markdown skill");

    let ao_dir = temp.path().join(".animus");
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
      - project-markdown-skill
workflows:
  - id: project-markdown-skill-test
    name: Project Markdown Skill Test
    phases:
      - project-phase
"#,
    )
    .expect("write workflow yaml");

    let result = compile_yaml_workflow_files(temp.path()).expect("compile should succeed");
    let config = result.expect("should have config");
    assert!(
        config
            .agent_profiles
            .get("project-agent")
            .is_some_and(|profile| profile.skills.as_deref().unwrap_or_default() == ["project-markdown-skill"]),
        "project markdown skill reference should remain intact"
    );
}

#[test]
fn validate_and_compile_yaml_validates_and_reloads() {
    let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");

    let workflows_dir = temp.path().join(".animus").join("workflows");
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
        worktree: None,
        budget: None,
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
                budget: None,
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
    let mut config = test_workflow_config_with_standard_pipeline();
    config.workflows.push(WorkflowDefinition {
        id: "review-cycle".into(),
        name: "Review Cycle".into(),
        description: String::new(),
        phases: vec![WorkflowPhaseEntry::Simple("code-review".into()), WorkflowPhaseEntry::Simple("testing".into())],
        post_success: None,
        variables: Vec::new(),
        worktree: None,
        budget: None,
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
    let mut config = test_workflow_config_with_standard_pipeline();
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
    let mut config = test_workflow_config_with_standard_pipeline();
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
            worktree: None,
            budget: None,
        },
        WorkflowDefinition {
            id: "review".into(),
            name: "Review".into(),
            description: String::new(),
            phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "standard".into() })],
            post_success: None,
            variables: Vec::new(),
            worktree: None,
            budget: None,
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
    assert_eq!(researcher.system_prompt.as_deref(), Some("You are a research agent focused on code analysis"));
    assert_eq!(researcher.model.as_deref(), Some("gemini-3.1-pro-preview"));
    assert_eq!(researcher.web_search, Some(true));
    assert_eq!(researcher.skills.clone().unwrap_or_default(), vec!["deep-search"]);
    assert_eq!(researcher.capabilities.clone().unwrap_or_default().get("code_execution"), Some(&false));
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
    assert_eq!(tool.supports_mcp, Some(true));
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
    assert_eq!(profile.mcp_servers.clone().unwrap_or_default(), vec!["ao".to_string()]);
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
tools_allowlist: ["cargo"]
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
    let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path());
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
    let _config_dir = EnvVarGuard::set("ANIMUS_CONFIG_DIR", temp.path());
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
            idempotency: Idempotency::Unknown,
            worktree: None,
            evals: None,
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
            supports_mcp: Some(true),
            supports_write: Some(false),
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
            oauth: None,
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
    assert_eq!(agent.description.as_deref(), Some("A fully configured agent"));
    assert_eq!(agent.system_prompt.as_deref(), Some("You are a specialized agent"));
    assert_eq!(agent.role.as_deref(), Some("researcher"));
    assert_eq!(agent.tool.as_deref(), Some("claude"));
    assert_eq!(agent.model.as_deref(), Some("claude-sonnet-4-6"));
    assert_eq!(agent.fallback_models.clone().unwrap_or_default(), vec!["claude-haiku-4-5"]);
    assert_eq!(agent.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(agent.web_search, Some(true));
    assert_eq!(agent.network_access, Some(false));
    assert_eq!(agent.timeout_secs, Some(600));
    assert_eq!(agent.max_attempts, Some(3));
    assert_eq!(agent.skills.clone().unwrap_or_default(), vec!["deep-search", "code-analysis"]);
    assert_eq!(agent.capabilities.clone().unwrap_or_default().get("code_execution"), Some(&true));
    assert_eq!(agent.capabilities.clone().unwrap_or_default().get("file_write"), Some(&false));
    assert_eq!(agent.tool_policy.clone().unwrap_or_default().allow, vec!["Read", "Grep"]);
    assert_eq!(agent.tool_policy.clone().unwrap_or_default().deny, vec!["Write"]);
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
fn expand_variables_does_not_rescan_substituted_values() {
    let mut vars = HashMap::new();
    vars.insert("A".to_string(), "uses {{B}} inside".to_string());
    vars.insert("B".to_string(), "b-value".to_string());
    let text = "first {{A}} then {{B}}";
    let expected = "first uses {{B}} inside then b-value";
    for _ in 0..16 {
        assert_eq!(expand_variables(text, &vars), expected, "expansion must be deterministic and non-recursive");
    }
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
        worktree: None,
        budget: None,
    };
    let json = serde_json::to_value(&workflow).expect("serialize");
    let obj = json.as_object().expect("json object");
    assert!(!obj.contains_key("variables"), "empty variables should not be serialized");
}

#[test]
fn repo_requirements_yaml_parses_requirement_workflows() {
    let yaml = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.animus/workflows/requirements.yaml"));

    let config = parse_yaml_workflow_config(yaml).expect("requirements workflow yaml should parse");
    let workflow_ids = config.workflows.iter().map(|workflow| workflow.id.as_str()).collect::<Vec<_>>();

    assert!(workflow_ids.contains(&"req-dispatch"));
    assert!(workflow_ids.contains(&"req-refine"));
    assert!(workflow_ids.contains(&"req-review"));
}

#[test]
fn yaml_compile_mixes_inline_and_file_based_system_prompts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workflows_dir = temp.path().join(".animus").join("workflows");
    fs::create_dir_all(&workflows_dir).expect("create workflows dir");

    let prompts_dir = workflows_dir.join("prompts");
    fs::create_dir_all(&prompts_dir).expect("create prompts dir");
    let researcher_prompt = "Researcher prompt from file.\nCite sources.\n";
    fs::write(prompts_dir.join("researcher.md"), researcher_prompt).expect("write prompt");

    fs::write(
        workflows_dir.join("agents.yaml"),
        r#"
agents:
  implementer:
    description: "Implementer"
    system_prompt: "You are the implementer."
  researcher:
    description: "Researcher"
    system_prompt_file: prompts/researcher.md

phases:
  research:
    mode: agent
    agent: researcher
    directive: "Research."
  implement:
    mode: agent
    agent: implementer
    directive: "Implement."

workflows:
  - id: standard
    name: Standard
    phases:
      - research
      - implement
"#,
    )
    .expect("write yaml");

    let result = compile_yaml_workflow_files(temp.path()).expect("compile should succeed");
    let config = result.expect("should have config");

    let implementer = config.agent_profiles.get("implementer").expect("implementer agent");
    assert_eq!(implementer.system_prompt.as_deref(), Some("You are the implementer."));
    assert!(implementer.system_prompt_file.is_none());

    let researcher = config.agent_profiles.get("researcher").expect("researcher agent");
    assert_eq!(researcher.system_prompt.as_deref(), Some(researcher_prompt));
    assert!(researcher.system_prompt_file.is_none(), "field should be consumed at compile time");
}

#[test]
fn yaml_parses_http_mcp_server() {
    let yaml = r#"
mcp_servers:
  robinhood-trading:
    transport: "http"
    url: "https://agent.robinhood.com/mcp/trading"
"#;
    let config = parse_yaml_workflow_config(yaml).expect("http mcp server should parse");
    let server = config.mcp_servers.get("robinhood-trading").expect("server should exist");
    assert_eq!(server.transport.as_deref(), Some("http"));
    assert_eq!(server.url.as_deref(), Some("https://agent.robinhood.com/mcp/trading"));
    assert!(server.command.is_empty());
    assert!(server.args.is_empty());
}

#[test]
fn validation_accepts_http_mcp_server() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "robinhood-trading".to_string(),
        McpServerDefinition {
            command: String::new(),
            args: Vec::new(),
            transport: Some("http".to_string()),
            url: Some("https://agent.robinhood.com/mcp/trading".to_string()),
            config: BTreeMap::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            oauth: None,
        },
    );
    validate_workflow_config(&config).expect("valid http mcp server should pass validation");
}

#[test]
fn validation_rejects_http_without_url() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "missing-url".to_string(),
        McpServerDefinition {
            command: String::new(),
            args: Vec::new(),
            transport: Some("http".to_string()),
            url: None,
            config: BTreeMap::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            oauth: None,
        },
    );
    let err = validate_workflow_config(&config).expect_err("missing url should fail");
    let message = err.to_string();
    assert!(
        message.contains("mcp_servers['missing-url'].url is required when transport is \"http\""),
        "error should mention missing url: {}",
        message
    );
}

#[test]
fn validation_rejects_http_with_command() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "mixed".to_string(),
        McpServerDefinition {
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            transport: Some("http".to_string()),
            url: Some("https://example.com/mcp".to_string()),
            config: BTreeMap::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            oauth: None,
        },
    );
    let err = validate_workflow_config(&config).expect_err("mutual exclusion violation should fail");
    let message = err.to_string();
    assert!(
        message.contains("mcp_servers['mixed'].command must not be set when transport is \"http\""),
        "error should mention command not allowed with http: {}",
        message
    );
    assert!(
        message.contains("mcp_servers['mixed'].args must not be set when transport is \"http\""),
        "error should mention args not allowed with http: {}",
        message
    );
}

#[test]
fn validation_rejects_stdio_with_url() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "mixed-stdio".to_string(),
        McpServerDefinition {
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            transport: Some("stdio".to_string()),
            url: Some("https://example.com/mcp".to_string()),
            config: BTreeMap::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            oauth: None,
        },
    );
    let err = validate_workflow_config(&config).expect_err("stdio + url should fail");
    let message = err.to_string();
    assert!(
        message.contains("mcp_servers['mixed-stdio'].url must not be set when transport is \"stdio\""),
        "error should mention url not allowed with stdio: {}",
        message
    );
}

#[test]
fn validation_rejects_http_with_invalid_url_scheme() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "bad-scheme".to_string(),
        McpServerDefinition {
            command: String::new(),
            args: Vec::new(),
            transport: Some("http".to_string()),
            url: Some("ftp://example.com/mcp".to_string()),
            config: BTreeMap::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            oauth: None,
        },
    );
    let err = validate_workflow_config(&config).expect_err("invalid scheme should fail");
    let message = err.to_string();
    assert!(
        message.contains("must be a valid http:// or https:// URL"),
        "error should mention url scheme: {}",
        message
    );
}

#[test]
fn validation_rejects_http_with_empty_host() {
    for bad in ["https:///mcp", "https:// /mcp", "http://"] {
        let mut config = builtin_workflow_config();
        config.mcp_servers.insert(
            "no-host".to_string(),
            McpServerDefinition {
                command: String::new(),
                args: Vec::new(),
                transport: Some("http".to_string()),
                url: Some(bad.to_string()),
                config: BTreeMap::new(),
                tools: Vec::new(),
                env: BTreeMap::new(),
                oauth: None,
            },
        );
        let err =
            validate_workflow_config(&config).err().unwrap_or_else(|| panic!("expected error for {bad:?}")).to_string();
        assert!(err.contains("must be a valid http:// or https:// URL"), "expected URL error for {bad:?}, got: {err}");
    }
}

#[test]
fn validation_rejects_unknown_transport() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "weird".to_string(),
        McpServerDefinition {
            command: String::new(),
            args: Vec::new(),
            transport: Some("websocket".to_string()),
            url: None,
            config: BTreeMap::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            oauth: None,
        },
    );
    let err = validate_workflow_config(&config).expect_err("unknown transport should fail");
    let message = err.to_string();
    assert!(message.contains("must be \"stdio\" or \"http\""), "error should mention valid transports: {}", message);
}

#[test]
fn yaml_env_var_interpolation_reaches_http_mcp_url() {
    let _guard = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env_guard = EnvVarGuard::set("ROBINHOOD_MCP_URL", "https://agent.robinhood.com/mcp/trading");
    let yaml_raw = r#"
mcp_servers:
  robinhood-trading:
    transport: "http"
    url: "${ROBINHOOD_MCP_URL}"
"#;
    let substituted = super::env_interp::interpolate_env(yaml_raw, "test.yaml").expect("env interp ok");
    let config = parse_yaml_workflow_config(&substituted).expect("should parse after interp");
    let server = config.mcp_servers.get("robinhood-trading").expect("server should exist");
    assert_eq!(server.url.as_deref(), Some("https://agent.robinhood.com/mcp/trading"));
}

#[test]
fn validation_rejects_agent_reference_to_unknown_mcp_server() {
    use crate::agent_runtime_config::AgentProfileOverlay;

    let mut config = builtin_workflow_config();
    let mut profile = AgentProfileOverlay::default();
    profile.mcp_servers = Some(vec!["does-not-exist".to_string()]);
    config.agent_profiles.insert("rogue".to_string(), profile);

    let err = validate_workflow_config(&config).expect_err("unknown server reference should fail");
    let message = err.to_string();
    assert!(
        message.contains("agent_profiles['rogue'].mcp_servers references unknown MCP server 'does-not-exist'"),
        "error should mention unknown reference: {}",
        message
    );
}

// ---------------------------------------------------------------------------
// v0.5.5 — worktree:, secrets:, and sensitive-interpolation lint.
// ---------------------------------------------------------------------------

#[test]
fn yaml_parses_workflow_level_worktree_block() {
    let yaml = r#"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build it."
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."
workflows:
- id: standard-workflow
  phases: [build]
  worktree:
    mode: required
    cleanup: false
    base_ref: develop
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let workflow = config.workflows.iter().find(|w| w.id == "standard-workflow").expect("workflow present");
    let worktree = workflow.worktree.as_ref().expect("worktree block populated");
    assert_eq!(worktree.mode, WorktreeMode::Required);
    assert!(!worktree.cleanup);
    assert_eq!(worktree.base_ref.as_deref(), Some("develop"));
}

#[test]
fn yaml_parses_short_form_phase_worktree_skip() {
    let yaml = r#"
phases:
  doc-only:
    mode: agent
    agent: swe
    directive: "Update docs."
    worktree: skip
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."
workflows:
- id: doc-flow
  phases: [doc-only]
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let phase = config.phase_definitions.get("doc-only").expect("phase present");
    let worktree = phase.worktree.as_ref().expect("phase-level worktree");
    assert_eq!(worktree.mode, WorktreeMode::Skip);
    assert!(worktree.cleanup, "cleanup should default to true for short-form");
    assert!(worktree.base_ref.is_none());
}

#[test]
fn yaml_parses_bool_shorthand_phase_worktree_false() {
    let yaml = r#"
phases:
  doc-only:
    mode: agent
    agent: swe
    directive: "Update docs."
    worktree: false
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."
workflows:
- id: doc-flow
  phases: [doc-only]
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let phase = config.phase_definitions.get("doc-only").expect("phase present");
    let worktree = phase.worktree.as_ref().expect("phase-level worktree");
    assert_eq!(worktree.mode, WorktreeMode::Skip);
    assert!(worktree.cleanup, "cleanup defaults to true for bool shorthand");
}

#[test]
fn yaml_parses_bool_shorthand_phase_worktree_true() {
    let yaml = r#"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build."
    worktree: true
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."
workflows:
- id: build-flow
  phases: [build]
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let phase = config.phase_definitions.get("build").expect("phase present");
    let worktree = phase.worktree.as_ref().expect("phase-level worktree");
    assert_eq!(worktree.mode, WorktreeMode::Auto);
}

#[test]
fn phase_level_worktree_skip_overrides_workflow_required() {
    // When a workflow is `mode: required` but an individual phase says
    // `worktree: skip`, the phase-level decision must win — the kernel
    // surfaces both fields and the runner resolves the override.
    let yaml = r#"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build it."
  docs:
    mode: agent
    agent: swe
    directive: "Update docs."
    worktree: skip
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."
workflows:
- id: hybrid
  worktree:
    mode: required
  phases:
    - build
    - docs
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let workflow = config.workflows.iter().find(|w| w.id == "hybrid").expect("workflow");
    assert_eq!(workflow.worktree.as_ref().unwrap().mode, WorktreeMode::Required);
    let build = config.phase_definitions.get("build").expect("build phase");
    assert!(build.worktree.is_none(), "phase without override inherits workflow setting");
    let docs = config.phase_definitions.get("docs").expect("docs phase");
    assert_eq!(docs.worktree.as_ref().unwrap().mode, WorktreeMode::Skip);
}

#[test]
fn yaml_rejects_invalid_worktree_mode() {
    let yaml = r#"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build it."
    worktree: rocket-fuel
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."
workflows:
- id: bad
  phases: [build]
"#;
    let err = parse_yaml_workflow_config(yaml).expect_err("unknown mode should fail");
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("invalid `worktree:` value") || msg.contains("worktree mode"),
        "error should mention worktree value: {msg}"
    );
}

#[test]
fn yaml_parses_top_level_secrets_block() {
    let yaml = r#"
secrets:
  linear_token:
    env: LINEAR_API_TOKEN
    description: Linear GraphQL auth token
  optional_key:
    env: OPTIONAL_KEY
    required: false

phases:
  build:
    mode: agent
    agent: swe
    directive: "Build."
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."
workflows:
- id: flow
  phases: [build]
"#;
    let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
    let linear = config.secrets.get("linear_token").expect("linear_token declared");
    assert_eq!(linear.env, "LINEAR_API_TOKEN");
    assert!(linear.required);
    assert_eq!(linear.description.as_deref(), Some("Linear GraphQL auth token"));
    let optional = config.secrets.get("optional_key").expect("optional_key declared");
    assert!(!optional.required);
}

#[test]
fn secret_interpolation_resolves_against_declared_env_var() {
    use super::env_interp::interpolate_secrets;
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _v = EnvVarGuard::set("ANIMUS_TEST_LINEAR_TOKEN", "lnk_test_value");

    let mut secrets = BTreeMap::new();
    secrets.insert(
        "linear_token".to_string(),
        SecretRef { env: "ANIMUS_TEST_LINEAR_TOKEN".to_string(), required: true, description: None },
    );

    let yaml = "value: ${secret.linear_token}\n";
    let out = interpolate_secrets(yaml, "test.yaml", &secrets).expect("interp ok");
    assert_eq!(out, "value: lnk_test_value\n");
}

#[test]
fn secret_interpolation_errors_on_undeclared_key() {
    use super::env_interp::interpolate_secrets;
    let secrets: BTreeMap<String, SecretRef> = BTreeMap::new();
    let yaml = "a: 1\nb: 2\nval: ${secret.missing}\n";
    let err = interpolate_secrets(yaml, ".animus/workflows.yaml", &secrets).expect_err("undeclared should error");
    let msg = format!("{:#}", err);
    assert!(msg.contains("line 3"), "missing line number: {msg}");
    assert!(msg.contains("missing"), "missing key name: {msg}");
    assert!(msg.contains("secrets:"), "should hint at secrets block: {msg}");
}

#[test]
fn secret_interpolation_errors_on_required_unset_env() {
    use super::env_interp::interpolate_secrets;
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _v = EnvVarGuard::unset("ANIMUS_TEST_DEFINITELY_UNSET_TOKEN");

    let mut secrets = BTreeMap::new();
    secrets.insert(
        "tok".to_string(),
        SecretRef { env: "ANIMUS_TEST_DEFINITELY_UNSET_TOKEN".to_string(), required: true, description: None },
    );
    let yaml = "val: ${secret.tok}\n";
    let err = interpolate_secrets(yaml, "test.yaml", &secrets).expect_err("required unset should fail");
    let msg = format!("{:#}", err);
    assert!(msg.contains("ANIMUS_TEST_DEFINITELY_UNSET_TOKEN"), "should name env var: {msg}");
    assert!(msg.contains("tok"), "should name secret: {msg}");
}

#[test]
fn optional_unset_secret_resolves_to_empty_string() {
    use super::env_interp::interpolate_secrets;
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _v = EnvVarGuard::unset("ANIMUS_TEST_OPTIONAL_SECRET");

    let mut secrets = BTreeMap::new();
    secrets.insert(
        "opt".to_string(),
        SecretRef { env: "ANIMUS_TEST_OPTIONAL_SECRET".to_string(), required: false, description: None },
    );
    let yaml = "val: \"${secret.opt}\"\n";
    let out = interpolate_secrets(yaml, "test.yaml", &secrets).expect("optional should not fail");
    assert_eq!(out, "val: \"\"\n");
}

#[test]
fn double_dollar_preserves_literal_secret_reference_through_both_passes() {
    // A user who wants the literal string `${secret.api}` in YAML output
    // (e.g. inside a prompt or example) writes `$${secret.api}`. The env
    // pass must hand the `$$` through to the secrets pass, which collapses
    // it back to `$` — yielding a literal that is NOT resolved.
    use super::env_interp::{interpolate_env, interpolate_secrets};
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _v = EnvVarGuard::set("ANIMUS_TEST_DOLLAR_DOLLAR", "should-not-leak");
    let mut secrets = BTreeMap::new();
    secrets.insert(
        "api".to_string(),
        SecretRef { env: "ANIMUS_TEST_DOLLAR_DOLLAR".to_string(), required: true, description: None },
    );

    let src = "prompt: $${secret.api}\n";
    let after_env = interpolate_env(src, "test.yaml").expect("env interp");
    let after_secrets = interpolate_secrets(&after_env, "test.yaml", &secrets).expect("secret interp");
    assert_eq!(after_secrets, "prompt: ${secret.api}\n");
}

#[test]
fn env_interpolation_passes_secret_references_through_untouched() {
    // The env interp pass must leave ${secret.X} references alone so the
    // secret pass can handle them; otherwise it would error on the `.` in
    // the name validation.
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _v = EnvVarGuard::set("ANIMUS_TEST_SOMETHING", "hello");
    let src = "a: ${ANIMUS_TEST_SOMETHING}\nb: ${secret.foo}\n";
    let out = super::env_interp::interpolate_env(src, "test.yaml").expect("env interp ok");
    assert_eq!(out, "a: hello\nb: ${secret.foo}\n");
}

#[test]
fn lint_flags_sensitive_var_outside_secrets_block() {
    use super::env_interp::lint_sensitive_interpolations;
    let yaml = r#"
mcp_servers:
  linear:
    transport: stdio
    command: linear-mcp
    env:
      LINEAR_API_TOKEN: "${API_TOKEN}"
"#;
    let warnings = lint_sensitive_interpolations(yaml, "workflows.yaml");
    assert!(!warnings.is_empty(), "expected at least one warning for API_TOKEN");
    let msg = &warnings[0];
    assert!(msg.contains("API_TOKEN"), "warning should mention the var: {msg}");
    assert!(msg.contains("secrets:"), "warning should hint at secrets block: {msg}");
}

#[test]
fn lint_skips_references_inside_secrets_block() {
    use super::env_interp::lint_sensitive_interpolations;
    let yaml = r#"
secrets:
  api:
    env: API_TOKEN
mcp_servers:
  linear:
    transport: stdio
    command: linear-mcp
    env:
      LINEAR_API_TOKEN: "${secret.api}"
"#;
    let warnings = lint_sensitive_interpolations(yaml, "workflows.yaml");
    assert!(warnings.is_empty(), "secret.* references should not trigger warnings: {:?}", warnings);
}

#[test]
fn lint_skips_secret_env_field_names() {
    // The webhook trigger config has a `secret_env: SOMETHING_TOKEN` field
    // — that is a declaration, not an interpolation. The lint should leave
    // it alone.
    use super::env_interp::lint_sensitive_interpolations;
    let yaml = r#"
triggers:
- id: github
  type: github_webhook
  workflow_ref: flow
  config:
    secret_env: GITHUB_WEBHOOK_TOKEN
"#;
    let warnings = lint_sensitive_interpolations(yaml, "workflows.yaml");
    assert!(warnings.is_empty(), "no interpolations means no warnings: {:?}", warnings);
}

#[test]
fn worktree_and_secrets_serde_roundtrip_through_workflow_config() {
    let mut config = builtin_workflow_config();
    config.default_workflow_ref = "standard-workflow".to_string();
    config.workflows.push(WorkflowDefinition {
        id: "standard-workflow".to_string(),
        name: "Standard".to_string(),
        description: String::new(),
        phases: vec!["implementation".to_string().into()],
        post_success: None,
        variables: Vec::new(),
        worktree: Some(WorktreeConfig {
            mode: WorktreeMode::Skip,
            cleanup: false,
            base_ref: Some("develop".to_string()),
        }),
        budget: None,
    });
    config.secrets.insert(
        "linear".to_string(),
        SecretRef { env: "LINEAR_API_TOKEN".to_string(), required: true, description: None },
    );

    let json = serde_json::to_string(&config).expect("serialize");
    let back: WorkflowConfig = serde_json::from_str(&json).expect("roundtrip");
    let workflow = back.workflows.iter().find(|w| w.id == "standard-workflow").expect("workflow present");
    let worktree = workflow.worktree.as_ref().expect("worktree preserved");
    assert_eq!(worktree.mode, WorktreeMode::Skip);
    assert!(!worktree.cleanup);
    assert_eq!(worktree.base_ref.as_deref(), Some("develop"));
    assert_eq!(back.secrets.get("linear").map(|s| s.env.as_str()), Some("LINEAR_API_TOKEN"));
}

#[test]
fn yaml_compile_resolves_secret_declared_in_earlier_overlay() {
    // Multi-file workflow configs are merged in lexicographic order. A
    // later file must be able to reference a secret declared in an
    // earlier file.
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _home = EnvVarGuard::set("HOME", tempfile::tempdir().unwrap().path());
    let _v = EnvVarGuard::set("ANIMUS_TEST_OVERLAY_SECRET", "overlay-value");

    let temp = tempfile::tempdir().expect("tempdir");
    let workflows_dir = temp.path().join(".animus").join("workflows");
    fs::create_dir_all(&workflows_dir).expect("mkdir");
    fs::write(
        workflows_dir.join("01-base.yaml"),
        r#"
secrets:
  api:
    env: ANIMUS_TEST_OVERLAY_SECRET
"#,
    )
    .expect("write base");
    fs::write(
        workflows_dir.join("02-mcp.yaml"),
        r#"
mcp_servers:
  linear:
    transport: stdio
    command: linear-mcp
    env:
      LINEAR_API_TOKEN: "${secret.api}"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build."
agents:
  swe:
    description: "SWE"
    system_prompt: "Be a SWE."
workflows:
- id: flow
  phases: [build]
"#,
    )
    .expect("write mcp");

    let compiled = compile_yaml_workflow_files(temp.path()).expect("compile").expect("Some");
    let server = compiled.mcp_servers.get("linear").expect("linear server present");
    assert_eq!(server.env.get("LINEAR_API_TOKEN").map(|s| s.as_str()), Some("overlay-value"));
}

#[test]
fn yaml_compile_resolves_secret_interpolation_end_to_end() {
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _home = EnvVarGuard::set("HOME", tempfile::tempdir().unwrap().path());
    let _v = EnvVarGuard::set("ANIMUS_TEST_E2E_SECRET", "resolved-value");

    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".animus").join("workflows")).expect("mkdir");
    let yaml = r#"
secrets:
  api:
    env: ANIMUS_TEST_E2E_SECRET
mcp_servers:
  linear:
    transport: stdio
    command: linear-mcp
    env:
      LINEAR_API_TOKEN: "${secret.api}"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build."
agents:
  swe:
    description: "SWE"
    system_prompt: "Be a SWE."
workflows:
- id: flow
  phases: [build]
"#;
    fs::write(temp.path().join(".animus").join("workflows.yaml"), yaml).expect("write yaml");

    let compiled = compile_yaml_workflow_files(temp.path()).expect("compile").expect("Some");
    let server = compiled.mcp_servers.get("linear").expect("linear server present");
    assert_eq!(server.env.get("LINEAR_API_TOKEN").map(|s| s.as_str()), Some("resolved-value"));
    assert_eq!(compiled.secrets.get("api").map(|s| s.env.as_str()), Some("ANIMUS_TEST_E2E_SECRET"));
}

#[test]
fn yaml_compile_emits_env_values_verbatim_without_rescanning() {
    // Env values containing `$$`, `${`, or `${secret.X}` must land in the
    // compiled config verbatim — never collapsed, re-parsed, or resolved
    // as secrets by a second interpolation pass.
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _home = EnvVarGuard::set("HOME", tempfile::tempdir().unwrap().path());
    let _v1 = EnvVarGuard::set("ANIMUS_TEST_DOLLARS_VALUE", "pa$$word");
    let _v2 = EnvVarGuard::set("ANIMUS_TEST_BRACE_VALUE", "open ${ brace");
    let _v3 = EnvVarGuard::set("ANIMUS_TEST_INJECTION_VALUE", "${secret.api}");
    let _v4 = EnvVarGuard::set("ANIMUS_TEST_REAL_SECRET", "should-not-leak");

    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".animus")).expect("mkdir");
    let yaml = r#"
secrets:
  api:
    env: ANIMUS_TEST_REAL_SECRET
mcp_servers:
  linear:
    transport: stdio
    command: linear-mcp
    env:
      DOLLARS: "${ANIMUS_TEST_DOLLARS_VALUE}"
      BRACE: "${ANIMUS_TEST_BRACE_VALUE}"
      INJECTION: "${ANIMUS_TEST_INJECTION_VALUE}"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build."
agents:
  swe:
    description: "SWE"
    system_prompt: "Be a SWE."
workflows:
- id: flow
  phases: [build]
"#;
    fs::write(temp.path().join(".animus").join("workflows.yaml"), yaml).expect("write yaml");

    let compiled = compile_yaml_workflow_files(temp.path()).expect("compile").expect("Some");
    let server = compiled.mcp_servers.get("linear").expect("linear server present");
    assert_eq!(server.env.get("DOLLARS").map(|s| s.as_str()), Some("pa$$word"));
    assert_eq!(server.env.get("BRACE").map(|s| s.as_str()), Some("open ${ brace"));
    assert_eq!(server.env.get("INJECTION").map(|s| s.as_str()), Some("${secret.api}"));
}

#[test]
fn yaml_parse_error_excerpt_shows_original_reference_not_resolved_secret() {
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _home = EnvVarGuard::set("HOME", tempfile::tempdir().unwrap().path());
    // A secret value that, once substituted, produces a YAML scan error on
    // the line carrying the reference. The rustc-style excerpt must render
    // the original `${secret.api}` reference, not the resolved value.
    let _v = EnvVarGuard::set("ANIMUS_TEST_EXCERPT_SECRET", "zzz-leak-zzz: x: y");

    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".animus")).expect("mkdir");
    let yaml = r#"
secrets:
  api:
    env: ANIMUS_TEST_EXCERPT_SECRET
default_workflow_ref: ${secret.api}
"#;
    fs::write(temp.path().join(".animus").join("workflows.yaml"), yaml).expect("write yaml");

    let err = compile_yaml_workflow_files(temp.path()).expect_err("compile should fail");
    let display = format!("{:#}", err);
    assert!(!display.contains("zzz-leak-zzz"), "resolved secret leaked into diagnostics: {display}");
    let debug = format!("{:#?}", err);
    assert!(!debug.contains("zzz-leak-zzz"), "resolved secret leaked into diagnostics: {debug}");
}

#[test]
fn yaml_parse_error_message_redacts_secret_resolved_into_typed_field() {
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _home = EnvVarGuard::set("HOME", tempfile::tempdir().unwrap().path());
    // A secret resolved into an int-typed position makes serde_yaml quote the
    // offending scalar in its own error message ("invalid type: string ...").
    // The surfaced error must name the secret instead of echoing its value.
    let _v = EnvVarGuard::set("ANIMUS_TEST_TYPED_SECRET", "zzz-typed-leak-zzz");

    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".animus")).expect("mkdir");
    let yaml = r#"
secrets:
  api_token:
    env: ANIMUS_TEST_TYPED_SECRET
daemon:
  pool_size: ${secret.api_token}
"#;
    fs::write(temp.path().join(".animus").join("workflows.yaml"), yaml).expect("write yaml");

    let err = compile_yaml_workflow_files(temp.path()).expect_err("compile should fail");
    let display = format!("{:#}", err);
    assert!(!display.contains("zzz-typed-leak-zzz"), "resolved secret leaked into diagnostics: {display}");
    assert!(display.contains("[redacted:api_token]"), "redaction marker should name the secret: {display}");
    let debug = format!("{:#?}", err);
    assert!(!debug.contains("zzz-typed-leak-zzz"), "resolved secret leaked into diagnostics: {debug}");
}

#[test]
fn yaml_parse_error_message_redacts_escaped_secret_rendering() {
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _home = EnvVarGuard::set("HOME", tempfile::tempdir().unwrap().path());
    // serde quotes offending scalars with `{:?}` escaping, so a secret
    // containing backslashes or quotes appears in the message in escaped
    // form; the redactor must catch that rendering too.
    let secret_value = r#"zzz\esc"leak-zzz"#;
    let _v = EnvVarGuard::set("ANIMUS_TEST_ESCAPED_SECRET", secret_value);

    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".animus")).expect("mkdir");
    let yaml = r#"
secrets:
  api_token:
    env: ANIMUS_TEST_ESCAPED_SECRET
daemon:
  pool_size: ${secret.api_token}
"#;
    fs::write(temp.path().join(".animus").join("workflows.yaml"), yaml).expect("write yaml");

    let err = compile_yaml_workflow_files(temp.path()).expect_err("compile should fail");
    let display = format!("{:#}", err);
    assert!(!display.contains(secret_value), "raw secret leaked into diagnostics: {display}");
    assert!(!display.contains(r#"zzz\\esc\"leak-zzz"#), "escaped secret leaked into diagnostics: {display}");
    assert!(display.contains("[redacted:api_token]"), "redaction marker should name the secret: {display}");
}

#[test]
fn yaml_parse_error_message_redacts_keychain_resolved_env_value() {
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _home = EnvVarGuard::set("HOME", tempfile::tempdir().unwrap().path());
    // A plain `${VAR}` resolved from the keychain fallback into an int-typed
    // position makes serde_yaml quote the offending scalar. The surfaced
    // error must redact the keychain-resolved value just like declared
    // secrets.
    let _v = EnvVarGuard::unset("ANIMUS_TEST_KEYCHAIN_REDACT");
    struct StubResolver;
    impl super::env_interp::WorkflowSecretResolver for StubResolver {
        fn resolve(&self, key: &str) -> Option<String> {
            (key == "ANIMUS_TEST_KEYCHAIN_REDACT").then(|| "zzz-keychain-leak-zzz".to_string())
        }
    }
    super::env_interp::install_workflow_secret_resolver_for_test(std::sync::Arc::new(StubResolver));

    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".animus")).expect("mkdir");
    let yaml = r#"
daemon:
  pool_size: ${ANIMUS_TEST_KEYCHAIN_REDACT}
"#;
    fs::write(temp.path().join(".animus").join("workflows.yaml"), yaml).expect("write yaml");

    let result = compile_yaml_workflow_files(temp.path());
    super::env_interp::clear_workflow_secret_resolver_for_test();
    let err = result.expect_err("compile should fail");
    let display = format!("{:#}", err);
    assert!(!display.contains("zzz-keychain-leak-zzz"), "keychain value leaked into diagnostics: {display}");
    assert!(
        display.contains("[redacted:ANIMUS_TEST_KEYCHAIN_REDACT]"),
        "redaction marker should name the env var: {display}"
    );
}

#[test]
fn upsert_generated_workflow_phase_drops_legacy_compiled_blocks() {
    // Pre-fix releases dumped the entire COMPILED config (resolved `${VAR}` /
    // `${secret.X}` values included) into generated-workflow.yaml. A post-fix
    // upsert keeps the authored blocks (phases, phase_catalog, workflows) but
    // drops the rest so leaked values stop being re-serialized.
    let temp = tempfile::tempdir().expect("tempdir");
    let workflows_dir = temp.path().join(".animus").join("workflows");
    fs::create_dir_all(&workflows_dir).expect("mkdir");
    let legacy_dump = r#"
default_workflow_ref: flow
mcp_servers:
  linear:
    transport: stdio
    command: linear-mcp
    env:
      LINEAR_API_TOKEN: "zzz-legacy-leak-zzz"
phases:
  old-phase:
    mode: agent
    agent: swe
    directive: "Old ${KEEP_UNRESOLVED}."
workflows:
- id: flow
  phases: [old-phase]
"#;
    fs::write(workflows_dir.join("generated-workflow.yaml"), legacy_dump).expect("write legacy dump");

    let definition: PhaseExecutionDefinition = serde_json::from_value(serde_json::json!({
        "mode": "agent",
        "agent_id": "swe",
        "directive": "New phase."
    }))
    .expect("definition should parse");
    super::yaml_compiler::upsert_generated_workflow_phase(temp.path(), "new-phase", &definition, None)
        .expect("upsert should succeed");

    let content = fs::read_to_string(workflows_dir.join("generated-workflow.yaml")).expect("read overlay");
    assert!(content.contains("new-phase"), "upserted phase missing: {content}");
    assert!(content.contains("old-phase"), "previously authored phase should survive: {content}");
    assert!(content.contains("${KEEP_UNRESOLVED}"), "unresolved references must round-trip verbatim: {content}");
    assert!(!content.contains("zzz-legacy-leak-zzz"), "legacy leaked value should be dropped: {content}");
    assert!(!content.contains("mcp_servers"), "non-authored blocks should be dropped: {content}");
}

#[test]
fn yaml_compiles_when_comment_references_unset_env_var() {
    let _g = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _home = EnvVarGuard::set("HOME", tempfile::tempdir().unwrap().path());
    let _v = EnvVarGuard::unset("ANIMUS_TEST_COMMENTED_OUT_VAR");

    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".animus")).expect("mkdir");
    let yaml = r#"
# setup: export ${ANIMUS_TEST_COMMENTED_OUT_VAR} before running
# see ${docs-url} for details
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build." # trailing note about ${ANIMUS_TEST_COMMENTED_OUT_VAR}
agents:
  swe:
    description: "SWE"
    system_prompt: "Be a SWE."
workflows:
- id: flow
  phases: [build]
"#;
    fs::write(temp.path().join(".animus").join("workflows.yaml"), yaml).expect("write yaml");

    let compiled = compile_yaml_workflow_files(temp.path()).expect("compile").expect("Some");
    assert!(compiled.phase_definitions.contains_key("build"));
}

#[test]
fn yaml_merge_field_merges_daemon_blocks_across_overlays() {
    let base_yaml = r#"
daemon:
  auto_run_ready: true
  active_hours: "09:00-17:00"
  pool_size: 4
workflows:
  - id: standard
    name: Standard
    phases:
      - requirements
"#;
    let overlay_yaml = r#"
daemon:
  pool_size: 2
"#;
    let base = parse_yaml_workflow_config(base_yaml).expect("parse base");
    let overlay = parse_yaml_workflow_config(overlay_yaml).expect("parse overlay");
    let merged = merge_yaml_into_config(base, overlay);
    let daemon = merged.daemon.expect("daemon block present");
    assert!(daemon.auto_run_ready, "earlier overlay's auto_run_ready must survive a later daemon block");
    assert_eq!(daemon.active_hours.as_deref(), Some("09:00-17:00"));
    assert_eq!(daemon.pool_size, Some(2), "later overlay's explicit field must win");
}

#[test]
fn validation_surfaces_malformed_trigger_configs() {
    let mut config = builtin_workflow_config();
    config.workflows.push(WorkflowDefinition {
        id: "flow".to_string(),
        name: "Flow".to_string(),
        description: String::new(),
        phases: vec!["implementation".to_string().into()],
        post_success: None,
        variables: Vec::new(),
        worktree: None,
        budget: None,
    });
    config.triggers.push(WorkflowTrigger {
        id: "bad-webhook".to_string(),
        trigger_type: TriggerType::Webhook,
        workflow_ref: Some("flow".to_string()),
        enabled: true,
        config: serde_json::json!({ "secret_env": 123 }),
        input: None,
    });
    config.triggers.push(WorkflowTrigger {
        id: "bad-watcher".to_string(),
        trigger_type: TriggerType::FileWatcher,
        workflow_ref: Some("flow".to_string()),
        enabled: true,
        config: serde_json::json!({ "paths": "not-a-list" }),
        input: None,
    });

    let err = validate_workflow_config(&config).expect_err("malformed trigger configs should fail validation");
    let msg = format!("{:#}", err);
    assert!(msg.contains("triggers['bad-webhook'].config is not a valid webhook config"), "{msg}");
    assert!(msg.contains("triggers['bad-watcher'].config is not a valid file_watcher config"), "{msg}");
    assert!(!msg.contains("paths must not be empty"), "type error must not masquerade as empty paths: {msg}");
}

fn http_oauth_server(oauth: OauthConfig) -> McpServerDefinition {
    McpServerDefinition {
        command: String::new(),
        args: Vec::new(),
        transport: Some("http".to_string()),
        url: Some("https://agent.example.com/mcp".to_string()),
        config: BTreeMap::new(),
        tools: Vec::new(),
        env: BTreeMap::new(),
        oauth: Some(oauth),
    }
}

#[test]
fn yaml_parses_workflow_level_budget_block() {
    let yaml = r#"
workflows:
  - id: expensive
    name: Expensive Flow
    phases:
      - requirements
      - implementation
    budget:
      max_tokens: 1000000
      max_cost_usd: 5.0
      on_exceed: pause
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse workflow budget");
    let workflow = config.workflows.iter().find(|p| p.id == "expensive").expect("workflow found");
    let budget = workflow.budget.as_ref().expect("workflow budget set");
    assert_eq!(budget.max_tokens, Some(1_000_000));
    assert!((budget.max_cost_usd.unwrap() - 5.0).abs() < 1e-9);
    assert_eq!(budget.on_exceed, BudgetOnExceed::Pause);
}

#[test]
fn yaml_parses_phase_level_budget_block() {
    let yaml = r#"
workflows:
  - id: expensive
    name: Expensive Flow
    phases:
      - exploration:
          budget:
            max_tokens: 100000
            max_cost_usd: 1.0
            on_exceed: fail
      - implementation
"#;
    let config = parse_yaml_workflow_config(yaml).expect("should parse phase budget");
    let workflow = config.workflows.iter().find(|p| p.id == "expensive").expect("workflow found");
    let phase_budget = workflow.phases[0].budget().expect("phase budget set");
    assert_eq!(phase_budget.max_tokens, Some(100_000));
    assert_eq!(phase_budget.on_exceed, BudgetOnExceed::Fail);
}

#[test]
fn yaml_budget_round_trips_through_overlay_writer() {
    let yaml = r#"
workflows:
  - id: expensive
    name: Expensive Flow
    phases:
      - implementation
    budget:
      max_tokens: 250000
      on_exceed: warn
"#;
    let parsed = parse_yaml_workflow_config(yaml).expect("parse");
    let definition = parsed.workflows.into_iter().find(|p| p.id == "expensive").expect("workflow found");
    let serialized = serde_json::to_value(&definition).expect("serialize");
    let budget = serialized.get("budget").expect("budget retained in json round trip");
    assert_eq!(budget.get("max_tokens").and_then(|v| v.as_u64()), Some(250_000));
    assert_eq!(budget.get("on_exceed").and_then(|v| v.as_str()), Some("warn"));
}

fn workflow_with_budget(id: &str, budget: BudgetConfig) -> WorkflowDefinition {
    WorkflowDefinition {
        id: id.to_string(),
        name: id.to_string(),
        description: String::new(),
        phases: vec![WorkflowPhaseEntry::Simple("requirements".to_string())],
        post_success: None,
        variables: Vec::new(),
        worktree: None,
        budget: Some(budget),
    }
}

#[test]
fn yaml_parses_oauth_client_credentials_block() {
    let yaml = r#"
mcp_servers:
  example:
    transport: "http"
    url: "https://agent.example.com/mcp"
    oauth:
      flow: client_credentials
      token_url: "https://auth.example.com/token"
      client_id_env: EXAMPLE_CLIENT_ID
      client_secret_env: EXAMPLE_CLIENT_SECRET
      scopes:
        - read
        - write
      audience: "https://api.example.com"
"#;
    let config = parse_yaml_workflow_config(yaml).expect("oauth block should parse");
    let server = config.mcp_servers.get("example").expect("server exists");
    let oauth = server.oauth.as_ref().expect("oauth block present");
    assert_eq!(oauth.flow, OauthFlow::ClientCredentials);
    assert_eq!(oauth.token_url.as_deref(), Some("https://auth.example.com/token"));
    assert_eq!(oauth.client_id_env.as_deref(), Some("EXAMPLE_CLIENT_ID"));
    assert_eq!(oauth.client_secret_env.as_deref(), Some("EXAMPLE_CLIENT_SECRET"));
    assert_eq!(oauth.scopes, vec!["read".to_string(), "write".to_string()]);
    assert_eq!(oauth.audience.as_deref(), Some("https://api.example.com"));
    assert!(oauth.cache, "cache should default to true");
}

#[test]
fn yaml_parses_oauth_refresh_token_block() {
    let yaml = r#"
mcp_servers:
  example:
    transport: "http"
    url: "https://agent.example.com/mcp"
    oauth:
      flow: refresh_token
      token_url: "https://auth.example.com/token"
      refresh_token_env: EXAMPLE_REFRESH
      cache: false
"#;
    let config = parse_yaml_workflow_config(yaml).expect("oauth block should parse");
    let oauth = config.mcp_servers.get("example").unwrap().oauth.as_ref().expect("oauth present");
    assert_eq!(oauth.flow, OauthFlow::RefreshToken);
    assert_eq!(oauth.refresh_token_env.as_deref(), Some("EXAMPLE_REFRESH"));
    assert!(!oauth.cache, "explicit cache: false should be honored");
}

#[test]
fn yaml_parses_oauth_manual_bearer_block() {
    let yaml = r#"
mcp_servers:
  example:
    transport: "http"
    url: "https://agent.example.com/mcp"
    oauth:
      flow: manual_bearer
      bearer_env: EXAMPLE_BEARER
"#;
    let config = parse_yaml_workflow_config(yaml).expect("oauth block should parse");
    let oauth = config.mcp_servers.get("example").unwrap().oauth.as_ref().expect("oauth present");
    assert_eq!(oauth.flow, OauthFlow::ManualBearer);
    assert_eq!(oauth.bearer_env.as_deref(), Some("EXAMPLE_BEARER"));
}

#[test]
fn yaml_parses_oauth_authorization_code_block() {
    let yaml = r#"
mcp_servers:
  github:
    transport: "http"
    url: "https://api.githubcopilot.com/mcp/"
    oauth:
      flow: authorization_code
      scopes:
        - repo
        - read:user
      client_id: "pre-registered-client"
"#;
    let config = parse_yaml_workflow_config(yaml).expect("authorization_code oauth block should parse");
    let oauth = config.mcp_servers.get("github").unwrap().oauth.as_ref().expect("oauth present");
    assert_eq!(oauth.flow, OauthFlow::AuthorizationCode);
    assert_eq!(oauth.scopes, vec!["repo".to_string(), "read:user".to_string()]);
    assert_eq!(oauth.client_id.as_deref(), Some("pre-registered-client"));
    assert!(oauth.cache, "cache should default to true");
}

#[test]
fn yaml_parses_minimal_oauth_authorization_code_block() {
    // No scopes, no client_id — discovery + DCR fill everything. The minimal
    // shape must still parse.
    let yaml = r#"
mcp_servers:
  linear:
    transport: "http"
    url: "https://mcp.linear.app/mcp"
    oauth:
      flow: authorization_code
"#;
    let config = parse_yaml_workflow_config(yaml).expect("minimal authorization_code block should parse");
    let oauth = config.mcp_servers.get("linear").unwrap().oauth.as_ref().expect("oauth present");
    assert_eq!(oauth.flow, OauthFlow::AuthorizationCode);
    assert!(oauth.scopes.is_empty());
    assert!(oauth.client_id.is_none());
}

#[test]
fn validation_rejects_authorization_code_with_m2m_fields() {
    // The authorization_code flow must reject machine-to-machine credential
    // pointers (token_url / *_env) — discovery fills those in.
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "github".to_string(),
        http_oauth_server(OauthConfig {
            flow: OauthFlow::AuthorizationCode,
            token_url: Some("https://example.com/token".to_string()),
            client_id_env: Some("LEFTOVER".to_string()),
            client_secret_env: None,
            refresh_token_env: None,
            bearer_env: None,
            scopes: vec!["repo".to_string()],
            audience: None,
            cache: true,
            client_id: None,
        }),
    );
    let err = crate::workflow_config::validate_workflow_config(&config)
        .expect_err("authorization_code with token_url/*_env must fail validation");
    let msg = err.to_string();
    assert!(msg.contains("token_url"), "should reject token_url: {msg}");
    assert!(msg.contains("client_id_env"), "should reject client_id_env: {msg}");
}

#[test]
fn validation_rejects_authorization_code_with_blank_client_id() {
    // A blank pinned client_id is a typo that would otherwise skip DCR with
    // an empty id; validation must reject it.
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "github".to_string(),
        http_oauth_server(OauthConfig {
            flow: OauthFlow::AuthorizationCode,
            token_url: None,
            client_id_env: None,
            client_secret_env: None,
            refresh_token_env: None,
            bearer_env: None,
            scopes: vec!["repo".to_string()],
            audience: None,
            cache: true,
            client_id: Some("   ".to_string()),
        }),
    );
    let err = crate::workflow_config::validate_workflow_config(&config)
        .expect_err("authorization_code with blank client_id must fail validation");
    assert!(err.to_string().contains("client_id"), "should reject blank client_id: {err}");
}

#[test]
fn validation_accepts_oauth_client_credentials() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "example".to_string(),
        http_oauth_server(OauthConfig {
            flow: OauthFlow::ClientCredentials,
            token_url: Some("https://auth.example.com/token".to_string()),
            client_id_env: Some("EXAMPLE_CLIENT_ID".to_string()),
            client_secret_env: Some("EXAMPLE_CLIENT_SECRET".to_string()),
            refresh_token_env: None,
            bearer_env: None,
            scopes: vec!["read".to_string()],
            audience: None,
            cache: true,
            client_id: None,
        }),
    );
    validate_workflow_config(&config).expect("complete cc oauth block should validate");
}

#[test]
fn validation_rejects_client_credentials_missing_token_url() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "example".to_string(),
        http_oauth_server(OauthConfig {
            flow: OauthFlow::ClientCredentials,
            token_url: None,
            client_id_env: Some("EXAMPLE_CLIENT_ID".to_string()),
            client_secret_env: Some("EXAMPLE_CLIENT_SECRET".to_string()),
            refresh_token_env: None,
            bearer_env: None,
            scopes: vec![],
            audience: None,
            cache: true,
            client_id: None,
        }),
    );
    let err = validate_workflow_config(&config).expect_err("missing token_url should fail");
    let message = err.to_string();
    assert!(
        message.contains("oauth.token_url is required for flow=\"client_credentials\""),
        "error should name missing token_url: {message}"
    );
}

#[test]
fn validation_rejects_client_credentials_missing_client_secret_env() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "example".to_string(),
        http_oauth_server(OauthConfig {
            flow: OauthFlow::ClientCredentials,
            token_url: Some("https://auth.example.com/token".to_string()),
            client_id_env: Some("EXAMPLE_CLIENT_ID".to_string()),
            client_secret_env: None,
            refresh_token_env: None,
            bearer_env: None,
            scopes: vec![],
            audience: None,
            cache: true,
            client_id: None,
        }),
    );
    let err = validate_workflow_config(&config).expect_err("missing client_secret_env should fail");
    let message = err.to_string();
    assert!(
        message.contains("oauth.client_secret_env is required for flow=\"client_credentials\""),
        "error should name missing client_secret_env: {message}"
    );
}

#[test]
fn validation_rejects_manual_bearer_without_bearer_env() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "example".to_string(),
        http_oauth_server(OauthConfig {
            flow: OauthFlow::ManualBearer,
            token_url: None,
            client_id_env: None,
            client_secret_env: None,
            refresh_token_env: None,
            bearer_env: None,
            scopes: vec![],
            audience: None,
            cache: false,
            client_id: None,
        }),
    );
    let err = validate_workflow_config(&config).expect_err("missing bearer_env should fail");
    let message = err.to_string();
    assert!(
        message.contains("oauth.bearer_env is required for flow=\"manual_bearer\""),
        "error should name missing bearer_env: {message}"
    );
}

#[test]
fn validation_rejects_oauth_on_stdio_transport() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "example".to_string(),
        McpServerDefinition {
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            transport: Some("stdio".to_string()),
            url: None,
            config: BTreeMap::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            oauth: Some(OauthConfig {
                flow: OauthFlow::ManualBearer,
                token_url: None,
                client_id_env: None,
                client_secret_env: None,
                refresh_token_env: None,
                bearer_env: Some("EXAMPLE_BEARER".to_string()),
                scopes: vec![],
                audience: None,
                cache: false,
                client_id: None,
            }),
        },
    );
    let err = validate_workflow_config(&config).expect_err("oauth on stdio should fail");
    let message = err.to_string();
    assert!(
        message.contains("oauth is only valid when transport is \"http\""),
        "error should reject stdio + oauth: {message}"
    );
}

#[test]
fn validation_rejects_oauth_token_url_with_missing_host() {
    // Regression guard for codex round-4 [P2]: previously a bare scheme
    // like `https://` passed the prefix check and only failed later
    // when reqwest tried to POST it. Now the validator requires a
    // non-empty host segment.
    for bad in ["https://", "http://", "https:// ", " https://auth.example.com/token"] {
        let mut config = builtin_workflow_config();
        config.mcp_servers.insert(
            "example".to_string(),
            http_oauth_server(OauthConfig {
                flow: OauthFlow::ClientCredentials,
                token_url: Some(bad.to_string()),
                client_id_env: Some("EXAMPLE_CLIENT_ID".to_string()),
                client_secret_env: Some("EXAMPLE_CLIENT_SECRET".to_string()),
                refresh_token_env: None,
                bearer_env: None,
                scopes: vec![],
                audience: None,
                cache: true,
                client_id: None,
            }),
        );
        let err = validate_workflow_config(&config).expect_err("expected error for {bad:?}");
        let message = err.to_string();
        assert!(
            message.contains("oauth.token_url"),
            "error should name the oauth.token_url field for {bad:?}: {message}"
        );
    }
}

#[test]
fn validation_rejects_refresh_token_with_bearer_env() {
    let mut config = builtin_workflow_config();
    config.mcp_servers.insert(
        "example".to_string(),
        http_oauth_server(OauthConfig {
            flow: OauthFlow::RefreshToken,
            token_url: Some("https://auth.example.com/token".to_string()),
            client_id_env: None,
            client_secret_env: None,
            refresh_token_env: Some("EXAMPLE_REFRESH".to_string()),
            bearer_env: Some("EXAMPLE_BEARER".to_string()),
            scopes: vec![],
            audience: None,
            cache: true,
            client_id: None,
        }),
    );
    let err = validate_workflow_config(&config).expect_err("bearer_env must not coexist with refresh_token");
    let message = err.to_string();
    assert!(
        message.contains("oauth.bearer_env must not be set for flow=\"refresh_token\""),
        "error should reject bearer_env in refresh flow: {message}"
    );
}

#[test]
fn yaml_evals_block_round_trips_through_parser() {
    let yaml_raw = r#"
phases:
  implementation:
    mode: agent
    agent: default
    evals:
      pass_threshold: 0.8
      on_fail: rework
      max_reworks: 2
      checks:
        - id: unit-tests
          kind: command
          command: cargo
          args: [test, --workspace]
          working_dir: $REPO_ROOT
          timeout_secs: 300
          expected_exit: 0
        - id: code-quality
          kind: llm_judge
          agent: default
          prompt: "Verdict?"
"#;
    let config = parse_yaml_workflow_config(yaml_raw).expect("parse ok");
    let phase = config.phase_definitions.get("implementation").expect("phase present");
    let evals = phase.evals.as_ref().expect("evals present");
    assert!((evals.pass_threshold - 0.8).abs() < 1e-3);
    assert_eq!(evals.max_reworks, 2);
    assert_eq!(evals.checks.len(), 2);
    let cmd = &evals.checks[0];
    assert_eq!(cmd.id, "unit-tests");
    assert_eq!(cmd.command.as_deref(), Some("cargo"));
    assert_eq!(cmd.args, vec!["test", "--workspace"]);
    assert_eq!(cmd.working_dir.as_deref(), Some("$REPO_ROOT"));
    assert_eq!(cmd.timeout_secs, Some(300));
    let judge = &evals.checks[1];
    assert_eq!(judge.id, "code-quality");
    assert_eq!(judge.agent.as_deref(), Some("default"));
    assert_eq!(judge.prompt.as_deref(), Some("Verdict?"));
}

#[test]
fn yaml_evals_defaults_apply_when_omitted() {
    let yaml_raw = r#"
phases:
  implementation:
    mode: agent
    agent: default
    evals:
      checks:
        - id: unit-tests
          kind: command
          command: cargo
"#;
    let config = parse_yaml_workflow_config(yaml_raw).expect("parse ok");
    let phase = config.phase_definitions.get("implementation").expect("phase present");
    let evals = phase.evals.as_ref().expect("evals present");
    assert!((evals.pass_threshold - 1.0).abs() < 1e-3, "default threshold should be 1.0");
    assert_eq!(evals.max_reworks, 0, "default max_reworks should be 0");
    assert_eq!(evals.on_fail, crate::agent_runtime_config::EvalOnFail::Block, "default on_fail should be block");
    assert_eq!(evals.checks[0].expected_exit, 0, "default expected_exit should be 0");
}

fn seed_implementation_phase(config: &mut WorkflowConfig) {
    config.phase_definitions.insert(
        "implementation".to_string(),
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: None,
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
            idempotency: Idempotency::Unknown,
            worktree: None,
            evals: None,
        },
    );
}

#[test]
fn validation_rejects_pass_threshold_outside_unit_range() {
    use crate::agent_runtime_config::{AgentProfileOverlay, EvalCheck, EvalKind, EvalOnFail, EvalsConfig};

    let mut config = builtin_workflow_config();
    config.agent_profiles.insert("default".to_string(), AgentProfileOverlay::default());
    seed_implementation_phase(&mut config);
    let phase = config.phase_definitions.get_mut("implementation").expect("phase exists");
    phase.evals = Some(EvalsConfig {
        pass_threshold: 1.5,
        on_fail: EvalOnFail::Block,
        max_reworks: 0,
        checks: vec![EvalCheck {
            id: "x".into(),
            kind: EvalKind::Command,
            command: Some("true".into()),
            args: Vec::new(),
            working_dir: None,
            timeout_secs: None,
            expected_exit: 0,
            agent: None,
            prompt: None,
        }],
    });
    let err = validate_workflow_config(&config).expect_err("out-of-range threshold should fail");
    assert!(err.to_string().contains("pass_threshold must be between 0.0 and 1.0"), "got: {err}");
}

#[test]
fn validation_rejects_command_check_missing_command_field() {
    use crate::agent_runtime_config::{EvalCheck, EvalKind, EvalOnFail, EvalsConfig};

    let mut config = builtin_workflow_config();
    seed_implementation_phase(&mut config);
    let phase = config.phase_definitions.get_mut("implementation").expect("phase exists");
    phase.evals = Some(EvalsConfig {
        pass_threshold: 1.0,
        on_fail: EvalOnFail::Block,
        max_reworks: 0,
        checks: vec![EvalCheck {
            id: "x".into(),
            kind: EvalKind::Command,
            command: None,
            args: Vec::new(),
            working_dir: None,
            timeout_secs: None,
            expected_exit: 0,
            agent: None,
            prompt: None,
        }],
    });
    let err = validate_workflow_config(&config).expect_err("missing command should fail");
    assert!(err.to_string().contains("kind='command' requires a non-empty command field"), "got: {err}");
}

#[test]
fn validation_rejects_rework_on_fail_with_zero_budget() {
    use crate::agent_runtime_config::{EvalCheck, EvalKind, EvalOnFail, EvalsConfig};

    let mut config = builtin_workflow_config();
    seed_implementation_phase(&mut config);
    let phase = config.phase_definitions.get_mut("implementation").expect("phase exists");
    phase.evals = Some(EvalsConfig {
        pass_threshold: 1.0,
        on_fail: EvalOnFail::Rework,
        max_reworks: 0,
        checks: vec![EvalCheck {
            id: "x".into(),
            kind: EvalKind::Command,
            command: Some("true".into()),
            args: Vec::new(),
            working_dir: None,
            timeout_secs: None,
            expected_exit: 0,
            agent: None,
            prompt: None,
        }],
    });
    let err = validate_workflow_config(&config).expect_err("rework with zero budget should fail");
    assert!(err.to_string().contains("max_reworks > 0"), "got: {err}");
}

#[test]
fn validation_rejects_llm_judge_with_timeout_secs() {
    use crate::agent_runtime_config::{AgentProfileOverlay, EvalCheck, EvalKind, EvalOnFail, EvalsConfig};

    let mut config = builtin_workflow_config();
    config.agent_profiles.insert("po-reviewer".to_string(), AgentProfileOverlay::default());
    seed_implementation_phase(&mut config);
    let phase = config.phase_definitions.get_mut("implementation").expect("phase exists");
    phase.evals = Some(EvalsConfig {
        pass_threshold: 1.0,
        on_fail: EvalOnFail::Block,
        max_reworks: 0,
        checks: vec![EvalCheck {
            id: "judge".into(),
            kind: EvalKind::LlmJudge,
            command: None,
            args: Vec::new(),
            working_dir: None,
            timeout_secs: Some(30),
            expected_exit: 0,
            agent: Some("po-reviewer".into()),
            prompt: Some("Verdict?".into()),
        }],
    });
    let err = validate_workflow_config(&config).expect_err("llm_judge with timeout_secs should fail");
    assert!(err.to_string().contains("does not support timeout_secs"), "got: {err}");
}

#[test]
fn validate_rejects_zero_budget_max_tokens() {
    let mut config = test_workflow_config_with_standard_pipeline();
    config.workflows.push(workflow_with_budget(
        "zero-budget",
        BudgetConfig { max_tokens: Some(0), max_cost_usd: None, on_exceed: BudgetOnExceed::Pause },
    ));
    let err = validate_workflow_config(&config).expect_err("zero max_tokens must reject");
    assert!(err.to_string().contains("max_tokens must be greater than 0"), "expected max_tokens error, got: {err}");
}

#[test]
fn validate_rejects_non_positive_budget_max_cost() {
    let mut config = test_workflow_config_with_standard_pipeline();
    config.workflows.push(workflow_with_budget(
        "neg-budget",
        BudgetConfig { max_tokens: None, max_cost_usd: Some(-1.5), on_exceed: BudgetOnExceed::Pause },
    ));
    let err = validate_workflow_config(&config).expect_err("negative cost must reject");
    assert!(err.to_string().contains("max_cost_usd must be greater than 0"), "expected max_cost_usd error, got: {err}");
}

#[test]
fn validate_rejects_empty_budget() {
    let mut config = test_workflow_config_with_standard_pipeline();
    config.workflows.push(workflow_with_budget(
        "empty-budget",
        BudgetConfig { max_tokens: None, max_cost_usd: None, on_exceed: BudgetOnExceed::Warn },
    ));
    let err = validate_workflow_config(&config).expect_err("empty budget must reject");
    assert!(
        err.to_string().contains("must declare at least one of max_tokens or max_cost_usd"),
        "expected empty-budget error, got: {err}"
    );
}

#[test]
fn validate_rejects_invalid_on_exceed_via_yaml() {
    let yaml = r#"
workflows:
  - id: bad
    name: Bad
    phases:
      - implementation
    budget:
      max_tokens: 1000
      on_exceed: explode
"#;
    let err = parse_yaml_workflow_config(yaml).expect_err("invalid on_exceed must fail parsing");
    // anyhow chains the underlying serde_yaml error; `{:#}` flattens
    // the full chain into one string we can grep against.
    let chain = format!("{:#}", err);
    assert!(
        chain.contains("explode") || chain.contains("on_exceed") || chain.contains("unknown variant"),
        "expected on_exceed parse error to mention the value or field, got: {chain}"
    );
}

#[test]
fn diagnostic_suggests_skip_for_worktree_no() {
    let yaml = r#"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build it."
    worktree: no
agents:
  swe:
    description: "SWE"
    system_prompt: "You are a SWE."
workflows:
- id: bad
  phases: [build]
"#;
    let err = parse_yaml_workflow_config(yaml).expect_err("worktree: no must error");
    let msg = format!("{:#}", err);
    assert!(msg.contains("invalid `worktree:`"), "expected worktree diagnostic, got: {msg}");
    assert!(
        msg.contains("did you mean `skip`") || msg.contains("did you mean `false`"),
        "expected `skip`/`false` suggestion, got: {msg}"
    );
}

#[test]
fn diagnostic_suggests_auto_for_worktree_yes() {
    let yaml = r#"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build it."
    worktree: yes
agents:
  swe:
    description: "SWE"
    system_prompt: "You are a SWE."
workflows:
- id: bad
  phases: [build]
"#;
    let err = parse_yaml_workflow_config(yaml).expect_err("worktree: yes must error");
    let msg = format!("{:#}", err);
    assert!(msg.contains("invalid `worktree:`"), "expected worktree diagnostic, got: {msg}");
    assert!(
        msg.contains("did you mean `auto`") || msg.contains("did you mean `true`"),
        "expected `auto`/`true` suggestion, got: {msg}"
    );
}

#[test]
fn diagnostic_rejects_worktree_map_with_invalid_mode() {
    let yaml = r#"
phases:
  build:
    mode: agent
    agent: swe
    directive: "Build it."
    worktree:
      mode: skipping
agents:
  swe:
    description: "SWE"
    system_prompt: "You are a SWE."
workflows:
- id: bad
  phases: [build]
"#;
    let err = parse_yaml_workflow_config(yaml).expect_err("worktree.mode: skipping must error");
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("invalid `worktree:` map") || msg.contains("skipping"),
        "expected invalid worktree map diagnostic, got: {msg}"
    );
    assert!(msg.contains("did you mean `skip`") || msg.contains("expected auto"), "expected mode hint, got: {msg}");
}

#[test]
fn diagnostic_suggests_field_for_sub_workflow_typo() {
    let yaml = r#"
phases:
  impl:
    mode: agent
    agent: swe
    directive: "Implement."
agents:
  swe:
    description: "SWE"
    system_prompt: "You are a SWE."
workflows:
- id: bad
  phases:
  - workflow_reff: standard
"#;
    let err = parse_yaml_workflow_config(yaml).expect_err("typo must error");
    let msg = format!("{:#}", err);
    assert!(msg.contains("unknown field"), "expected unknown-field diagnostic, got: {msg}");
    assert!(msg.contains("did you mean `workflow_ref`"), "expected workflow_ref suggestion, got: {msg}");
}

#[test]
fn diagnostic_rejects_phase_entry_with_multi_key_map() {
    let yaml = r#"
phases:
  impl:
    mode: agent
    agent: swe
    directive: "Implement."
agents:
  swe:
    description: "SWE"
    system_prompt: "You are a SWE."
workflows:
- id: bad
  phases:
  - impl: { max_rework_attempts: 1 }
    review: { max_rework_attempts: 1 }
"#;
    let err = parse_yaml_workflow_config(yaml).expect_err("multi-key rich phase entry must error");
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("rich phase entry") || msg.contains("single-key"),
        "expected multi-key phase entry diagnostic, got: {msg}"
    );
}

#[test]
fn diagnostic_carries_source_path_and_line() {
    let temp = tempfile::tempdir().expect("tempdir");
    let yaml = "phases:\n  build:\n    mode: agent\n    worktree: no\n";
    let yaml_path = temp.path().join("phases.yaml");
    std::fs::write(&yaml_path, yaml).expect("write yaml");
    let base = builtin_workflow_config();
    let err = parse_yaml_workflow_config_with_base_and_source(yaml, &base, Some(&yaml_path))
        .expect_err("worktree: no must error");
    let msg = format!("{:#}", err);
    assert!(msg.contains(&yaml_path.display().to_string()), "expected file path, got: {msg}");
    assert!(msg.contains("-->"), "expected rustc-style location arrow, got: {msg}");
}

#[test]
fn diagnostic_does_not_steal_rich_phase_id_starting_with_workflow_underscore() {
    let yaml = r#"
phases:
  workflow_setup:
    mode: agent
    agent: swe
    directive: "Set up."
agents:
  swe:
    description: "SWE"
    system_prompt: "You are a SWE."
workflows:
- id: ok
  phases:
  - workflow_setup: { max_rework_attempts: 1 }
"#;
    let config = parse_yaml_workflow_config(yaml)
        .expect("rich phase id starting with `workflow_` must parse as a rich entry, not a sub-workflow typo");
    let workflow = config.workflows.iter().find(|w| w.id == "ok").expect("workflow ok");
    assert!(matches!(workflow.phases.first().unwrap(), super::WorkflowPhaseEntry::Rich(_)));
}

mod cache_tests {
    use super::*;
    use crate::workflow_config::loading::load_workflow_config_with_metadata;

    fn write_simple_yaml(temp: &std::path::Path, content: &str) {
        let ao_dir = temp.join(".animus");
        std::fs::create_dir_all(&ao_dir).expect("create .animus dir");
        std::fs::write(ao_dir.join("workflows.yaml"), content).expect("write yaml");
    }

    const SAMPLE_YAML: &str = r#"
workflows:
  - id: standard
    name: Cache Test
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#;

    const ALT_YAML: &str = r#"
workflows:
  - id: standard
    name: Cache Test Updated
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#;

    #[test]
    fn workflow_cache_round_trip_returns_same_compiled_output() {
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let _home_guard = EnvVarGuard::set("HOME", temp.path());
        write_simple_yaml(temp.path(), SAMPLE_YAML);

        let first = load_workflow_config_with_metadata(temp.path()).expect("first compile");
        let cache_path = crate::cache::workflow_cache_path(temp.path());
        assert!(cache_path.exists(), "cache file should be written");

        let second = load_workflow_config_with_metadata(temp.path()).expect("second compile (cached)");
        assert_eq!(first.metadata.hash, second.metadata.hash, "cached compile must match fresh compile");
        assert_eq!(first.config.workflows.len(), second.config.workflows.len());
    }

    #[test]
    fn workflow_cache_invalidates_when_yaml_changes() {
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let _home_guard = EnvVarGuard::set("HOME", temp.path());
        write_simple_yaml(temp.path(), SAMPLE_YAML);
        let first = load_workflow_config_with_metadata(temp.path()).expect("first compile");
        let original_name =
            first.config.workflows.iter().find(|w| w.id == "standard").map(|w| w.name.clone()).unwrap_or_default();
        assert_eq!(original_name, "Cache Test");

        // Sleep briefly so mtime ticks past 1s granularity on slow FSes
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write_simple_yaml(temp.path(), ALT_YAML);

        let second = load_workflow_config_with_metadata(temp.path()).expect("second compile after edit");
        let new_name =
            second.config.workflows.iter().find(|w| w.id == "standard").map(|w| w.name.clone()).unwrap_or_default();
        assert_eq!(new_name, "Cache Test Updated", "cache must invalidate on YAML change");
    }

    #[test]
    fn workflow_cache_corrupt_file_falls_through_to_fresh_compile() {
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let _home_guard = EnvVarGuard::set("HOME", temp.path());
        write_simple_yaml(temp.path(), SAMPLE_YAML);

        let cache_path = crate::cache::workflow_cache_path(temp.path());
        std::fs::create_dir_all(cache_path.parent().unwrap()).expect("mkdir cache dir");
        std::fs::write(&cache_path, b"not json at all").expect("write corrupt cache");

        let loaded = load_workflow_config_with_metadata(temp.path()).expect("corrupt cache must fall through");
        assert!(loaded.config.workflows.iter().any(|w| w.id == "standard"));
    }

    #[test]
    fn workflow_cache_disabled_by_env_skips_read_and_write() {
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let _home_guard = EnvVarGuard::set("HOME", temp.path());
        let _disable = EnvVarGuard::set("ANIMUS_DISABLE_WORKFLOW_CACHE", "1");
        write_simple_yaml(temp.path(), SAMPLE_YAML);

        let _ = load_workflow_config_with_metadata(temp.path()).expect("compile");
        let cache_path = crate::cache::workflow_cache_path(temp.path());
        assert!(!cache_path.exists(), "cache write must be skipped when disabled");
    }

    #[test]
    fn workflow_cache_bypassed_when_sources_reference_env_vars() {
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let _home_guard = EnvVarGuard::set("HOME", temp.path());
        let _probe = EnvVarGuard::set("WF_CACHE_PROBE_NAME", "EnvDriven");
        let yaml = r#"
workflows:
  - id: standard
    name: ${WF_CACHE_PROBE_NAME:-Fallback}
    phases:
      - requirements
      - implementation
      - code-review
      - testing
"#;
        write_simple_yaml(temp.path(), yaml);
        let _ = load_workflow_config_with_metadata(temp.path()).expect("compile");
        let cache_path = crate::cache::workflow_cache_path(temp.path());
        assert!(!cache_path.exists(), "cache must not be written when YAML references ${{VAR}} env interpolation");
    }
}

mod unenforced_field_warnings {
    use super::super::validation::{unenforced_project_yaml_warnings, unenforced_yaml_field_warnings};
    use std::fs;

    fn fields(yaml: &str) -> Vec<String> {
        unenforced_yaml_field_warnings(yaml, "test.yaml").into_iter().map(|w| w.field).collect()
    }

    #[test]
    fn daemon_unenforced_keys_are_flagged() {
        let yaml = "daemon:\n  pool_size: 4\n  interval_secs: 10\n  max_task_retries: 3\n  retry_cooldown_secs: 60\n  auto_merge: true\n  auto_pr: false\n  auto_commit_before_merge: true\n  auto_prune_worktrees: true\n";
        let fields = fields(yaml);
        for expected in [
            "daemon.pool_size",
            "daemon.interval_secs",
            "daemon.max_task_retries",
            "daemon.retry_cooldown_secs",
            "daemon.auto_merge",
            "daemon.auto_pr",
            "daemon.auto_commit_before_merge",
            "daemon.auto_prune_worktrees",
        ] {
            assert!(fields.iter().any(|f| f == expected), "expected warning for {expected}, got {fields:?}");
        }
    }

    #[test]
    fn daemon_pool_size_alias_max_agents_is_flagged() {
        let fields = fields("daemon:\n  max_agents: 4\n");
        assert_eq!(fields, vec!["daemon.max_agents"]);
    }

    #[test]
    fn enforced_daemon_keys_are_not_flagged() {
        let yaml = "daemon:\n  auto_run_ready: true\n  active_hours: \"09:00-17:00\"\n  phase_routing: {}\n  mcp: {}\n";
        assert!(fields(yaml).is_empty(), "enforced daemon fields must not warn: {:?}", fields(yaml));
    }

    #[test]
    fn phase_evals_block_is_flagged() {
        let yaml = "phases:\n  impl:\n    mode: agent\n    agent: dev\n    evals:\n      checks:\n        - id: tests\n          kind: command\n          command: cargo\n  review:\n    mode: agent\n    agent: reviewer\n";
        assert_eq!(fields(yaml), vec!["phases.impl.evals"]);
    }

    #[test]
    fn budgets_are_no_longer_flagged_as_unenforced() {
        // Daemon-side enforcement landed: declaring `budget:` must not emit a
        // declared-but-unenforced warning anymore.
        let yaml = "workflows:\n  - id: flow\n    name: Flow\n    budget:\n      max_cost_usd: 5.0\n    phases:\n      - exploration:\n          budget:\n            max_tokens: 1000\n      - impl\n      - workflow_ref: sub-flow\n";
        assert!(fields(yaml).is_empty(), "{:?}", fields(yaml));
    }

    #[test]
    fn clean_yaml_produces_no_warnings() {
        let yaml = "phases:\n  impl:\n    mode: agent\n    agent: dev\nworkflows:\n  - id: flow\n    name: Flow\n    phases:\n      - impl\n";
        assert!(fields(yaml).is_empty());
    }

    #[test]
    fn unparseable_yaml_produces_no_warnings() {
        assert!(fields(": not valid\n[]\n").is_empty());
    }

    #[test]
    fn warning_message_includes_source_and_field() {
        let warnings = unenforced_yaml_field_warnings("daemon:\n  pool_size: 4\n", "/tmp/workflows.yaml");
        assert_eq!(warnings.len(), 1);
        let rendered = warnings[0].to_string();
        assert!(rendered.contains("/tmp/workflows.yaml"), "{rendered}");
        assert!(rendered.contains("`daemon.pool_size`"), "{rendered}");
        assert!(rendered.contains("--pool-size"), "{rendered}");
    }

    #[test]
    fn project_scan_attributes_warnings_to_source_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let animus = temp.path().join(".animus");
        fs::create_dir_all(animus.join("workflows")).unwrap();
        fs::write(animus.join("workflows.yaml"), "daemon:\n  pool_size: 4\n").unwrap();
        fs::write(animus.join("workflows").join("extra.yaml"), "daemon:\n  max_task_retries: 3\n").unwrap();

        let warnings = unenforced_project_yaml_warnings(temp.path());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.field == "daemon.pool_size" && w.source.ends_with("workflows.yaml")));
        assert!(warnings.iter().any(|w| w.field == "daemon.max_task_retries" && w.source.ends_with("extra.yaml")));
    }
}
#[test]
fn missing_skill_yaml_warnings_fire_for_explicit_unresolved_declarations() {
    let yaml = r#"
phases:
  code-review:
    mode: agent
    skills:
      - review-checklist
      - security-audit
agents:
  reviewer:
    system_prompt: Review prompt
    skills:
      - review-checklist
      - ghost-skill
workflows:
  - id: review-flow
    name: Review Flow
    phases:
      - code-review
"#;
    let resolves = |name: &str| name == "review-checklist";
    let warnings = super::validation::missing_skill_yaml_warnings(yaml, ".animus/workflows.yaml", &resolves);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert_eq!(warnings[0].field, "phases.code-review.skills");
    assert_eq!(warnings[0].skill, "security-audit");
    assert_eq!(warnings[1].field, "agents.reviewer.skills");
    assert_eq!(warnings[1].skill, "ghost-skill");
    let message = warnings[0].to_string();
    assert!(message.contains("phases.code-review.skills"), "{message}");
    assert!(message.contains("security-audit"), "{message}");
    assert!(message.contains("animus skill list"), "{message}");
}

#[test]
fn missing_skill_yaml_warnings_skip_resolvable_and_undeclared_skills() {
    // No `skills:` declarations at all (implicit builtin/persona profile
    // skill defaults never appear in project YAML) -> no warnings.
    let yaml_without_skills = r#"
workflows:
  - id: plain
    name: Plain
    phases:
      - implementation
"#;
    let resolves_nothing = |_name: &str| false;
    assert!(super::validation::missing_skill_yaml_warnings(yaml_without_skills, "workflows.yaml", &resolves_nothing)
        .is_empty());

    // Every declared name resolves -> no warnings.
    let yaml_with_skills = r#"
phases:
  review:
    mode: agent
    skills:
      - present-skill
"#;
    let resolves_all = |_name: &str| true;
    assert!(
        super::validation::missing_skill_yaml_warnings(yaml_with_skills, "workflows.yaml", &resolves_all).is_empty()
    );

    // Unparseable YAML yields no warnings (parse errors surface elsewhere).
    assert!(
        super::validation::missing_skill_yaml_warnings(": not yaml [", "workflows.yaml", &resolves_nothing).is_empty()
    );
}

#[test]
fn project_skill_reference_warnings_resolve_against_scoped_skill_sources() {
    let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    // Pin HOME so user-scope skill discovery never reads the real home dir.
    let _home_guard = EnvVarGuard::set("HOME", temp.path());

    let skills_dir = temp.path().join(".animus").join("config").join("skill_definitions");
    fs::create_dir_all(&skills_dir).expect("create project skills dir");
    fs::write(
        skills_dir.join("installed-skill.yaml"),
        r#"
name: installed-skill
description: Present fixture skill
"#,
    )
    .expect("write project skill");

    let ao_dir = temp.path().join(".animus");
    fs::write(
        ao_dir.join("workflows.yaml"),
        r#"
phase_catalog:
  review:
    label: Review
    category: verification
phases:
  review:
    mode: agent
    skills:
      - installed-skill
      - tpyoed-skill
workflows:
  - id: review-flow
    name: Review Flow
    phases:
      - review
"#,
    )
    .expect("write workflow yaml");

    let warnings = super::validation::missing_project_skill_reference_warnings(temp.path());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].field, "phases.review.skills");
    assert_eq!(warnings[0].skill, "tpyoed-skill");
    assert!(warnings[0].source.ends_with("workflows.yaml"), "{}", warnings[0].source);

    // Validation itself stays green: missing skills warn, never error.
    let config = compile_yaml_workflow_files(temp.path()).expect("compile should succeed").expect("config");
    validate_workflow_config(&config).expect("missing skill must not fail validation");
}

#[test]
fn skill_reference_warnings_honor_env_interpolation_in_skill_names() {
    let _lock = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let _home_guard = EnvVarGuard::set("HOME", temp.path());

    let skills_dir = temp.path().join(".animus").join("config").join("skill_definitions");
    fs::create_dir_all(&skills_dir).expect("create project skills dir");
    fs::write(skills_dir.join("review-checklist.yaml"), "name: review-checklist\ndescription: fixture\n")
        .expect("write project skill");

    let ao_dir = temp.path().join(".animus");
    fs::write(
        ao_dir.join("workflows.yaml"),
        r#"
phases:
  review:
    mode: agent
    skills:
      - "${REVIEW_SKILL_FIXTURE:-review-checklist}"
      - "${UNSET_SKILL_FIXTURE_VAR}"
workflows:
  - id: review-flow
    name: Review Flow
    phases:
      - review
"#,
    )
    .expect("write workflow yaml");

    // The default-fallback form interpolates to an existing skill (no
    // warning); the unresolvable required form fails interpolation, so the
    // raw placeholder is skipped rather than reported as a missing skill.
    let warnings = super::validation::missing_project_skill_reference_warnings(temp.path());
    assert!(warnings.is_empty(), "{warnings:?}");
}
