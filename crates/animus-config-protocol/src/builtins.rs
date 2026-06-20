use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::workflow_types::*;

pub fn builtin_workflow_config_base() -> WorkflowConfig {
    WorkflowConfig {
        schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: WORKFLOW_CONFIG_VERSION,
        default_workflow_ref: String::new(),
        checkpoint_retention: WorkflowCheckpointRetentionConfig::default(),
        // Kernel-purification (v0.6): the kernel ships ZERO baked phase UI
        // definitions. Pack overlays + the config_source-sourced workflow
        // overlay populate the catalog.
        phase_catalog: BTreeMap::new(),
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
                oauth: None,
            },
        )]),
        phase_mcp_bindings: BTreeMap::new(),
        tools: BTreeMap::new(),
        integrations: None,
        schedules: Vec::new(),
        triggers: Vec::new(),
        daemon: None,
        secrets: BTreeMap::new(),
    }
}

pub fn builtin_workflow_config() -> WorkflowConfig {
    static BUILTIN_CONFIG: OnceLock<WorkflowConfig> = OnceLock::new();
    BUILTIN_CONFIG.get_or_init(builtin_workflow_config_base).clone()
}
