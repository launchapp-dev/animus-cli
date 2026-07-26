mod cli_error;
mod output;
mod parsing;
mod runner;
mod tables;

pub(crate) use cli_error::*;
pub(crate) use output::*;
pub(crate) use parsing::*;
pub(crate) use runner::*;
pub(crate) use tables::*;

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Test fixture reproducing the personas/phases the kernel used to bake in
/// before the v0.6 kernel-purification refactor. The kernel now ships an EMPTY
/// `builtin_agent_runtime_config()`; these agents/phases are supplied by packs
/// and the config_source-sourced workflow overlay at runtime. CLI tests that
/// exercise merge/derivation/lookup/runtime behavior seed this valid base
/// instead of relying on the (now empty, and thus invalid) builtin.
#[cfg(test)]
pub(crate) fn seeded_agent_runtime_config() -> orchestrator_config::AgentRuntimeConfig {
    use orchestrator_config::types::WorkflowDecisionRisk;
    use orchestrator_config::{
        AgentCommunicationConfig, AgentHooksConfig, AgentMemoryConfig, AgentProfile, AgentRuntimeConfig,
        AgentRuntimeOverrides, AgentToolPolicy, Idempotency, PhaseDecisionContract, PhaseExecutionDefinition,
        PhaseExecutionMode, PhaseOutputContract, AGENT_RUNTIME_CONFIG_SCHEMA_ID, AGENT_RUNTIME_CONFIG_VERSION,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    let implementation_output_contract = PhaseOutputContract {
        kind: "implementation_result".to_string(),
        required_fields: vec!["commit_message".to_string()],
        fields: BTreeMap::new(),
    };
    let swe_mcp_servers = vec!["animus".to_string()];
    let swe_tool_policy = AgentToolPolicy {
        allow: vec![
            "task.*".to_string(),
            "workflow.*".to_string(),
            "output.*".to_string(),
            "history.*".to_string(),
            "errors.*".to_string(),
        ],
        deny: vec!["project.remove".to_string(), "daemon.stop".to_string(), "requirements.delete".to_string()],
    };
    let swe_capabilities = BTreeMap::from([
        ("planning".to_string(), false),
        ("queue_management".to_string(), false),
        ("scheduling".to_string(), false),
        ("requirements_authoring".to_string(), false),
        ("acceptance_validation".to_string(), false),
        ("implementation".to_string(), true),
        ("testing".to_string(), true),
        ("code_review".to_string(), true),
    ]);

    let make_profile = |description: &str,
                        system_prompt: &str,
                        role: Option<&str>,
                        mcp_servers: Vec<String>,
                        tool_policy: AgentToolPolicy,
                        skills: Vec<String>,
                        capabilities: BTreeMap<String, bool>| {
        AgentProfile {
            name: None,
            description: description.to_string(),
            system_prompt: system_prompt.to_string(),
            system_prompt_file: None,
            role: role.map(ToOwned::to_owned),
            persona: None,
            memory: AgentMemoryConfig::default(),
            communication: AgentCommunicationConfig::default(),
            mcp_servers,
            tool_policy,
            skills,
            application_chat_controls: None,
            capabilities,
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
    };

    let make_agent_phase = |agent_id: &str, directive: &str| PhaseExecutionDefinition {
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
    };

    AgentRuntimeConfig {
        schema: AGENT_RUNTIME_CONFIG_SCHEMA_ID.to_string(),
        version: AGENT_RUNTIME_CONFIG_VERSION,
        tools_allowlist: vec![
            "cargo".to_string(),
            "npm".to_string(),
            "pnpm".to_string(),
            "yarn".to_string(),
            "bun".to_string(),
            "pytest".to_string(),
            "go".to_string(),
            "bash".to_string(),
            "sh".to_string(),
            "make".to_string(),
            "just".to_string(),
        ],
        agents: BTreeMap::from([
            (
                "default".to_string(),
                make_profile(
                    "Default workflow phase agent profile",
                    "You are the workflow phase execution agent. Produce deterministic, repository-safe outputs and keep changes scoped to the active phase.",
                    None,
                    Vec::new(),
                    AgentToolPolicy::default(),
                    vec![],
                    BTreeMap::new(),
                ),
            ),
            (
                "implementation".to_string(),
                make_profile(
                    "Compatibility alias for the software engineer persona.",
                    "You are the software engineer execution agent. Implement production-ready code changes, add or update tests, and perform rigorous code review while keeping edits minimal and verifiable.",
                    Some("software_engineer"),
                    swe_mcp_servers.clone(),
                    swe_tool_policy.clone(),
                    vec![
                        "implementation".to_string(),
                        "testing".to_string(),
                        "code-review".to_string(),
                        "debugging".to_string(),
                    ],
                    swe_capabilities.clone(),
                ),
            ),
            (
                "em".to_string(),
                make_profile(
                    "Engineering Manager persona for prioritization, queue management, and scheduling.",
                    "You are the Engineering Manager agent. Prioritize work, manage queue health, sequence delivery safely, and keep execution plans realistic and dependency-aware.",
                    Some("engineering_manager"),
                    vec!["animus".to_string()],
                    AgentToolPolicy {
                        allow: vec!["task.*".to_string(), "workflow.*".to_string(), "history.*".to_string()],
                        deny: vec![
                            "task.delete".to_string(),
                            "requirements.delete".to_string(),
                            "project.remove".to_string(),
                            "git.*".to_string(),
                        ],
                    },
                    vec![
                        "prioritization".to_string(),
                        "queue-management".to_string(),
                        "scheduling".to_string(),
                        "risk-management".to_string(),
                    ],
                    BTreeMap::from([
                        ("planning".to_string(), true),
                        ("queue_management".to_string(), true),
                        ("scheduling".to_string(), true),
                        ("requirements_authoring".to_string(), false),
                        ("acceptance_validation".to_string(), true),
                        ("implementation".to_string(), false),
                        ("testing".to_string(), false),
                        ("code_review".to_string(), true),
                    ]),
                ),
            ),
            (
                "po".to_string(),
                make_profile(
                    "Product Owner persona for requirements, vision, acceptance criteria, and deliverable validation.",
                    "You are the Product Owner agent. Refine requirements into clear acceptance criteria, align work to product vision, and validate deliverables against user outcomes.",
                    Some("product_owner"),
                    vec!["animus".to_string()],
                    AgentToolPolicy {
                        allow: vec![
                            "vision.*".to_string(),
                            "requirements.*".to_string(),
                            "task.*".to_string(),
                            "review.*".to_string(),
                            "qa.*".to_string(),
                            "workflow.*".to_string(),
                        ],
                        deny: vec!["task.delete".to_string(), "project.remove".to_string(), "git.*".to_string()],
                    },
                    vec![
                        "vision-alignment".to_string(),
                        "requirements-management".to_string(),
                        "acceptance-criteria".to_string(),
                        "deliverable-validation".to_string(),
                    ],
                    BTreeMap::from([
                        ("planning".to_string(), true),
                        ("queue_management".to_string(), false),
                        ("scheduling".to_string(), false),
                        ("requirements_authoring".to_string(), true),
                        ("acceptance_validation".to_string(), true),
                        ("implementation".to_string(), false),
                        ("testing".to_string(), false),
                        ("code_review".to_string(), true),
                    ]),
                ),
            ),
            (
                "swe".to_string(),
                make_profile(
                    "Software Engineer persona for implementation, testing, and code review.",
                    "You are the software engineer execution agent. Implement production-ready code changes, add or update tests, and perform rigorous code review while keeping edits minimal and verifiable.",
                    Some("software_engineer"),
                    swe_mcp_servers,
                    swe_tool_policy,
                    vec![
                        "implementation".to_string(),
                        "testing".to_string(),
                        "code-review".to_string(),
                        "debugging".to_string(),
                    ],
                    swe_capabilities,
                ),
            ),
        ]),
        phases: BTreeMap::from([
            (
                "default".to_string(),
                make_agent_phase("default", "Execute the current workflow phase with production-quality output."),
            ),
            (
                "requirements".to_string(),
                PhaseExecutionDefinition {
                    decision_contract: Some(PhaseDecisionContract {
                        required_evidence: Vec::new(),
                        min_confidence: 0.6,
                        max_risk: WorkflowDecisionRisk::Medium,
                        allow_missing_decision: true,
                        extra_json_schema: None,
                        fields: BTreeMap::new(),
                    }),
                    ..make_agent_phase(
                        "po",
                        "Clarify implementation scope, constraints, and acceptance criteria. Update docs and implementation notes as needed.",
                    )
                },
            ),
            (
                "research".to_string(),
                PhaseExecutionDefinition {
                    runtime: Some(AgentRuntimeOverrides {
                        web_search: Some(true),
                        timeout_secs: Some(900),
                        ..AgentRuntimeOverrides::default()
                    }),
                    ..make_agent_phase(
                        "default",
                        "Gather external and codebase evidence needed to de-risk the next implementation step.",
                    )
                },
            ),
            (
                "ux-research".to_string(),
                make_agent_phase(
                    "default",
                    "Produce a UX brief from requirements and user flows. Identify key screens, interactions, and accessibility constraints.",
                ),
            ),
            (
                "wireframe".to_string(),
                make_agent_phase(
                    "default",
                    "Create concrete UI mockups/wireframes in the repository under mockups/. Prefer production-like React-oriented layouts and realistic states.",
                ),
            ),
            (
                "mockup-review".to_string(),
                make_agent_phase(
                    "default",
                    "Review mockups against linked requirements. Resolve mismatches, improve usability, and ensure acceptance criteria traceability.",
                ),
            ),
            (
                "implementation".to_string(),
                PhaseExecutionDefinition {
                    output_contract: Some(implementation_output_contract),
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
                        max_risk: WorkflowDecisionRisk::Medium,
                        allow_missing_decision: true,
                        extra_json_schema: None,
                        fields: BTreeMap::new(),
                    }),
                    ..make_agent_phase(
                        "swe",
                        "Implement production-quality code for this task. Keep changes focused and executable.",
                    )
                },
            ),
        ]),
        cli_tools: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentRunArgs;
    use anyhow::anyhow;
    use protocol::test_utils::EnvVarGuard;
    use protocol::RunId;

    fn make_agent_run_args() -> AgentRunArgs {
        AgentRunArgs {
            run_id: None,
            tool: "claude".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
            prompt: Some("test".to_string()),
            reasoning_effort: None,
            permission_mode: None,
            approvals: false,
            cwd: None,
            timeout_secs: None,
            context_json: None,
            runtime_contract_json: None,
            detach: false,
            stream: true,
            save_jsonl: false,
            jsonl_dir: None,
            start_runner: false,
            agent: None,
            skill: None,
            mcp_server: Vec::new(),
            no_animus_mcp: false,
        }
    }

    #[test]
    fn run_dir_defaults_to_scoped_runtime_runs_root() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project-root");
        std::fs::create_dir_all(&project_root).expect("project dir should be created");
        let run_id = RunId("trace-run-010".to_string());

        let resolved = run_dir(project_root.to_string_lossy().as_ref(), &run_id, None);
        let scope = protocol::repository_scope_for_path(&project_root);
        let expected = dirs::home_dir()
            .expect("home directory should resolve")
            .join(".animus")
            .join(scope)
            .join("runs")
            .join(&run_id.0);

        assert_eq!(resolved, expected);
        assert_ne!(resolved, project_root.join(".animus").join("runs").join(&run_id.0));
    }

    #[test]
    fn run_dir_uses_base_override_when_provided() {
        let project_root = tempfile::tempdir().expect("tempdir should be created");
        let override_root = tempfile::tempdir().expect("tempdir should be created");
        let run_id = RunId("trace-run-override".to_string());

        let resolved = run_dir(
            project_root.path().to_string_lossy().as_ref(),
            &run_id,
            Some(override_root.path().to_string_lossy().as_ref()),
        );
        assert_eq!(resolved, override_root.path().join(&run_id.0));
    }

    #[test]
    fn classify_error_maps_expected_exit_codes() {
        let invalid = invalid_input_error("invalid status");
        let confirmation = invalid_input_error("CONFIRMATION_REQUIRED: rerun command with --confirm TASK-1");
        let unavailable = unavailable_error("failed to connect to runner");
        let not_found = not_found_error("task not found");
        let conflict = conflict_error("architecture entity already exists");
        let internal = anyhow!("runner returned status payload while waiting for control response");

        assert_eq!(classify_exit_code(&invalid), 2);
        assert_eq!(classify_exit_code(&confirmation), 2);
        assert_eq!(classify_exit_code(&not_found), 3);
        assert_eq!(classify_exit_code(&conflict), 4);
        assert_eq!(classify_exit_code(&unavailable), 5);
        assert_eq!(classify_exit_code(&internal), 1);
    }

    #[test]
    fn collect_json_payload_lines_keeps_json_objects_and_arrays_only() {
        let input = "\n{\"kind\":\"event\"}\nnot-json\n[1,2,3]\n123\n";
        let rows = collect_json_payload_lines(input);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "{\"kind\":\"event\"}");
        assert!(rows[0].1.is_object());
        assert!(rows[1].1.is_array());
    }

    #[test]
    fn build_runtime_contract_includes_rich_shape() {
        let contract = build_runtime_contract(
            "codex",
            protocol::default_model_for_tool("codex").expect("default model for codex should be configured"),
            "hello world",
        )
        .expect("codex runtime contract should be generated");

        assert_eq!(contract.pointer("/cli/name").and_then(serde_json::Value::as_str), Some("codex"));
        assert_eq!(
            contract.pointer("/cli/capabilities/supports_tool_use").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(contract.get("mcp").is_some());
    }

    #[test]
    fn build_agent_context_rejects_cwd_outside_project() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&project).expect("project dir should be created");
        std::fs::create_dir_all(&outside).expect("outside dir should be created");

        let mut args = make_agent_run_args();
        args.cwd = Some(outside.to_string_lossy().to_string());

        let error = build_agent_context(&args, project.to_string_lossy().as_ref())
            .expect_err("cwd outside project must be rejected");
        assert!(error.to_string().contains("Security violation"));
    }

    #[test]
    fn build_agent_context_accepts_relative_cwd_inside_project() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let project = temp.path().join("project");
        let nested = project.join("src");
        std::fs::create_dir_all(&nested).expect("nested dir should be created");

        let mut args = make_agent_run_args();
        args.cwd = Some("src".to_string());

        let context = build_agent_context(&args, project.to_string_lossy().as_ref())
            .expect("relative cwd inside project should be accepted");
        let expected = nested.canonicalize().expect("nested path should canonicalize").to_string_lossy().to_string();
        assert_eq!(context.get("cwd").and_then(serde_json::Value::as_str), Some(expected.as_str()));
    }
}
