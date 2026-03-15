use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::types::*;
use super::yaml_compiler::merge_yaml_into_config;
use super::yaml_parser::parse_yaml_workflow_config_with_base;

pub(crate) fn builtin_workflow_config_base() -> WorkflowConfig {
    WorkflowConfig {
        schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: WORKFLOW_CONFIG_VERSION,
        default_workflow_ref: "standard".to_string(),
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
                id: "standard".to_string(),
                name: "Standard".to_string(),
                description: "Default execution flow across requirements, implementation, review, and testing."
                    .to_string(),
                phases: vec![
                    "requirements".to_string().into(),
                    "implementation".to_string().into(),
                    "code-review".to_string().into(),
                    "testing".to_string().into(),
                ],
                post_success: None,
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
                post_success: None,
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
                config: BTreeMap::new(),
                tools: Vec::new(),
                env: BTreeMap::new(),
            },
        )]),
        tools: BTreeMap::new(),
        integrations: None,
        schedules: Vec::new(),
        daemon: None,
    }
}

pub(crate) fn builtin_workflow_yaml_overlays() -> [(&'static str, &'static str); 13] {
    [
        (
            "vision-draft",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/vision-draft.yaml")),
        ),
        (
            "vision-refine",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/vision-refine.yaml")),
        ),
        (
            "requirements-draft",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/requirements-draft.yaml")),
        ),
        (
            "requirements-refine",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/requirements-refine.yaml")),
        ),
        (
            "requirements-execute",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/requirements-execute.yaml")),
        ),
        (
            "task-standard",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/task-standard.yaml")),
        ),
        ("task-ui-ux", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/task-ui-ux.yaml"))),
        (
            "review-cycle",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/review-cycle.yaml")),
        ),
        (
            "task-quick-fix",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/task-quick-fix.yaml")),
        ),
        ("task-gated", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/task-gated.yaml"))),
        (
            "task-triage",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/task-triage.yaml")),
        ),
        (
            "task-refine",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/task-refine.yaml")),
        ),
        (
            "requirement-plan",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/builtin-workflows/requirement-plan.yaml")),
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
