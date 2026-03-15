use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::{discover_bundled_pack_manifests, load_pack_workflow_overlay};

use super::types::*;
use super::yaml_compiler::merge_yaml_into_config;
use super::yaml_parser::parse_yaml_workflow_config_with_base;

const STANDARD_WORKFLOW_REF: &str = "ao.task/standard";
const UI_UX_WORKFLOW_REF: &str = "ao.task/ui-ux";
const REQUIREMENT_TASK_GENERATION_WORKFLOW_REF: &str = "ao.requirement/plan";
const REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF: &str = "ao.requirement/execute";

pub(crate) fn builtin_workflow_config_base() -> WorkflowConfig {
    WorkflowConfig {
        schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: WORKFLOW_CONFIG_VERSION,
        default_workflow_ref: STANDARD_WORKFLOW_REF.to_string(),
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
                id: STANDARD_WORKFLOW_REF.to_string(),
                name: "AO Task Standard".to_string(),
                description: "Canonical pack-qualified task workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: "standard".to_string() })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: UI_UX_WORKFLOW_REF.to_string(),
                name: "AO Task UI UX".to_string(),
                description: "Canonical pack-qualified frontend task workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "ui-ux-standard".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string(),
                name: "AO Requirement Plan".to_string(),
                description: "Canonical pack-qualified requirement planning workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/requirement-plan".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF.to_string(),
                name: "AO Requirement Execute".to_string(),
                description: "Canonical pack-qualified requirement execution workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/requirements-execute".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: "ao.task/quick-fix".to_string(),
                name: "AO Task Quick Fix".to_string(),
                description: "Canonical pack-qualified quick-fix workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/task-quick-fix".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: "ao.task/gated".to_string(),
                name: "AO Task Gated".to_string(),
                description: "Canonical pack-qualified gated workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/task-gated".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: "ao.task/triage".to_string(),
                name: "AO Task Triage".to_string(),
                description: "Canonical pack-qualified triage workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/task-triage".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: "ao.task/refine".to_string(),
                name: "AO Task Refine".to_string(),
                description: "Canonical pack-qualified task refinement workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/task-refine".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: "ao.review/cycle".to_string(),
                name: "AO Review Cycle".to_string(),
                description: "Canonical pack-qualified review cycle workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/review-cycle".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: "ao.requirement/draft".to_string(),
                name: "AO Requirement Draft".to_string(),
                description: "Canonical pack-qualified requirement drafting workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/requirements-draft".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: "ao.requirement/refine".to_string(),
                name: "AO Requirement Refine".to_string(),
                description: "Canonical pack-qualified requirement refinement workflow ref.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: "builtin/requirements-refine".to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
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
            WorkflowDefinition {
                id: "requirement-task-generation".to_string(),
                name: "Requirement Task Generation".to_string(),
                description: "Legacy alias for the canonical requirement planning workflow.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: REQUIREMENT_TASK_GENERATION_WORKFLOW_REF.to_string(),
                })],
                post_success: None,
                variables: Vec::new(),
            },
            WorkflowDefinition {
                id: "requirement-task-generation-run".to_string(),
                name: "Requirement Task Generation Run".to_string(),
                description: "Legacy alias for the canonical requirement execution workflow.".to_string(),
                phases: vec![WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef {
                    workflow_ref: REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF.to_string(),
                })],
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
        phase_mcp_bindings: BTreeMap::new(),
        tools: BTreeMap::new(),
        integrations: None,
        schedules: Vec::new(),
        daemon: None,
    }
}

fn pack_owned_workflow_ids() -> [&'static str; 15] {
    [
        STANDARD_WORKFLOW_REF,
        UI_UX_WORKFLOW_REF,
        REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
        REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF,
        "ao.task/quick-fix",
        "ao.task/gated",
        "ao.task/triage",
        "ao.task/refine",
        "ao.review/cycle",
        "ao.requirement/draft",
        "ao.requirement/refine",
        "builtin/requirements-draft",
        "builtin/requirements-refine",
        "builtin/requirements-execute",
        "builtin/requirement-plan",
    ]
}

pub(crate) fn bundled_kernel_workflow_config_base() -> WorkflowConfig {
    let pack_owned_ids = pack_owned_workflow_ids();
    let mut config = builtin_workflow_config_base();
    config.workflows.retain(|workflow| !pack_owned_ids.iter().any(|id| workflow.id == *id));
    config
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
            let mut config = bundled_kernel_workflow_config_base();
            for (name, yaml) in builtin_workflow_yaml_overlays() {
                let overlay = parse_yaml_workflow_config_with_base(yaml, &config)
                    .unwrap_or_else(|error| panic!("invalid builtin workflow YAML '{name}': {error}"));
                config = merge_yaml_into_config(config, overlay);
            }
            for pack in discover_bundled_pack_manifests()
                .unwrap_or_else(|error| panic!("failed to load bundled pack manifests: {error}"))
            {
                if let Some(overlay) = load_pack_workflow_overlay(&pack, &config).unwrap_or_else(|error| {
                    panic!("invalid bundled pack workflow overlay '{}': {error}", pack.manifest.id)
                }) {
                    config = merge_yaml_into_config(config, overlay);
                }
            }
            config
        })
        .clone()
}
