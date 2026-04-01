use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::types::*;
use super::yaml_compiler::merge_yaml_into_config;
use super::yaml_parser::parse_yaml_workflow_config_with_base;

pub(crate) fn builtin_workflow_config_base() -> WorkflowConfig {
    WorkflowConfig {
        schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: WORKFLOW_CONFIG_VERSION,
        default_workflow_ref: "standard-workflow".to_string(),
        checkpoint_retention: WorkflowCheckpointRetentionConfig::default(),
        phase_catalog: BTreeMap::from([
            (
                "requirements".to_string(),
                phase_ui_definition(
                    "Requirements",
                    "Clarify scope, constraints, and acceptance criteria.",
                    "planning",
                    &["planning", "scope"],
                ),
            ),
            (
                "research".to_string(),
                phase_ui_definition(
                    "Research",
                    "Gather implementation evidence and references for execution.",
                    "planning",
                    &["research"],
                ),
            ),
            (
                "ux-research".to_string(),
                phase_ui_definition(
                    "UX Research",
                    "Document interaction patterns, user journeys, and accessibility constraints.",
                    "design",
                    &["design", "ux"],
                ),
            ),
            (
                "wireframe".to_string(),
                phase_ui_definition(
                    "Wireframe",
                    "Produce concrete wireframes and interaction states.",
                    "design",
                    &["design", "wireframe"],
                ),
            ),
            (
                "mockup-review".to_string(),
                phase_ui_definition(
                    "Mockup Review",
                    "Validate mockups against requirements and UX constraints.",
                    "review",
                    &["design", "review"],
                ),
            ),
            (
                "implementation".to_string(),
                phase_ui_definition(
                    "Implementation",
                    "Deliver production-quality implementation changes.",
                    "build",
                    &["build", "code"],
                ),
            ),
            (
                "code-review".to_string(),
                phase_ui_definition(
                    "Code Review",
                    "Review quality, risks, and maintainability before completion.",
                    "review",
                    &["review", "quality"],
                ),
            ),
            (
                "testing".to_string(),
                phase_ui_definition(
                    "Testing",
                    "Run and update test coverage for the delivered changes.",
                    "qa",
                    &["qa", "testing"],
                ),
            ),
        ]),
        workflows: vec![
            WorkflowDefinition {
                id: "ao.vision/draft".to_string(),
                name: "AO Vision Draft".to_string(),
                description: "Canonical pack-qualified vision drafting workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/vision-draft".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: "ao.vision/refine".to_string(),
                name: "AO Vision Refine".to_string(),
                description: "Canonical pack-qualified vision refinement workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/vision-refine".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: "standard-workflow".to_string(),
                name: "Standard Workflow".to_string(),
                description: "Default task delivery workflow for this repository.".to_string(),
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
            },
            WorkflowDefinition {
                id: "ui-ux-standard".to_string(),
                name: "UI UX Standard".to_string(),
                description: "Frontend-oriented flow with UX research, wireframes, and mockup review gates."
                    .to_string(),
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
            },
        ],
        phase_definitions: BTreeMap::new(),
        agent_profiles: BTreeMap::new(),
        tools_allowlist: Vec::new(),
        mcp_servers: BTreeMap::from([(
            "ao".to_string(),
            McpServerDefinition {
                command: "ao".to_string(),
                args: vec!["mcp".to_string(), "serve".to_string()],
                transport: Some("stdio".to_string()),
                url: None,
                config: BTreeMap::new(),
                tools: Vec::new(),
                env: BTreeMap::new(),
            },
        )]),
        phase_mcp_bindings: BTreeMap::new(),
        tools: BTreeMap::new(),
        integrations: None,
        schedules: Vec::new(),
        triggers: Vec::new(),
        daemon: None,
    }
}

pub(crate) fn builtin_workflow_yaml_overlays() -> [(&'static str, &'static str); 2] {
    [
        (
            "vision-draft",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/vision-draft.yaml")),
        ),
        (
            "vision-refine",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/vision-refine.yaml")),
        ),
    ]
}

pub fn builtin_workflow_config() -> WorkflowConfig {
    static BUILTIN_CONFIG: OnceLock<WorkflowConfig> = OnceLock::new();
    BUILTIN_CONFIG
        .get_or_init(|| {
            let mut config = builtin_workflow_config_base();
            for (name, yaml) in builtin_workflow_yaml_overlays() {
                let overlay = parse_yaml_workflow_config_with_base(yaml, &config)
                    .unwrap_or_else(|error| panic!("invalid builtin workflow YAML '{name}': {error}"));
                config = merge_yaml_into_config(config, overlay);
            }
            config
        })
        .clone()
}
