use super::*;

#[test]
fn missing_config_reports_actionable_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let err = load_agent_runtime_config(temp.path()).expect_err("missing config should fail");
    let message = err.to_string();
    assert!(message.contains(".ao/workflows.yaml"));
    assert!(message.contains(".ao/workflows/*.yaml"));
}

#[test]
fn ensure_creates_config_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    ensure_agent_runtime_config_file(temp.path()).expect("ensure file");

    let workflows_dir = crate::workflow_config::yaml_workflows_dir(temp.path());
    assert!(workflows_dir.join("custom.yaml").exists());
    assert!(workflows_dir.join("standard-workflow.yaml").exists());
    assert!(workflows_dir.join("hotfix-workflow.yaml").exists());
    assert!(workflows_dir.join("research-workflow.yaml").exists());
}

#[test]
fn runtime_resolution_merges_workflow_config_overlays() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut workflow = crate::workflow_config::builtin_workflow_config();
    let builtin = builtin_agent_runtime_config();
    let mut triager = builtin.agent_profile("triager").expect("builtin triager profile should exist").clone();
    triager.mcp_servers.clear();
    workflow.agent_profiles.insert("triager".to_string(), triager);
    workflow.phase_definitions.insert(
        "triage".to_string(),
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("triager".to_string()),
            directive: Some("triage".to_string()),
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: Some(PhaseDecisionContract {
                required_evidence: Vec::new(),
                min_confidence: 0.7,
                max_risk: crate::types::WorkflowDecisionRisk::Medium,
                allow_missing_decision: false,
                extra_json_schema: None,
                fields: BTreeMap::new(),
            }),
            retry: None,
            skills: Vec::new(),
            command: None,
            manual: None,
            default_tool: None,
        },
    );
    workflow.tools.insert(
        "custom-runner".to_string(),
        crate::workflow_config::ToolDefinition {
            executable: "custom-runner-bin".to_string(),
            supports_mcp: true,
            supports_write: true,
            context_window: Some(42_000),
            base_args: vec![],
        },
    );
    crate::workflow_config::write_workflow_config(temp.path(), &workflow).expect("write workflow config");

    let resolved = load_agent_runtime_config_or_default(temp.path());
    let triage = resolved.phase_decision_contract("triage").expect("triage contract");
    assert!(!triage.allow_missing_decision);
}

#[test]
fn builtin_defaults_expose_phase_definitions() {
    let config = builtin_agent_runtime_config();
    assert_eq!(config.phase_agent_id("requirements"), Some("po"));
    assert_eq!(config.phase_agent_id("implementation"), Some("swe"));
    assert_eq!(config.phase_agent_id("code-review"), Some("swe"));
    assert_eq!(config.phase_agent_id("testing"), Some("swe"));
    assert!(config.phase_output_json_schema("implementation").is_some());
}

#[test]
fn builtin_phase_prompts_resolve_to_expected_personas() {
    let config = builtin_agent_runtime_config();
    for (phase_id, agent_id) in
        [("requirements", "po"), ("implementation", "swe"), ("code-review", "swe"), ("testing", "swe")]
    {
        let expected_prompt =
            config.agent_profile(agent_id).expect("phase agent profile should exist").system_prompt.trim().to_string();
        assert_eq!(config.phase_agent_id(phase_id), Some(agent_id));
        assert_eq!(config.phase_system_prompt(phase_id), Some(expected_prompt.as_str()));
    }
}

#[test]
fn builtin_phase_decision_contracts_match_expected_evidence_requirements() {
    let config = builtin_agent_runtime_config();

    assert_eq!(config.phase_decision_contract("triage").map(|contract| contract.allow_missing_decision), Some(false));
    assert_eq!(
        config.phase_decision_contract("refine-requirements").map(|contract| contract.allow_missing_decision),
        Some(false)
    );
    assert_eq!(
        config.phase_decision_contract("requirements").map(|contract| contract.required_evidence.clone()),
        Some(Vec::new())
    );
    assert_eq!(
        config.phase_decision_contract("implementation").map(|contract| contract.required_evidence.clone()),
        Some(vec![crate::types::PhaseEvidenceKind::FilesModified])
    );
    assert_eq!(
        config.phase_decision_contract("code-review").map(|contract| contract.required_evidence.clone()),
        Some(vec![crate::types::PhaseEvidenceKind::CodeReviewClean])
    );
    assert_eq!(
        config.phase_decision_contract("testing").map(|contract| contract.required_evidence.clone()),
        Some(vec![crate::types::PhaseEvidenceKind::TestsPassed])
    );
}

#[test]
fn builtin_defaults_include_em_po_and_swe_profiles() {
    let config = builtin_agent_runtime_config();
    for agent_id in ["em", "po", "swe"] {
        let profile = config.agent_profile(agent_id).expect("builtin profile should exist");
        assert!(!profile.description.trim().is_empty());
        assert!(!profile.system_prompt.trim().is_empty());
        assert!(profile.role.as_deref().is_some_and(|role| !role.is_empty()));
        assert!(!profile.capabilities.is_empty());
        assert!(!profile.mcp_servers.is_empty());
    }
}

#[test]
fn builtin_persona_skills_resolve_against_builtin_catalog() {
    let config = builtin_agent_runtime_config();
    validate_agent_runtime_config(&config).expect("builtin runtime config should validate against builtin skills");

    for agent_id in ["implementation", "em", "po", "swe"] {
        let profile = config.agent_profile(agent_id).expect("builtin profile should exist");
        for skill_name in &profile.skills {
            let resolved = crate::skill_resolution::resolve_skill(
                skill_name,
                &[crate::skill_scoping::load_builtin_skills().expect("builtin skills should load")],
            )
            .expect("builtin persona skill should resolve");
            assert_eq!(resolved.definition.name, *skill_name);
        }
    }
}

#[test]
fn runtime_validation_reports_missing_skill_loudly() {
    let mut config = builtin_agent_runtime_config();
    config.agents.get_mut("swe").expect("swe profile should exist").skills.push("missing-skill".to_string());

    let err = validate_agent_runtime_config(&config).expect_err("missing skill should fail validation");
    let message = err.to_string();
    assert!(message.contains("agents['swe'].skills"));
    assert!(message.contains("missing-skill"));
}

#[test]
fn builtin_persona_capabilities_and_tool_patterns_are_role_specific() {
    let config = builtin_agent_runtime_config();
    let em = config.agent_profile("em").expect("em profile should exist");
    let po = config.agent_profile("po").expect("po profile should exist");
    let swe = config.agent_profile("swe").expect("swe profile should exist");

    assert_eq!(em.capabilities.get("queue_management"), Some(&true));
    assert_eq!(em.capabilities.get("scheduling"), Some(&true));
    assert_eq!(em.capabilities.get("implementation"), Some(&false));

    assert_eq!(po.capabilities.get("requirements_authoring"), Some(&true));
    assert_eq!(po.capabilities.get("acceptance_validation"), Some(&true));
    assert_eq!(po.capabilities.get("implementation"), Some(&false));

    assert_eq!(swe.capabilities.get("implementation"), Some(&true));
    assert_eq!(swe.capabilities.get("testing"), Some(&true));
    assert_eq!(swe.capabilities.get("code_review"), Some(&true));
    assert_eq!(swe.capabilities.get("planning"), Some(&false));

    assert!(em.mcp_servers.iter().any(|server| server == "ao"));
    assert!(po.mcp_servers.iter().any(|server| server == "ao"));
    assert!(swe.mcp_servers.iter().any(|server| server == "ao"));
}

#[test]
fn builtin_json_and_fallback_match_persona_phase_defaults() {
    let from_json = serde_json::from_str::<AgentRuntimeConfig>(BUILTIN_AGENT_RUNTIME_CONFIG_JSON)
        .expect("builtin json should deserialize");
    validate_agent_runtime_config(&from_json).expect("builtin json should validate");
    let fallback = hardcoded_builtin_agent_runtime_config();

    for phase_id in ["requirements", "implementation", "code-review", "testing"] {
        assert_eq!(from_json.phase_agent_id(phase_id), fallback.phase_agent_id(phase_id));
        assert_eq!(
            from_json.phase_decision_contract(phase_id).map(|contract| (
                contract.required_evidence.clone(),
                contract.min_confidence,
                contract.max_risk.clone(),
                contract.allow_missing_decision,
                contract.extra_json_schema.clone()
            )),
            fallback.phase_decision_contract(phase_id).map(|contract| (
                contract.required_evidence.clone(),
                contract.min_confidence,
                contract.max_risk.clone(),
                contract.allow_missing_decision,
                contract.extra_json_schema.clone()
            ))
        );
    }

    for agent_id in ["em", "po", "swe"] {
        let json_profile = from_json.agent_profile(agent_id).expect("json profile should exist");
        let fallback_profile = fallback.agent_profile(agent_id).expect("fallback profile should exist");
        assert_eq!(json_profile.role, fallback_profile.role);
        assert_eq!(json_profile.mcp_servers, fallback_profile.mcp_servers);
        assert_eq!(json_profile.tool_policy, fallback_profile.tool_policy);
        assert_eq!(json_profile.skills, fallback_profile.skills);
        assert_eq!(json_profile.capabilities, fallback_profile.capabilities);
    }
}

#[test]
fn phase_decision_contract_lookup_is_case_insensitive() {
    let config = builtin_agent_runtime_config();
    assert!(config.phase_decision_contract("code-review").is_some());
    assert!(config.phase_decision_contract("CODE-REVIEW").is_some());
}

#[test]
fn builtin_defaults_mark_review_as_structured_output() {
    let config = builtin_agent_runtime_config();
    assert!(config.is_structured_output_phase("code-review"));
    assert!(config.is_structured_output_phase("implementation"));
    assert!(config.is_structured_output_phase("testing"));
}

#[test]
fn structured_output_phase_accepts_trimmed_phase_ids() {
    let config = builtin_agent_runtime_config();
    assert!(config.is_structured_output_phase(" implementation "));
    assert!(config.is_structured_output_phase(" CODE-REVIEW "));
    assert!(config.is_structured_output_phase(" testing "));
}

#[test]
fn builtin_runtime_supports_extended_builtin_workflow_phases() {
    let config = builtin_agent_runtime_config();

    assert_eq!(config.phase_agent_id("triage"), Some("triager"));
    assert_eq!(config.phase_agent_id("refine-requirements"), Some("requirements-refiner"));
    assert_eq!(config.phase_agent_id("requirement-task-generation"), Some("requirements-planner"));
    assert_eq!(config.phase_agent_id("requirement-workflow-bootstrap"), Some("requirements-planner"));
    assert_eq!(config.phase_agent_id("po-review"), Some("po-reviewer"));
    assert_eq!(config.phase_mode("unit-test"), Some(PhaseExecutionMode::Command));
    assert_eq!(config.phase_mode("lint"), Some(PhaseExecutionMode::Command));
}

#[test]
fn structured_output_phase_rejects_empty_phase_even_with_structured_default() {
    let mut config = builtin_agent_runtime_config();
    let default_phase = config.phases.get_mut("default").expect("builtin config includes default phase");
    default_phase.output_contract = Some(PhaseOutputContract {
        kind: "phase_result".to_string(),
        required_fields: Vec::new(),
        fields: BTreeMap::new(),
    });

    assert!(config.is_structured_output_phase("custom-phase"));
    assert!(!config.is_structured_output_phase("   "));
}

fn make_minimal_config_with_phase(phase_id: &str, definition: PhaseExecutionDefinition) -> AgentRuntimeConfig {
    let mut config = builtin_agent_runtime_config();
    config.phases.insert(phase_id.to_string(), definition);
    config
}

#[test]
fn command_mode_phase_roundtrips_through_json() {
    let definition = PhaseExecutionDefinition {
        mode: PhaseExecutionMode::Command,
        agent_id: None,
        directive: Some("Run cargo test".to_string()),
        system_prompt: None,
        runtime: None,
        capabilities: None,
        output_contract: None,
        output_json_schema: None,
        decision_contract: None,
        retry: None,
        skills: Vec::new(),
        command: Some(PhaseCommandDefinition {
            program: "cargo".to_string(),
            args: vec!["test".to_string(), "--workspace".to_string()],
            env: BTreeMap::from([("RUST_LOG".to_string(), "info".to_string())]),
            cwd_mode: CommandCwdMode::ProjectRoot,
            cwd_path: None,
            timeout_secs: Some(300),
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
        default_tool: None,
    };

    let json = serde_json::to_string(&definition).expect("serialize");
    let restored: PhaseExecutionDefinition = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(restored.mode, PhaseExecutionMode::Command);
    assert!(restored.agent_id.is_none());
    let cmd = restored.command.expect("command block present");
    assert_eq!(cmd.program, "cargo");
    assert_eq!(cmd.args, vec!["test", "--workspace"]);
    assert_eq!(cmd.timeout_secs, Some(300));
    assert_eq!(cmd.success_exit_codes, vec![0]);
    assert!(!cmd.parse_json_output);
}

#[test]
fn command_mode_phase_validates_successfully() {
    let config = make_minimal_config_with_phase(
        "lint",
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
            agent_id: None,
            directive: Some("Run linter".to_string()),
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: Some(PhaseCommandDefinition {
                program: "cargo".to_string(),
                args: vec!["clippy".to_string()],
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
            default_tool: None,
        },
    );
    validate_agent_runtime_config(&config).expect("valid command-mode config");
}

#[test]
fn command_mode_rejects_missing_command_block() {
    let config = make_minimal_config_with_phase(
        "lint",
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
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
        },
    );
    let err = validate_agent_runtime_config(&config).unwrap_err();
    assert!(err.to_string().contains("requires command block"));
}

#[test]
fn command_mode_rejects_empty_program() {
    let config = make_minimal_config_with_phase(
        "lint",
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
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
            command: Some(PhaseCommandDefinition {
                program: "  ".to_string(),
                args: vec![],
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
            default_tool: None,
        },
    );
    let err = validate_agent_runtime_config(&config).unwrap_err();
    assert!(err.to_string().contains("program must not be empty"));
}

#[test]
fn command_mode_rejects_agent_id() {
    let config = make_minimal_config_with_phase(
        "lint",
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
            agent_id: Some("swe".to_string()),
            directive: None,
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: Some(PhaseCommandDefinition {
                program: "cargo".to_string(),
                args: vec![],
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
            default_tool: None,
        },
    );
    let err = validate_agent_runtime_config(&config).unwrap_err();
    assert!(err.to_string().contains("must not include agent_id"));
}

#[test]
fn command_mode_rejects_empty_success_exit_codes() {
    let config = make_minimal_config_with_phase(
        "lint",
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
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
            command: Some(PhaseCommandDefinition {
                program: "cargo".to_string(),
                args: vec![],
                env: BTreeMap::new(),
                cwd_mode: CommandCwdMode::ProjectRoot,
                cwd_path: None,
                timeout_secs: None,
                success_exit_codes: vec![],
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
            default_tool: None,
        },
    );
    let err = validate_agent_runtime_config(&config).unwrap_err();
    assert!(err.to_string().contains("success_exit_codes must include at least one code"));
}

#[test]
fn command_mode_cwd_path_required_for_path_mode() {
    let config = make_minimal_config_with_phase(
        "lint",
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
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
            command: Some(PhaseCommandDefinition {
                program: "cargo".to_string(),
                args: vec![],
                env: BTreeMap::new(),
                cwd_mode: CommandCwdMode::Path,
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
            default_tool: None,
        },
    );
    let err = validate_agent_runtime_config(&config).unwrap_err();
    assert!(err.to_string().contains("cwd_path must be set"));
}

#[test]
fn command_mode_rejects_manual_block() {
    let config = make_minimal_config_with_phase(
        "lint",
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
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
            command: Some(PhaseCommandDefinition {
                program: "cargo".to_string(),
                args: vec![],
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
            manual: Some(PhaseManualDefinition {
                instructions: "Wait for approval".to_string(),
                approval_note_required: false,
                timeout_secs: None,
            }),
            default_tool: None,
        },
    );
    let err = validate_agent_runtime_config(&config).unwrap_err();
    assert!(err.to_string().contains("must not include manual block"));
}

#[test]
fn phase_mode_returns_command_for_command_phase() {
    let config = make_minimal_config_with_phase(
        "lint",
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
            agent_id: None,
            directive: Some("Run linter".to_string()),
            system_prompt: None,
            runtime: None,
            capabilities: None,
            output_contract: None,
            output_json_schema: None,
            decision_contract: None,
            retry: None,
            skills: Vec::new(),
            command: Some(PhaseCommandDefinition {
                program: "cargo".to_string(),
                args: vec!["clippy".to_string()],
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
            default_tool: None,
        },
    );
    assert_eq!(config.phase_mode("lint"), Some(PhaseExecutionMode::Command));
    let cmd = config.phase_command("lint").expect("command block present");
    assert_eq!(cmd.program, "cargo");
    assert_eq!(cmd.args, vec!["clippy"]);
}

#[test]
fn command_mode_with_json_output_parsing_roundtrips() {
    let definition = PhaseExecutionDefinition {
        mode: PhaseExecutionMode::Command,
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
        command: Some(PhaseCommandDefinition {
            program: "bash".to_string(),
            args: vec!["-c".to_string(), "echo '{\"kind\":\"test_result\",\"passed\":true}'".to_string()],
            env: BTreeMap::new(),
            cwd_mode: CommandCwdMode::TaskRoot,
            cwd_path: None,
            timeout_secs: Some(60),
            success_exit_codes: vec![0, 1],
            parse_json_output: true,
            expected_result_kind: Some("test_result".to_string()),
            expected_schema: Some(serde_json::json!({
                "type": "object",
                "required": ["kind", "passed"],
                "properties": {
                    "kind": {"const": "test_result"},
                    "passed": {"type": "boolean"}
                }
            })),
            category: None,
            failure_pattern: None,
            excerpt_max_chars: None,
            on_success_verdict: None,
            on_failure_verdict: None,
            confidence: None,
            failure_risk: None,
        }),
        manual: None,
        default_tool: None,
    };

    let json = serde_json::to_string_pretty(&definition).expect("serialize");
    let restored: PhaseExecutionDefinition = serde_json::from_str(&json).expect("deserialize");

    let cmd = restored.command.expect("command present");
    assert!(cmd.parse_json_output);
    assert_eq!(cmd.expected_result_kind.as_deref(), Some("test_result"));
    assert!(cmd.expected_schema.is_some());
    assert_eq!(cmd.success_exit_codes, vec![0, 1]);
    assert_eq!(cmd.cwd_mode, CommandCwdMode::TaskRoot);
}

#[test]
fn command_mode_defaults_cwd_to_project_root_and_exit_code_zero() {
    let json = r#"{
            "mode": "command",
            "command": {
                "program": "make"
            }
        }"#;
    let definition: PhaseExecutionDefinition = serde_json::from_str(json).expect("deserialize minimal command phase");
    assert_eq!(definition.mode, PhaseExecutionMode::Command);
    let cmd = definition.command.expect("command present");
    assert_eq!(cmd.program, "make");
    assert_eq!(cmd.cwd_mode, CommandCwdMode::ProjectRoot);
    assert_eq!(cmd.success_exit_codes, vec![0]);
    assert!(cmd.args.is_empty());
    assert!(cmd.env.is_empty());
    assert!(cmd.timeout_secs.is_none());
    assert!(!cmd.parse_json_output);
}

#[test]
fn builtin_config_all_phases_are_agent_mode() {
    let config = builtin_agent_runtime_config();
    for (phase_id, definition) in &config.phases {
        if matches!(phase_id.as_str(), "lint" | "unit-test") {
            assert_eq!(
                definition.mode,
                PhaseExecutionMode::Command,
                "builtin phase '{}' should be command mode",
                phase_id
            );
            assert!(definition.command.is_some(), "builtin phase '{}' should have a command block", phase_id);
        } else {
            assert_eq!(definition.mode, PhaseExecutionMode::Agent, "builtin phase '{}' should be agent mode", phase_id);
            assert!(definition.command.is_none(), "builtin phase '{}' should have no command block", phase_id);
        }
    }
}

#[test]
fn command_mode_rejects_empty_args() {
    let config = make_minimal_config_with_phase(
        "lint",
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
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
            command: Some(PhaseCommandDefinition {
                program: "cargo".to_string(),
                args: vec!["test".to_string(), "  ".to_string()],
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
            default_tool: None,
        },
    );
    let err = validate_agent_runtime_config(&config).unwrap_err();
    assert!(err.to_string().contains("args must not contain empty values"));
}

#[test]
fn command_mode_rejects_empty_env_keys() {
    let config = make_minimal_config_with_phase(
        "lint",
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Command,
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
            command: Some(PhaseCommandDefinition {
                program: "cargo".to_string(),
                args: vec![],
                env: BTreeMap::from([("  ".to_string(), "value".to_string())]),
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
            default_tool: None,
        },
    );
    let err = validate_agent_runtime_config(&config).unwrap_err();
    assert!(err.to_string().contains("env must not contain empty keys"));
}

#[test]
fn legacy_config_without_new_fields_deserializes_with_none_defaults() {
    let json = r#"{
            "schema": "ao.agent-runtime-config.v2",
            "version": 2,
            "tools_allowlist": ["cargo"],
            "agents": {
                "default": {
                    "description": "Test agent",
                    "system_prompt": "You are a test agent.",
                    "tool": null,
                    "model": null,
                    "fallback_models": [],
                    "reasoning_effort": null,
                    "web_search": null,
                    "network_access": null,
                    "timeout_secs": null,
                    "max_attempts": null,
                    "extra_args": [],
                    "codex_config_overrides": []
                }
            },
            "phases": {
                "default": {
                    "mode": "agent",
                    "agent_id": "default",
                    "directive": "Do work."
                }
            }
        }"#;

    let config: AgentRuntimeConfig = serde_json::from_str(json).expect("deserialize");
    validate_agent_runtime_config(&config).expect("validate");
    let profile = config.agent_profile("default").expect("default profile");
    assert!(profile.role.is_none());
    assert!(profile.mcp_servers.is_empty());
    assert!(profile.skills.is_empty());
    assert!(profile.capabilities.is_empty());
    assert_eq!(profile.tool_policy, AgentToolPolicy::default());
    assert!(profile.mcp_server_configs.is_none());
    assert!(profile.structured_capabilities.is_none());
    assert!(profile.project_overrides.is_none());
}

#[test]
fn agent_tool_policy_roundtrips() {
    let policy = AgentToolPolicy {
        allow: vec!["task.*".to_string(), "workflow.*".to_string()],
        deny: vec!["project.remove".to_string()],
    };
    let json = serde_json::to_string(&policy).expect("serialize");
    let restored: AgentToolPolicy = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored, policy);
}

#[test]
fn agent_mcp_server_config_roundtrips() {
    let config = AgentMcpServerConfig {
        source: AgentMcpServerSource::Custom,
        tool_policy: AgentToolPolicy { allow: vec!["read.*".to_string()], deny: vec!["write.*".to_string()] },
        env: BTreeMap::from([("API_KEY".to_string(), "secret".to_string())]),
    };
    let json = serde_json::to_string(&config).expect("serialize");
    let restored: AgentMcpServerConfig = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored, config);
}

#[test]
fn agent_mcp_server_source_defaults_to_builtin() {
    let config: AgentMcpServerConfig = serde_json::from_str("{}").expect("deserialize empty");
    assert_eq!(config.source, AgentMcpServerSource::Builtin);
    assert!(config.tool_policy.allow.is_empty());
    assert!(config.tool_policy.deny.is_empty());
    assert!(config.env.is_empty());
}

#[test]
fn agent_capabilities_flattens_bool_map() {
    let caps = AgentCapabilities {
        flags: BTreeMap::from([("planning".to_string(), true), ("implementation".to_string(), false)]),
    };
    let json = serde_json::to_string(&caps).expect("serialize");
    let value: Value = serde_json::from_str(&json).expect("parse value");
    assert_eq!(value["planning"], json!(true));
    assert_eq!(value["implementation"], json!(false));

    let restored: AgentCapabilities = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored, caps);
}

#[test]
fn agent_project_overrides_roundtrips() {
    let overrides = AgentProjectOverrides {
        tool: Some("codex".to_string()),
        model: Some("gpt-4".to_string()),
        extra_args: vec!["--verbose".to_string()],
        env: BTreeMap::from([("DEBUG".to_string(), "1".to_string())]),
    };
    let json = serde_json::to_string(&overrides).expect("serialize");
    let restored: AgentProjectOverrides = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored.tool, overrides.tool);
    assert_eq!(restored.model, overrides.model);
    assert_eq!(restored.extra_args, overrides.extra_args);
    assert_eq!(restored.env, overrides.env);
}

#[test]
fn profile_with_new_fields_roundtrips_through_json() {
    let mut config = builtin_agent_runtime_config();
    let profile = config.agents.get_mut("default").expect("default profile");
    profile.mcp_server_configs = Some(BTreeMap::from([(
        "ao".to_string(),
        AgentMcpServerConfig {
            source: AgentMcpServerSource::Builtin,
            tool_policy: AgentToolPolicy { allow: vec!["task.*".to_string()], deny: vec![] },
            env: BTreeMap::new(),
        },
    )]));
    profile.structured_capabilities =
        Some(AgentCapabilities { flags: BTreeMap::from([("planning".to_string(), true)]) });
    profile.project_overrides = Some(BTreeMap::from([(
        "my-project".to_string(),
        AgentProjectOverrides {
            tool: Some("codex".to_string()),
            model: None,
            extra_args: vec![],
            env: BTreeMap::new(),
        },
    )]));

    let json = serde_json::to_string_pretty(&config).expect("serialize");
    let restored: AgentRuntimeConfig = serde_json::from_str(&json).expect("deserialize");
    validate_agent_runtime_config(&restored).expect("validate");

    let restored_profile = restored.agent_profile("default").expect("default profile");
    assert!(restored_profile.mcp_server_configs.is_some());
    let mcp_configs = restored_profile.mcp_server_configs.as_ref().unwrap();
    assert_eq!(mcp_configs.len(), 1);
    assert_eq!(mcp_configs["ao"].source, AgentMcpServerSource::Builtin);

    assert!(restored_profile.structured_capabilities.is_some());
    let caps = restored_profile.structured_capabilities.as_ref().unwrap();
    assert_eq!(caps.flags.get("planning"), Some(&true));

    assert!(restored_profile.project_overrides.is_some());
    let overrides = restored_profile.project_overrides.as_ref().unwrap();
    assert_eq!(overrides["my-project"].tool.as_deref(), Some("codex"));
}

#[test]
fn new_fields_skipped_in_serialization_when_none() {
    let config = builtin_agent_runtime_config();
    let json = serde_json::to_string_pretty(&config).expect("serialize");
    assert!(!json.contains("mcp_server_configs"));
    assert!(!json.contains("structured_capabilities"));
    assert!(!json.contains("project_overrides"));
}

#[test]
fn tool_policy_empty_permits_all() {
    let policy = AgentToolPolicy::default();
    assert!(policy.is_tool_permitted("task.list"));
    assert!(policy.is_tool_permitted("anything"));
    assert!(policy.is_tool_permitted(""));
}

#[test]
fn tool_policy_allowlist_only() {
    let policy = AgentToolPolicy { allow: vec!["task.*".to_string(), "workflow.run".to_string()], deny: vec![] };
    assert!(policy.is_tool_permitted("task.list"));
    assert!(policy.is_tool_permitted("task.create"));
    assert!(policy.is_tool_permitted("task.get"));
    assert!(policy.is_tool_permitted("workflow.run"));
    assert!(!policy.is_tool_permitted("workflow.cancel"));
    assert!(!policy.is_tool_permitted("daemon.stop"));
    assert!(!policy.is_tool_permitted(""));
}

#[test]
fn tool_policy_denylist_only() {
    let policy = AgentToolPolicy { allow: vec![], deny: vec!["daemon.*".to_string(), "project.remove".to_string()] };
    assert!(policy.is_tool_permitted("task.list"));
    assert!(policy.is_tool_permitted("workflow.run"));
    assert!(!policy.is_tool_permitted("daemon.stop"));
    assert!(!policy.is_tool_permitted("daemon.start"));
    assert!(!policy.is_tool_permitted("project.remove"));
    assert!(policy.is_tool_permitted("project.list"));
}

#[test]
fn tool_policy_combined_allow_and_deny() {
    let policy = AgentToolPolicy { allow: vec!["task.*".to_string()], deny: vec!["task.delete".to_string()] };
    assert!(policy.is_tool_permitted("task.list"));
    assert!(policy.is_tool_permitted("task.create"));
    assert!(!policy.is_tool_permitted("task.delete"));
    assert!(!policy.is_tool_permitted("workflow.run"));
}

#[test]
fn tool_policy_glob_wildcard_matches_across_dots() {
    let policy = AgentToolPolicy { allow: vec!["ao.*".to_string()], deny: vec![] };
    assert!(policy.is_tool_permitted("ao.task.list"));
    assert!(policy.is_tool_permitted("ao.workflow.run"));
    assert!(policy.is_tool_permitted("ao.x"));
    assert!(!policy.is_tool_permitted("other.thing"));
}

#[test]
fn tool_policy_exact_match() {
    let policy = AgentToolPolicy { allow: vec!["task.list".to_string()], deny: vec![] };
    assert!(policy.is_tool_permitted("task.list"));
    assert!(!policy.is_tool_permitted("task.create"));
    assert!(!policy.is_tool_permitted("task.list.extra"));
}

#[test]
fn tool_policy_wildcard_only_pattern() {
    let policy = AgentToolPolicy { allow: vec!["*".to_string()], deny: vec![] };
    assert!(policy.is_tool_permitted("anything"));
    assert!(policy.is_tool_permitted("a.b.c"));
    assert!(policy.is_tool_permitted(""));
}

#[test]
fn tool_policy_empty_tool_name() {
    let policy = AgentToolPolicy { allow: vec!["task.*".to_string()], deny: vec![] };
    assert!(!policy.is_tool_permitted(""));

    let deny_policy = AgentToolPolicy { allow: vec![], deny: vec!["*".to_string()] };
    assert!(!deny_policy.is_tool_permitted(""));
}

#[test]
fn tool_policy_multiple_wildcards() {
    let policy = AgentToolPolicy { allow: vec!["a.*.c".to_string()], deny: vec![] };
    assert!(policy.is_tool_permitted("a.b.c"));
    assert!(policy.is_tool_permitted("a.x.y.c"));
    assert!(!policy.is_tool_permitted("a.b.d"));
}

#[test]
fn tool_policy_prefix_wildcard() {
    let policy = AgentToolPolicy { allow: vec!["task.get*".to_string()], deny: vec![] };
    assert!(policy.is_tool_permitted("task.get"));
    assert!(policy.is_tool_permitted("task.get_by_id"));
    assert!(!policy.is_tool_permitted("task.list"));
}

#[test]
fn glob_match_basic() {
    assert!(glob_match("*", "anything"));
    assert!(glob_match("abc", "abc"));
    assert!(!glob_match("abc", "abcd"));
    assert!(!glob_match("abcd", "abc"));
    assert!(glob_match("a*c", "abc"));
    assert!(glob_match("a*c", "aXYZc"));
    assert!(!glob_match("a*c", "aXYZd"));
    assert!(glob_match("*.*", "a.b"));
    assert!(glob_match("task.*", "task.list"));
    assert!(glob_match("task.*", "task.list.nested"));
}

fn make_agent_profile_with_system_prompt(prompt: &str) -> AgentProfile {
    serde_json::from_value(serde_json::json!({
        "system_prompt": prompt
    }))
    .expect("deserialize agent profile")
}

#[test]
fn phase_system_prompt_override_takes_precedence_over_agent_profile() {
    let mut config = builtin_agent_runtime_config();
    config.agents.insert("test-agent".to_string(), make_agent_profile_with_system_prompt("Agent profile prompt"));
    config.phases.insert(
        "custom-phase".to_string(),
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("test-agent".to_string()),
            directive: Some("Do the thing".to_string()),
            system_prompt: Some("Phase-level prompt override".to_string()),
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
        },
    );
    assert_eq!(config.phase_system_prompt("custom-phase"), Some("Phase-level prompt override"));
}

#[test]
fn phase_system_prompt_falls_back_to_agent_profile() {
    let mut config = builtin_agent_runtime_config();
    config.agents.insert("test-agent".to_string(), make_agent_profile_with_system_prompt("Agent profile prompt"));
    config.phases.insert(
        "custom-phase".to_string(),
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("test-agent".to_string()),
            directive: Some("Do the thing".to_string()),
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
        },
    );
    assert_eq!(config.phase_system_prompt("custom-phase"), Some("Agent profile prompt"));
}

#[test]
fn phase_system_prompt_ignores_empty_override() {
    let mut config = builtin_agent_runtime_config();
    config.agents.insert("test-agent".to_string(), make_agent_profile_with_system_prompt("Agent profile prompt"));
    config.phases.insert(
        "custom-phase".to_string(),
        PhaseExecutionDefinition {
            mode: PhaseExecutionMode::Agent,
            agent_id: Some("test-agent".to_string()),
            directive: None,
            system_prompt: Some("   ".to_string()),
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
        },
    );
    assert_eq!(config.phase_system_prompt("custom-phase"), Some("Agent profile prompt"));
}

#[test]
fn phase_system_prompt_deserializes_with_and_without_field() {
    let with_prompt: PhaseExecutionDefinition = serde_json::from_str(
        r#"{
            "mode": "agent",
            "agent_id": "default",
            "system_prompt": "Custom prompt from JSON"
        }"#,
    )
    .expect("deserialize with system_prompt");
    assert_eq!(with_prompt.system_prompt.as_deref(), Some("Custom prompt from JSON"));

    let without_prompt: PhaseExecutionDefinition = serde_json::from_str(
        r#"{
            "mode": "agent",
            "agent_id": "default"
        }"#,
    )
    .expect("deserialize without system_prompt");
    assert!(without_prompt.system_prompt.is_none());
}

#[test]
fn phase_system_prompt_skips_serialization_when_none() {
    let definition = PhaseExecutionDefinition {
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
    };
    let json = serde_json::to_string(&definition).expect("serialize");
    assert!(!json.contains("system_prompt"));

    let with_prompt = PhaseExecutionDefinition { system_prompt: Some("My custom prompt".to_string()), ..definition };
    let json = serde_json::to_string(&with_prompt).expect("serialize");
    assert!(json.contains("system_prompt"));
    assert!(json.contains("My custom prompt"));
}
