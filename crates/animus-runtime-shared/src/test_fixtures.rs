//! Test-only fixtures for `animus-runtime-shared`.
//!
//! v0.6 kernel-purification: `orchestrator_core::builtin_agent_runtime_config()`
//! now returns a STRUCTURAL EMPTY config (no baked agents/phases). In production
//! the personas/phases arrive from installed packs and the config_source-sourced
//! workflow overlay. Tests that exercise merge/derivation/contract-fallback
//! behavior seed this fixture (reproducing the pre-v0.6 baked personas/phases)
//! instead of relying on the kernel builtin.

use std::collections::BTreeMap;

use orchestrator_config::agent_runtime_config::{
    AgentCommunicationConfig, AgentHooksConfig, AgentMemoryConfig, AgentProfile, AgentRuntimeConfig, AgentToolPolicy,
    Idempotency, PhaseDecisionContract, PhaseExecutionDefinition, PhaseExecutionMode, PhaseOutputContract,
    AGENT_RUNTIME_CONFIG_SCHEMA_ID, AGENT_RUNTIME_CONFIG_VERSION,
};
use serde_json::json;

fn profile(description: &str, system_prompt: &str, role: Option<&str>) -> AgentProfile {
    AgentProfile {
        name: None,
        description: description.to_string(),
        system_prompt: system_prompt.to_string(),
        system_prompt_file: None,
        role: role.map(ToOwned::to_owned),
        persona: None,
        memory: AgentMemoryConfig::default(),
        communication: AgentCommunicationConfig::default(),
        mcp_servers: Vec::new(),
        tool_policy: AgentToolPolicy::default(),
        skills: vec![],
        application_chat_controls: None,
        capabilities: BTreeMap::new(),
        tool: None,
        tool_profile: None,
        model: None,
        fallback_models: vec![],
        models: vec![],
        fallback_tools: vec![],
        reasoning_effort: None,
        permission_mode: None,
        web_search: None,
        network_access: None,
        timeout_secs: None,
        max_attempts: None,
        retry_on: vec![],
        no_retry_on: vec![],
        extra_args: vec![],
        codex_config_overrides: vec![],
        max_continuations: None,
        approval_policy: None,
        hooks: AgentHooksConfig::default(),
        mcp_server_configs: None,
        structured_capabilities: None,
        project_overrides: None,
    }
}

fn agent_phase(agent_id: &str, directive: &str) -> PhaseExecutionDefinition {
    PhaseExecutionDefinition {
        mode: PhaseExecutionMode::Agent,
        agent_id: Some(agent_id.to_string()),
        directive: Some(directive.to_string()),
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
    }
}

/// A valid agent-runtime config carrying the standard personas/phases the
/// runtime-shared accessors historically read out of the kernel builtin
/// (`default`/`po`/`swe` profiles; `requirements`/`implementation` phases with
/// their decision/output contracts). Mirrors the kernel's own test fixture.
pub(crate) fn seeded_agent_runtime_config() -> AgentRuntimeConfig {
    let mut agents = BTreeMap::new();
    agents.insert(
        "default".to_string(),
        profile(
            "Default workflow phase agent profile",
            "You are the workflow phase execution agent. Produce deterministic, repository-safe outputs and keep changes scoped to the active phase.",
            None,
        ),
    );
    agents.insert(
        "po".to_string(),
        profile("Product Owner persona.", "You are the Product Owner agent.", Some("product_owner")),
    );
    agents.insert(
        "swe".to_string(),
        profile(
            "Software Engineer persona for implementation, testing, and code review.",
            "You are the software engineer execution agent. Implement production-ready code changes, add or update tests, and perform rigorous code review while keeping edits minimal and verifiable.",
            Some("software_engineer"),
        ),
    );

    let mut phases = BTreeMap::new();
    phases.insert("default".to_string(), agent_phase("default", "Execute the current workflow phase."));
    phases.insert(
        "requirements".to_string(),
        PhaseExecutionDefinition {
            decision_contract: Some(PhaseDecisionContract {
                required_evidence: Vec::new(),
                min_confidence: 0.6,
                max_risk: orchestrator_core::types::WorkflowDecisionRisk::Medium,
                allow_missing_decision: true,
                extra_json_schema: None,
                fields: BTreeMap::new(),
            }),
            ..agent_phase("po", "Clarify implementation scope, constraints, and acceptance criteria.")
        },
    );
    phases.insert(
        "implementation".to_string(),
        PhaseExecutionDefinition {
            output_contract: Some(PhaseOutputContract {
                kind: "implementation_result".to_string(),
                required_fields: vec!["commit_message".to_string()],
                fields: BTreeMap::new(),
            }),
            output_json_schema: Some(json!({
                "type": "object",
                "required": ["kind", "commit_message"],
                "properties": {
                    "kind": {"const": "implementation_result"},
                    "commit_message": {"type": "string", "minLength": 1}
                },
                "additionalProperties": true
            })),
            decision_contract: Some(PhaseDecisionContract {
                required_evidence: Vec::new(),
                min_confidence: 0.7,
                max_risk: orchestrator_core::types::WorkflowDecisionRisk::Medium,
                allow_missing_decision: true,
                extra_json_schema: None,
                fields: BTreeMap::new(),
            }),
            runtime: Some(orchestrator_core::AgentRuntimeOverrides {
                model: Some("claude-sonnet-4-6".to_string()),
                ..Default::default()
            }),
            ..agent_phase("swe", "Implement production-quality code for this task.")
        },
    );

    AgentRuntimeConfig {
        schema: AGENT_RUNTIME_CONFIG_SCHEMA_ID.to_string(),
        version: AGENT_RUNTIME_CONFIG_VERSION,
        tools_allowlist: vec!["cargo".to_string()],
        agents,
        phases,
        cli_tools: BTreeMap::new(),
    }
}
