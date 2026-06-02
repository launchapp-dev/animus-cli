use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::types::*;

pub(crate) fn builtin_workflow_config_base() -> WorkflowConfig {
    WorkflowConfig {
        schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: WORKFLOW_CONFIG_VERSION,
        default_workflow_ref: String::new(),
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
        workflows: Vec::new(),
        phase_definitions: BTreeMap::new(),
        agent_profiles: BTreeMap::new(),
        agent_channels: BTreeMap::new(),
        tools_allowlist: Vec::new(),
        mcp_servers: BTreeMap::from([(
            "animus".to_string(),
            McpServerDefinition {
                command: "animus".to_string(),
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

pub fn builtin_workflow_config() -> WorkflowConfig {
    static BUILTIN_CONFIG: OnceLock<WorkflowConfig> = OnceLock::new();
    BUILTIN_CONFIG.get_or_init(builtin_workflow_config_base).clone()
}
