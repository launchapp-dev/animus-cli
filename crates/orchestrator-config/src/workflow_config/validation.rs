use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::agent_runtime_config::{
    default_eval_expected_exit as default_eval_expected_exit_value, AgentProfileOverlay, AgentRuntimeConfig, EvalKind,
    EvalsConfig, PhaseExecutionMode,
};

use super::types::*;

fn validate_evals_block(phase_id: &str, evals: &EvalsConfig, config: &WorkflowConfig, errors: &mut Vec<String>) {
    if !(0.0..=1.0).contains(&evals.pass_threshold) || !evals.pass_threshold.is_finite() {
        errors.push(format!(
            "phase_definitions['{}'].evals.pass_threshold must be between 0.0 and 1.0 (got {})",
            phase_id, evals.pass_threshold
        ));
    }
    if evals.checks.is_empty() {
        errors.push(format!("phase_definitions['{}'].evals must declare at least one check", phase_id));
    }
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    for (idx, check) in evals.checks.iter().enumerate() {
        let trimmed_id = check.id.trim();
        if trimmed_id.is_empty() {
            errors.push(format!("phase_definitions['{}'].evals.checks[{}].id must not be empty", phase_id, idx));
        } else if !seen_ids.insert(trimmed_id.to_ascii_lowercase()) {
            errors
                .push(format!("phase_definitions['{}'].evals.checks contains duplicate id '{}'", phase_id, trimmed_id));
        }
        match check.kind {
            EvalKind::Command => {
                match check.command.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
                    None => errors.push(format!(
                        "phase_definitions['{}'].evals.checks['{}'] kind='command' requires a non-empty command field",
                        phase_id, check.id
                    )),
                    Some(program) => {
                        if !config.tools_allowlist.is_empty()
                            && !config.tools_allowlist.iter().any(|t| t.eq_ignore_ascii_case(program))
                        {
                            errors.push(format!(
                                "phase_definitions['{}'].evals.checks['{}'].command '{}' is not in tools_allowlist",
                                phase_id, check.id, program
                            ));
                        }
                    }
                }
                if check.agent.is_some() || check.prompt.is_some() {
                    errors.push(format!(
                        "phase_definitions['{}'].evals.checks['{}'] kind='command' must not declare agent/prompt",
                        phase_id, check.id
                    ));
                }
                if let Some(timeout) = check.timeout_secs {
                    if timeout == 0 {
                        errors.push(format!(
                            "phase_definitions['{}'].evals.checks['{}'].timeout_secs must be greater than 0",
                            phase_id, check.id
                        ));
                    }
                }
            }
            EvalKind::LlmJudge => {
                let agent_ok = check.agent.as_deref().map(str::trim).filter(|s| !s.is_empty());
                let prompt_ok = check.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty());
                if agent_ok.is_none() {
                    errors.push(format!(
                        "phase_definitions['{}'].evals.checks['{}'] kind='llm_judge' requires a non-empty agent field",
                        phase_id, check.id
                    ));
                } else if let Some(agent_id) = agent_ok {
                    if !config.agent_profiles.contains_key(agent_id) {
                        errors.push(format!(
                            "phase_definitions['{}'].evals.checks['{}'] references agent '{}' not found in agent_profiles",
                            phase_id, check.id, agent_id
                        ));
                    }
                }
                if prompt_ok.is_none() {
                    errors.push(format!(
                        "phase_definitions['{}'].evals.checks['{}'] kind='llm_judge' requires a non-empty prompt field",
                        phase_id, check.id
                    ));
                }
                if check.command.is_some() || !check.args.is_empty() {
                    errors.push(format!(
                        "phase_definitions['{}'].evals.checks['{}'] kind='llm_judge' must not declare command/args",
                        phase_id, check.id
                    ));
                }
                // Codex round-9 P3: working_dir and expected_exit are
                // command-only knobs; `run_llm_judge_check` does not consume
                // them. Reject so misleading YAML doesn't slip through.
                if check.working_dir.is_some() {
                    errors.push(format!(
                        "phase_definitions['{}'].evals.checks['{}'] kind='llm_judge' must not declare working_dir",
                        phase_id, check.id
                    ));
                }
                if check.expected_exit != default_eval_expected_exit_value() {
                    errors.push(format!(
                        "phase_definitions['{}'].evals.checks['{}'] kind='llm_judge' must not override expected_exit",
                        phase_id, check.id
                    ));
                }
                // Codex round-7 P2: llm_judge dispatch is one-shot through
                // the session backend; `timeout_secs` is silently ignored
                // by `run_llm_judge_check`. Reject it so operators get a
                // clear error instead of a misleading config.
                if check.timeout_secs.is_some() {
                    errors.push(format!(
                        "phase_definitions['{}'].evals.checks['{}'] kind='llm_judge' does not support timeout_secs (judge timeouts inherit from the agent profile)",
                        phase_id, check.id
                    ));
                }
            }
        }
    }
    if evals.on_fail == crate::agent_runtime_config::EvalOnFail::Rework && evals.max_reworks == 0 {
        errors.push(format!("phase_definitions['{}'].evals.on_fail='rework' requires max_reworks > 0", phase_id));
    }
}

fn validate_cron_expression(expression: &str) -> Result<()> {
    let expression = expression.trim();
    if expression.is_empty() {
        anyhow::bail!("cron expression must not be empty");
    }

    let parser = croner::parser::CronParser::builder()
        .seconds(croner::parser::Seconds::Disallowed)
        .year(croner::parser::Year::Disallowed)
        .build();
    parser.parse(expression).map_err(|error| anyhow::anyhow!("invalid cron expression '{}': {}", expression, error))?;
    Ok(())
}

fn is_supported_shortcut_cron(expression: &str) -> bool {
    matches!(expression, "@hourly" | "@daily" | "@weekly" | "@monthly")
}

fn validate_budget_config(budget: &BudgetConfig, scope_label: &str, errors: &mut Vec<String>) {
    if budget.is_empty() {
        errors.push(format!("{scope_label} must declare at least one of max_tokens or max_cost_usd"));
        return;
    }
    if let Some(max_tokens) = budget.max_tokens {
        if max_tokens == 0 {
            errors.push(format!("{scope_label}.max_tokens must be greater than 0"));
        }
    }
    if let Some(max_cost_usd) = budget.max_cost_usd {
        if !max_cost_usd.is_finite() {
            errors.push(format!("{scope_label}.max_cost_usd must be a finite number"));
        } else if max_cost_usd <= 0.0 {
            errors.push(format!("{scope_label}.max_cost_usd must be greater than 0"));
        }
    }
}

pub fn validate_workflow_and_runtime_configs(workflow: &WorkflowConfig, runtime: &AgentRuntimeConfig) -> Result<()> {
    validate_workflow_and_runtime_configs_with_project_root(workflow, runtime, None)
}

pub fn validate_workflow_and_runtime_configs_with_project_root(
    workflow: &WorkflowConfig,
    runtime: &AgentRuntimeConfig,
    project_root: Option<&Path>,
) -> Result<()> {
    validate_workflow_config(workflow)?;

    let mut errors = Vec::new();
    let mut known_claude_profiles: Option<BTreeSet<String>> = None;
    if project_root.is_some() {
        match protocol::Config::load_global() {
            Ok(config) => {
                known_claude_profiles = Some(config.claude_profiles.keys().cloned().collect());
            }
            Err(error) => {
                errors.push(format!("failed to load global Animus config for claude profile validation: {error}"));
            }
        }
    }

    for workflow_def in &workflow.workflows {
        let expanded = match expand_workflow_phases(&workflow.workflows, &workflow_def.id) {
            Ok(phases) => phases,
            Err(_) => continue,
        };

        for entry in &expanded {
            let phase_id = entry.phase_id().trim();
            if phase_id.is_empty() {
                continue;
            }

            if workflow.phase_catalog.keys().all(|candidate| !candidate.eq_ignore_ascii_case(phase_id)) {
                errors
                    .push(format!("workflow '{}' phase '{}' is missing from phase_catalog", workflow_def.id, phase_id));
            }

            let in_workflow = workflow.phase_definitions.keys().any(|k| k.eq_ignore_ascii_case(phase_id));
            if !in_workflow && !runtime.has_phase_definition(phase_id) {
                errors.push(format!(
                    "workflow '{}' phase '{}' is missing from agent-runtime phases and workflow phase_definitions",
                    workflow_def.id, phase_id
                ));
            }
        }
    }

    for (agent_id, profile) in &workflow.agent_profiles {
        if let Some(hooks) = profile.hooks.as_ref() {
            if let Err(error) = crate::agent_runtime_config::validate_agent_hooks_block(agent_id, hooks) {
                errors.push(format!("agent_profiles['{agent_id}'].hooks invalid: {error}"));
            }
        }
        if let Some(profile_name) = trim_nonempty(profile.tool_profile.as_deref()) {
            let resolved_tool = resolve_tool_id(profile.tool.as_deref(), profile.model.as_deref()).or_else(|| {
                runtime
                    .agent_profile(agent_id)
                    .and_then(|profile| resolve_tool_id(profile.tool.as_deref(), profile.model.as_deref()))
            });
            validate_claude_profile_selection(
                &format!("agent_profiles['{}'].tool_profile", agent_id),
                profile_name,
                resolved_tool.as_deref(),
                known_claude_profiles.as_ref(),
                &mut errors,
            );
        }
    }

    for (phase_id, definition) in &workflow.phase_definitions {
        let Some(runtime_overrides) = definition.runtime.as_ref() else {
            continue;
        };
        let Some(profile_name) = trim_nonempty(runtime_overrides.tool_profile.as_deref()) else {
            continue;
        };
        if definition.mode != PhaseExecutionMode::Agent {
            errors.push(format!(
                "phase_definitions['{}'].runtime.tool_profile is only supported for agent phases",
                phase_id
            ));
            continue;
        }

        let resolved_tool = resolve_tool_id(runtime_overrides.tool.as_deref(), runtime_overrides.model.as_deref())
            .or_else(|| {
                definition.agent_id.as_deref().and_then(|agent_id| {
                    lookup_workflow_agent_profile(workflow, agent_id)
                        .and_then(|profile| resolve_tool_id(profile.tool.as_deref(), profile.model.as_deref()))
                        .or_else(|| {
                            runtime
                                .agent_profile(agent_id)
                                .and_then(|profile| resolve_tool_id(profile.tool.as_deref(), profile.model.as_deref()))
                        })
                })
            });
        validate_claude_profile_selection(
            &format!("phase_definitions['{}'].runtime.tool_profile", phase_id),
            profile_name,
            resolved_tool.as_deref(),
            known_claude_profiles.as_ref(),
            &mut errors,
        );
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

fn trim_nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn resolve_tool_id(tool: Option<&str>, model: Option<&str>) -> Option<String> {
    trim_nonempty(tool)
        .map(|value| value.to_ascii_lowercase())
        .or_else(|| trim_nonempty(model).map(|value| protocol::tool_for_model_id(value).to_string()))
}

fn lookup_workflow_agent_profile<'a>(workflow: &'a WorkflowConfig, agent_id: &str) -> Option<&'a AgentProfileOverlay> {
    workflow
        .agent_profiles
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(agent_id))
        .map(|(_, profile)| profile)
}

fn has_workflow_agent_profile(workflow: &WorkflowConfig, agent_id: &str) -> bool {
    lookup_workflow_agent_profile(workflow, agent_id).is_some()
}

fn validate_claude_profile_selection(
    field_path: &str,
    profile_name: &str,
    resolved_tool: Option<&str>,
    known_claude_profiles: Option<&BTreeSet<String>>,
    errors: &mut Vec<String>,
) {
    match resolved_tool {
        Some(tool_id) if tool_id.eq_ignore_ascii_case("claude") => {}
        Some(tool_id) => {
            errors.push(format!(
                "{field_path} is only supported when the effective tool is claude (resolved '{}')",
                tool_id
            ));
            return;
        }
        None => {
            errors.push(format!(
                "{field_path} requires an effective Claude tool to be resolvable from the phase or agent config",
            ));
            return;
        }
    }

    if let Some(known_profiles) = known_claude_profiles {
        if !known_profiles.contains(profile_name) {
            errors.push(format!("{field_path} references unknown global claude profile '{}'", profile_name));
        }
    }
}

fn validate_skill_references(
    field_path: &str,
    skills: &[String],
    _project_root: Option<&Path>,
    errors: &mut Vec<String>,
) {
    for skill_name in skills {
        if skill_name.trim().is_empty() {
            errors.push(format!("{field_path} must not contain empty values"));
            return;
        }
    }
}

fn validate_oauth_config(server_name: &str, transport: Option<&str>, oauth: &OauthConfig, errors: &mut Vec<String>) {
    if transport != Some("http") {
        errors.push(format!("mcp_servers['{}'].oauth is only valid when transport is \"http\"", server_name));
        return;
    }
    let field = |name: &str| format!("mcp_servers['{}'].oauth.{}", server_name, name);
    let require_nonempty = |value: &Option<String>, name: &str, errors: &mut Vec<String>| match value {
        None => {
            errors.push(format!("{} is required for flow=\"{}\"", field(name), oauth.flow.as_str()));
            false
        }
        Some(v) if v.trim().is_empty() => {
            errors.push(format!("{} must not be empty", field(name)));
            false
        }
        _ => true,
    };
    if let Some(token_url) = &oauth.token_url {
        // Same shape check as the HTTP MCP server `url` field: untrimmed
        // surrounding whitespace fails (it would be sent to reqwest
        // verbatim), the scheme must be http/https, and the host
        // segment must be present and contain no whitespace.
        let raw = token_url.as_str();
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let untrimmed = raw.len() != trimmed.len();
            let after_scheme = trimmed.strip_prefix("http://").or_else(|| trimmed.strip_prefix("https://"));
            let host_ok = after_scheme
                .map(|rest| {
                    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
                    !host.trim().is_empty() && !host.contains(char::is_whitespace)
                })
                .unwrap_or(false);
            if untrimmed || !host_ok {
                errors.push(format!(
                    "{} must be a valid http:// or https:// URL with a non-empty host, got \"{}\"",
                    field("token_url"),
                    raw
                ));
            }
        }
    }
    if oauth.scopes.iter().any(|s| s.trim().is_empty()) {
        errors.push(format!("{} must not contain empty values", field("scopes")));
    }
    match oauth.flow {
        OauthFlow::ClientCredentials => {
            require_nonempty(&oauth.token_url, "token_url", errors);
            require_nonempty(&oauth.client_id_env, "client_id_env", errors);
            require_nonempty(&oauth.client_secret_env, "client_secret_env", errors);
            if oauth.refresh_token_env.is_some() {
                errors.push(format!("{} must not be set for flow=\"client_credentials\"", field("refresh_token_env")));
            }
            if oauth.bearer_env.is_some() {
                errors.push(format!("{} must not be set for flow=\"client_credentials\"", field("bearer_env")));
            }
        }
        OauthFlow::RefreshToken => {
            require_nonempty(&oauth.token_url, "token_url", errors);
            require_nonempty(&oauth.refresh_token_env, "refresh_token_env", errors);
            if oauth.bearer_env.is_some() {
                errors.push(format!("{} must not be set for flow=\"refresh_token\"", field("bearer_env")));
            }
        }
        OauthFlow::ManualBearer => {
            require_nonempty(&oauth.bearer_env, "bearer_env", errors);
            if oauth.token_url.is_some() {
                errors.push(format!("{} must not be set for flow=\"manual_bearer\"", field("token_url")));
            }
            if oauth.client_id_env.is_some() {
                errors.push(format!("{} must not be set for flow=\"manual_bearer\"", field("client_id_env")));
            }
            if oauth.client_secret_env.is_some() {
                errors.push(format!("{} must not be set for flow=\"manual_bearer\"", field("client_secret_env")));
            }
            if oauth.refresh_token_env.is_some() {
                errors.push(format!("{} must not be set for flow=\"manual_bearer\"", field("refresh_token_env")));
            }
            if !oauth.scopes.is_empty() {
                errors.push(format!("{} must not be set for flow=\"manual_bearer\"", field("scopes")));
            }
        }
        OauthFlow::AuthorizationCode => {
            // Discovery + DCR fill in the token/authorization endpoints, so
            // none of the machine-to-machine `*_env` fields apply. Reject
            // them so a misplaced credential pointer surfaces at validation
            // time rather than being silently ignored.
            for (value, name) in [
                (&oauth.client_id_env, "client_id_env"),
                (&oauth.client_secret_env, "client_secret_env"),
                (&oauth.refresh_token_env, "refresh_token_env"),
                (&oauth.bearer_env, "bearer_env"),
                (&oauth.token_url, "token_url"),
            ] {
                if value.is_some() {
                    errors.push(format!("{} must not be set for flow=\"authorization_code\"", field(name)));
                }
            }
            // A blank/whitespace-only pinned `client_id` is a config typo: the
            // auth flow would treat `Some("")` as a pinned-client flow and
            // skip Dynamic Client Registration with an empty id, failing later
            // at the browser/token-exchange step. Reject it here. Omit the key
            // entirely to use DCR.
            if oauth.client_id.as_ref().is_some_and(|id| id.trim().is_empty()) {
                errors.push(format!("{} must not be blank for flow=\"authorization_code\"", field("client_id")));
            }
        }
    }
}

pub fn validate_workflow_config(config: &WorkflowConfig) -> Result<()> {
    validate_workflow_config_with_project_root(config, None)
}

pub fn validate_workflow_config_with_project_root(config: &WorkflowConfig, project_root: Option<&Path>) -> Result<()> {
    let mut errors = Vec::new();

    if config.schema.trim() != WORKFLOW_CONFIG_SCHEMA_ID {
        errors.push(format!("schema must be '{}' (got '{}')", WORKFLOW_CONFIG_SCHEMA_ID, config.schema));
    }

    if config.version != WORKFLOW_CONFIG_VERSION {
        errors.push(format!("version must be {} (got {})", WORKFLOW_CONFIG_VERSION, config.version));
    }

    if config.checkpoint_retention.keep_last_per_phase == 0 {
        errors.push("checkpoint_retention.keep_last_per_phase must be greater than zero".to_string());
    }

    if config.phase_catalog.is_empty() {
        errors.push("phase_catalog must include at least one phase".to_string());
    }

    for (phase_id, definition) in &config.phase_catalog {
        if phase_id.trim().is_empty() {
            errors.push("phase_catalog contains an empty phase id".to_string());
            continue;
        }

        if definition.label.trim().is_empty() {
            errors.push(format!("phase_catalog['{}'].label must not be empty", phase_id));
        }

        if definition.tags.iter().any(|tag| tag.trim().is_empty()) {
            errors.push(format!("phase_catalog['{}'].tags must not contain empty values", phase_id));
        }
    }

    let mut workflow_refs = BTreeMap::<String, usize>::new();
    for workflow in &config.workflows {
        let workflow_ref = workflow.id.trim();
        if workflow_ref.is_empty() {
            errors.push("workflows contains a workflow with an empty id".to_string());
            continue;
        }

        let normalized = workflow_ref.to_ascii_lowercase();
        if let Some(existing) = workflow_refs.insert(normalized.clone(), 1) {
            let _ = existing;
            errors.push(format!("duplicate workflow id '{}'", workflow_ref));
        }

        if workflow.name.trim().is_empty() {
            errors.push(format!("workflow '{}' name must not be empty", workflow_ref));
        }

        if workflow.phases.is_empty() {
            errors.push(format!("workflow '{}' must include at least one phase", workflow_ref));
            continue;
        }

        for entry in &workflow.phases {
            if let WorkflowPhaseEntry::SubWorkflow(sub) = entry {
                let ref_id = sub.workflow_ref.trim();
                if ref_id.is_empty() {
                    errors.push(format!(
                        "workflow '{}' contains a sub-workflow reference with an empty workflow_ref",
                        workflow_ref
                    ));
                    continue;
                }
                if !config.workflows.iter().any(|p| p.id.eq_ignore_ascii_case(ref_id)) {
                    errors.push(format!("workflow '{}' references unknown sub-workflow '{}'", workflow_ref, ref_id));
                }
                continue;
            }

            let phase_id = entry.phase_id().trim();
            if phase_id.is_empty() {
                errors.push(format!("workflow '{}' contains an empty phase id", workflow_ref));
                continue;
            }

            if config.phase_catalog.keys().all(|candidate| !candidate.eq_ignore_ascii_case(phase_id)) {
                errors.push(format!(
                    "workflow '{}' references unknown phase '{}'; add it to phase_catalog",
                    workflow_ref, phase_id
                ));
            }
        }

        if let Some(budget) = workflow.budget.as_ref() {
            validate_budget_config(budget, &format!("workflow '{workflow_ref}' budget"), &mut errors);
        }

        for entry in &workflow.phases {
            if let Some(budget) = entry.budget() {
                let phase_id = entry.phase_id().trim();
                validate_budget_config(
                    budget,
                    &format!("workflow '{workflow_ref}' phase '{phase_id}' budget"),
                    &mut errors,
                );
            }
        }

        match expand_workflow_phases(&config.workflows, workflow_ref) {
            Ok(expanded) => {
                if expanded.is_empty() {
                    errors.push(format!("workflow '{}' expands to zero phases", workflow_ref));
                }

                let expanded_phase_ids: Vec<String> =
                    expanded.iter().map(|e| e.phase_id().trim().to_owned()).filter(|id| !id.is_empty()).collect();

                for entry in &expanded {
                    let phase_id = entry.phase_id().trim();
                    if let Some(max_rework_attempts) = entry.max_rework_attempts() {
                        if max_rework_attempts == 0 {
                            errors.push(format!(
                                "workflow '{}' phase '{}' max_rework_attempts must be greater than 0",
                                workflow_ref, phase_id
                            ));
                        }
                    }

                    if let Some(verdicts) = entry.on_verdict() {
                        for (verdict_key, transition) in verdicts {
                            let target = transition.target.trim();
                            if target.is_empty() {
                                errors.push(format!(
                                    "workflow '{}' phase '{}' on_verdict '{}' has an empty target",
                                    workflow_ref, phase_id, verdict_key
                                ));
                                continue;
                            }
                            if !expanded_phase_ids.iter().any(|id| id.eq_ignore_ascii_case(target)) {
                                errors.push(format!(
                                    "workflow '{}' phase '{}' on_verdict '{}' targets unknown phase '{}'",
                                    workflow_ref, phase_id, verdict_key, target
                                ));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                errors.push(format!("workflow '{}' sub-workflow expansion failed: {}", workflow_ref, e));
            }
        }
    }

    if !config.default_workflow_ref.trim().is_empty()
        && config
            .workflows
            .iter()
            .all(|workflow| !workflow.id.eq_ignore_ascii_case(config.default_workflow_ref.as_str()))
    {
        errors.push(format!(
            "default_workflow_ref '{}' must reference an existing workflow",
            config.default_workflow_ref
        ));
    }

    for (phase_id, definition) in &config.phase_definitions {
        if phase_id.trim().is_empty() {
            errors.push("phase_definitions contains an empty phase id".to_string());
            continue;
        }
        validate_skill_references(
            format!("phase_definitions['{}'].skills", phase_id).as_str(),
            &definition.skills,
            project_root,
            &mut errors,
        );
        match definition.mode {
            PhaseExecutionMode::Command => {
                let Some(command) = definition.command.as_ref() else {
                    errors.push(format!("phase_definitions['{}'] mode 'command' requires command block", phase_id));
                    continue;
                };
                if command.program.trim().is_empty() {
                    errors.push(format!("phase_definitions['{}'].command.program must not be empty", phase_id));
                }
                if command.success_exit_codes.is_empty() {
                    errors.push(format!(
                        "phase_definitions['{}'].command.success_exit_codes must include at least one code",
                        phase_id
                    ));
                }
                if !config.tools_allowlist.is_empty()
                    && !config.tools_allowlist.iter().any(|t| t.eq_ignore_ascii_case(&command.program))
                {
                    errors.push(format!(
                        "phase_definitions['{}'].command.program '{}' is not in tools_allowlist",
                        phase_id, command.program
                    ));
                }
                if definition.manual.is_some() {
                    errors.push(format!(
                        "phase_definitions['{}'] mode 'command' must not include manual block",
                        phase_id
                    ));
                }
            }
            PhaseExecutionMode::Manual => {
                let Some(manual) = definition.manual.as_ref() else {
                    errors.push(format!("phase_definitions['{}'] mode 'manual' requires manual block", phase_id));
                    continue;
                };
                if manual.instructions.trim().is_empty() {
                    errors.push(format!("phase_definitions['{}'].manual.instructions must not be empty", phase_id));
                }
                if let Some(timeout_secs) = manual.timeout_secs {
                    if timeout_secs == 0 {
                        errors.push(format!(
                            "phase_definitions['{}'].manual.timeout_secs must be greater than 0",
                            phase_id
                        ));
                    }
                }
                if definition.command.is_some() {
                    errors.push(format!(
                        "phase_definitions['{}'] mode 'manual' must not include command block",
                        phase_id
                    ));
                }
            }
            PhaseExecutionMode::Agent => {
                if definition.agent_id.is_some() {
                    if let Some(agent_id) = definition.agent_id.as_deref() {
                        if !agent_id.trim().is_empty() && !config.agent_profiles.contains_key(agent_id) {
                            errors.push(format!(
                                "phase_definitions['{}'] references agent '{}' not found in agent_profiles (will check runtime config at execution time)",
                                phase_id, agent_id
                            ));
                        }
                    }
                }
            }
        }
        if let Some(evals) = definition.evals.as_ref() {
            validate_evals_block(phase_id, evals, config, &mut errors);
        }
    }

    for (name, definition) in &config.mcp_servers {
        if name.trim().is_empty() {
            errors.push("mcp_servers contains an empty server name".to_string());
            continue;
        }
        let transport = definition.transport.as_deref().map(str::trim).filter(|t| !t.is_empty());
        match transport {
            Some("http") => {
                match definition.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
                    None => {
                        errors.push(format!("mcp_servers['{}'].url is required when transport is \"http\"", name));
                    }
                    Some(url) => {
                        let after_scheme = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"));
                        let host_ok = after_scheme
                            .map(|rest| {
                                let host = rest.split(['/', '?', '#']).next().unwrap_or("");
                                !host.trim().is_empty() && !host.contains(char::is_whitespace)
                            })
                            .unwrap_or(false);
                        if !host_ok {
                            errors.push(format!(
                                "mcp_servers['{}'].url must be a valid http:// or https:// URL, got \"{}\"",
                                name, url
                            ));
                        }
                    }
                }
                if !definition.command.trim().is_empty() {
                    errors.push(format!("mcp_servers['{}'].command must not be set when transport is \"http\"", name));
                }
                if !definition.args.is_empty() {
                    errors.push(format!("mcp_servers['{}'].args must not be set when transport is \"http\"", name));
                }
                if !definition.env.is_empty() {
                    errors.push(format!("mcp_servers['{}'].env must not be set when transport is \"http\"", name));
                }
            }
            Some(other) if other != "stdio" => {
                errors.push(format!(
                    "mcp_servers['{}'].transport must be \"stdio\" or \"http\", got \"{}\"",
                    name, other
                ));
            }
            _ => {
                if definition.command.trim().is_empty() {
                    errors.push(format!("mcp_servers['{}'].command must not be empty", name));
                }
                if definition.url.as_deref().is_some_and(|u| !u.trim().is_empty()) {
                    errors.push(format!("mcp_servers['{}'].url must not be set when transport is \"stdio\"", name));
                }
            }
        }
        if definition.transport.as_deref().is_some_and(|transport| transport.trim().is_empty()) {
            errors.push(format!("mcp_servers['{}'].transport must not be empty when set", name));
        }
        if definition.args.iter().any(|arg| arg.trim().is_empty()) {
            errors.push(format!("mcp_servers['{}'].args must not contain empty values", name));
        }
        if definition.tools.iter().any(|tool| tool.trim().is_empty()) {
            errors.push(format!("mcp_servers['{}'].tools must not contain empty values", name));
        }
        if definition.env.iter().any(|(key, value)| key.trim().is_empty() || value.trim().is_empty()) {
            errors.push(format!("mcp_servers['{}'].env must not contain empty keys or values", name));
        }
        if let Some(oauth) = &definition.oauth {
            validate_oauth_config(name, transport, oauth, &mut errors);
        }
    }

    for (agent_id, profile) in &config.agent_profiles {
        if agent_id.trim().is_empty() {
            errors.push("agent_profiles contains an empty agent id".to_string());
            continue;
        }
        if profile.name.as_deref().is_some_and(|value| value.trim().is_empty()) {
            errors.push(format!("agent_profiles['{}'].name must not be empty", agent_id));
        }
        if let Some(persona) = profile.persona.as_ref() {
            if persona.style.as_deref().is_some_and(|value| value.trim().is_empty()) {
                errors.push(format!("agent_profiles['{}'].persona.style must not be empty", agent_id));
            }
            if persona.instructions.as_deref().is_some_and(|value| value.trim().is_empty()) {
                errors.push(format!("agent_profiles['{}'].persona.instructions must not be empty", agent_id));
            }
            if persona.traits.iter().any(|value| value.trim().is_empty()) {
                errors.push(format!("agent_profiles['{}'].persona.traits must not contain empty values", agent_id));
            }
            if persona.customizations.keys().any(|value| value.trim().is_empty()) {
                errors
                    .push(format!("agent_profiles['{}'].persona.customizations must not contain empty keys", agent_id));
            }
        }
        let memory = profile.memory.clone().unwrap_or_default();
        let communication = profile.communication.clone().unwrap_or_default();
        if memory.scope.as_deref().is_some_and(|value| value.trim().is_empty()) {
            errors.push(format!("agent_profiles['{}'].memory.scope must not be empty", agent_id));
        }
        if memory.max_context_chars == Some(0) {
            errors.push(format!("agent_profiles['{}'].memory.max_context_chars must be greater than 0", agent_id));
        }
        if communication.max_context_chars == Some(0) {
            errors
                .push(format!("agent_profiles['{}'].communication.max_context_chars must be greater than 0", agent_id));
        }
        validate_skill_references(
            format!("agent_profiles['{}'].skills", agent_id).as_str(),
            profile.skills.as_deref().unwrap_or_default(),
            project_root,
            &mut errors,
        );
        for server in profile.mcp_servers.as_deref().unwrap_or_default() {
            if server.trim().is_empty() {
                errors.push(format!("agent_profiles['{}'].mcp_servers must not contain empty values", agent_id));
                continue;
            }
            if !config.mcp_servers.contains_key(server) {
                errors.push(format!(
                    "agent_profiles['{}'].mcp_servers references unknown MCP server '{}'",
                    agent_id, server
                ));
            }
        }
        if communication.enabled && communication.channels.is_empty() && communication.can_message.is_empty() {
            errors.push(format!(
                "agent_profiles['{}'].communication requires at least one channel or can_message target when enabled",
                agent_id
            ));
        }
        for channel in &communication.channels {
            if channel.trim().is_empty() {
                errors.push(format!(
                    "agent_profiles['{}'].communication.channels must not contain empty values",
                    agent_id
                ));
                continue;
            }
            match config.agent_channels.get(channel) {
                Some(channel_config)
                    if !channel_config.participants.is_empty()
                        && !channel_config
                            .participants
                            .iter()
                            .any(|participant| participant.eq_ignore_ascii_case(agent_id)) =>
                {
                    errors.push(format!(
                        "agent_profiles['{}'].communication.channels references channel '{}' where the agent is not a participant",
                        agent_id, channel
                    ));
                }
                Some(_) => {}
                None => errors.push(format!(
                    "agent_profiles['{}'].communication.channels references unknown channel '{}'",
                    agent_id, channel
                )),
            }
        }
        for target in &communication.can_message {
            if target.trim().is_empty() {
                errors.push(format!(
                    "agent_profiles['{}'].communication.can_message must not contain empty values",
                    agent_id
                ));
                continue;
            }
            if !has_workflow_agent_profile(config, target) {
                errors.push(format!(
                    "agent_profiles['{}'].communication.can_message references unknown agent '{}'",
                    agent_id, target
                ));
            }
        }
    }

    for (channel_name, channel) in &config.agent_channels {
        if channel_name.trim().is_empty() {
            errors.push("agent_channels contains an empty channel name".to_string());
            continue;
        }
        if channel.description.as_deref().is_some_and(|value| value.trim().is_empty()) {
            errors.push(format!("agent_channels['{}'].description must not be empty", channel_name));
        }
        if channel.participants.is_empty() {
            errors.push(format!("agent_channels['{}'].participants must include at least one agent", channel_name));
        }
        if channel.max_context_chars == Some(0) {
            errors.push(format!("agent_channels['{}'].max_context_chars must be greater than 0", channel_name));
        }
        for participant in &channel.participants {
            if participant.trim().is_empty() {
                errors.push(format!("agent_channels['{}'].participants must not contain empty values", channel_name));
                continue;
            }
            if !has_workflow_agent_profile(config, participant) {
                errors.push(format!(
                    "agent_channels['{}'].participants references unknown agent '{}'",
                    channel_name, participant
                ));
            }
        }
    }

    for (phase_id, binding) in &config.phase_mcp_bindings {
        if phase_id.trim().is_empty() {
            errors.push("phase_mcp_bindings contains an empty phase id".to_string());
            continue;
        }
        if binding.servers.is_empty() {
            errors.push(format!("phase_mcp_bindings['{}'].servers must include at least one MCP server", phase_id));
            continue;
        }
        for server in &binding.servers {
            if server.trim().is_empty() {
                errors.push(format!("phase_mcp_bindings['{}'].servers must not contain empty values", phase_id));
                continue;
            }
            if !config.mcp_servers.contains_key(server) {
                errors.push(format!(
                    "phase_mcp_bindings['{}'].servers references unknown MCP server '{}'",
                    phase_id, server
                ));
            }
        }
    }

    for (name, definition) in &config.tools {
        if name.trim().is_empty() {
            errors.push("tools contains an empty tool name".to_string());
            continue;
        }
        if definition.executable.trim().is_empty() {
            errors.push(format!("tools['{}'].executable must not be empty", name));
        }
        if definition.base_args.iter().any(|arg| arg.trim().is_empty()) {
            errors.push(format!("tools['{}'].base_args must not contain empty values", name));
        }
        if definition.context_window.is_some_and(|value| value == 0) {
            errors.push(format!("tools['{}'].context_window must be greater than 0 when set", name));
        }
    }

    if let Some(integrations) = &config.integrations {
        if let Some(tasks) = &integrations.tasks {
            if tasks.provider.trim().is_empty() {
                errors.push("integrations.tasks.provider must not be empty".to_string());
            }
        }
        if let Some(git) = &integrations.git {
            if git.provider.trim().is_empty() {
                errors.push("integrations.git.provider must not be empty".to_string());
            }
            if let Some(base_branch) = git.base_branch.as_deref() {
                if base_branch.trim().is_empty() {
                    errors.push("integrations.git.base_branch must not be empty when set".to_string());
                }
            }
        }
    }

    let mut schedule_ids = BTreeMap::<String, usize>::new();
    for schedule in &config.schedules {
        if schedule.id.trim().is_empty() {
            errors.push("schedules contains an empty schedule id".to_string());
            continue;
        }

        let schedule_id = schedule.id.trim();
        let normalized = schedule_id.to_ascii_lowercase();
        if let Some(existing) = schedule_ids.insert(normalized.clone(), 1) {
            let _ = existing;
            errors.push(format!("duplicate schedule id '{}'", schedule_id));
        }

        if schedule.cron.trim().is_empty() {
            errors.push(format!("schedules['{}'].cron must not be empty", schedule_id));
        }
        if schedule.workflow_ref.is_none() {
            errors.push(format!("schedules['{}'] must define workflow_ref", schedule_id));
        }
        if let Some(workflow_ref) = schedule.workflow_ref.as_deref() {
            if workflow_ref.trim().is_empty() {
                errors.push(format!("schedules['{}'].workflow_ref must not be empty", schedule_id));
            } else if !config.workflows.iter().any(|workflow| workflow.id.eq_ignore_ascii_case(workflow_ref)) {
                errors.push(format!("schedules['{}'].workflow_ref '{}' does not exist", schedule_id, workflow_ref));
            }
        }
        if let Some(command) = schedule.command.as_deref() {
            if command.trim().is_empty() {
                errors.push(format!("schedules['{}'].command must not be empty", schedule_id));
            } else {
                errors.push(format!("schedules['{}'].command is no longer supported; use workflow_ref", schedule_id));
            }
        }
        if let Err(error) = validate_cron_expression(schedule.cron.as_str()) {
            errors.push(format!("schedules['{}'].cron is not valid: {}", schedule_id, error));
        } else if schedule.cron.trim().starts_with('@') {
            let shortcut = schedule.cron.trim().to_ascii_lowercase();
            if !is_supported_shortcut_cron(shortcut.as_str()) {
                errors.push(format!("schedules['{}'].cron shortcut '{}' is not supported", schedule_id, schedule.cron));
            }
        }
    }

    let mut trigger_ids = BTreeMap::<String, usize>::new();
    for trigger in &config.triggers {
        if trigger.id.trim().is_empty() {
            errors.push("triggers contains an empty trigger id".to_string());
            continue;
        }

        let trigger_id = trigger.id.trim();
        let normalized = trigger_id.to_ascii_lowercase();
        if let Some(existing) = trigger_ids.insert(normalized.clone(), 1) {
            let _ = existing;
            errors.push(format!("duplicate trigger id '{}'", trigger_id));
        }

        if trigger.workflow_ref.is_none() {
            errors.push(format!("triggers['{}'] must define workflow_ref", trigger_id));
        }
        if let Some(workflow_ref) = trigger.workflow_ref.as_deref() {
            if workflow_ref.trim().is_empty() {
                errors.push(format!("triggers['{}'].workflow_ref must not be empty", trigger_id));
            } else if !config.workflows.iter().any(|workflow| workflow.id.eq_ignore_ascii_case(workflow_ref)) {
                errors.push(format!("triggers['{}'].workflow_ref '{}' does not exist", trigger_id, workflow_ref));
            }
        }

        match trigger.trigger_type {
            crate::workflow_config::TriggerType::FileWatcher => {
                match crate::workflow_config::FileWatcherTriggerConfig::try_from_value(&trigger.config) {
                    Ok(fw_config) => {
                        if fw_config.paths.is_empty() {
                            errors.push(format!(
                                "triggers['{}'].config.paths must not be empty for file_watcher triggers",
                                trigger_id
                            ));
                        }
                    }
                    Err(error) => {
                        errors.push(format!(
                            "triggers['{}'].config is not a valid file_watcher config: {}",
                            trigger_id, error
                        ));
                    }
                }
            }
            crate::workflow_config::TriggerType::Webhook | crate::workflow_config::TriggerType::GithubWebhook => {
                match crate::workflow_config::WebhookTriggerConfig::try_from_value(&trigger.config) {
                    Ok(wh_config) => {
                        if wh_config.max_triggers_per_minute == 0 {
                            errors.push(format!(
                                "triggers['{}'].config.max_triggers_per_minute must be greater than zero",
                                trigger_id
                            ));
                        }
                    }
                    Err(error) => {
                        errors.push(format!(
                            "triggers['{}'].config is not a valid webhook config: {}",
                            trigger_id, error
                        ));
                    }
                }
            }
            crate::workflow_config::TriggerType::Plugin => {
                // Plugin triggers delegate event production to a trigger
                // backend plugin discovered by the daemon. The config block
                // is forwarded opaquely to the plugin via `trigger/watch`;
                // the host does not validate its shape.
            }
        }
    }

    if let Some(daemon) = &config.daemon {
        if daemon.interval_secs == Some(0) {
            errors.push("daemon.interval_secs must be greater than zero when set".to_string());
        }
        if daemon.pool_size == Some(0) {
            errors.push("daemon.pool_size must be greater than zero when set".to_string());
        }
        if daemon.active_hours.as_deref().is_some_and(|value| value.trim().is_empty()) {
            errors.push("daemon.active_hours must not be empty when set".to_string());
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

// ---------------------------------------------------------------------------
// Declared-but-unenforced config warnings
// ---------------------------------------------------------------------------

/// A structured warning for workflow YAML that parses, validates, and
/// round-trips but is silently ignored (or only partially honoured) by the
/// runtime. Warnings never fail a compile or validation — existing configs
/// keep compiling.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UnenforcedFieldWarning {
    /// Dotted path of the offending declaration, e.g. `daemon.pool_size`.
    pub field: String,
    /// Source file the declaration came from.
    pub source: String,
    /// One-line explanation of what is/isn't enforced and where the real
    /// knob lives.
    pub message: String,
}

impl std::fmt::Display for UnenforcedFieldWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: `{}` is declared but not enforced — {}", self.source, self.field, self.message)
    }
}

enum UnenforcedDetector {
    /// A key under the top-level `daemon:` block. The slice lists the
    /// canonical key first, followed by serde aliases.
    DaemonKey(&'static [&'static str]),
    /// Any `phases.<id>.evals:` block.
    PhaseEvals,
}

struct UnenforcedRule {
    detector: UnenforcedDetector,
    explanation: &'static str,
}

/// THE single registry of declared-but-unenforced workflow YAML fields.
///
/// Every emission point (compile-path stderr warnings, `animus workflow
/// config validate`, `animus workflow config compile`) reads this table.
/// When enforcement for a field lands in the runtime, delete its entry here
/// and update the matching section of `docs/reference/workflow-yaml.md`.
const UNENFORCED_RULES: &[UnenforcedRule] = &[
    UnenforcedRule {
        detector: UnenforcedDetector::DaemonKey(&["max_task_retries"]),
        explanation: "this field is a no-op: the daemon never reads it, so task retry limits are not enforced anywhere yet",
    },
    UnenforcedRule {
        detector: UnenforcedDetector::DaemonKey(&["retry_cooldown_secs"]),
        explanation: "this field is a no-op: the daemon never reads it, so retry cooldowns are not enforced anywhere yet",
    },
    UnenforcedRule {
        detector: UnenforcedDetector::DaemonKey(&["pool_size", "max_agents"]),
        explanation: "the daemon ignores this YAML value; set pool size via `animus daemon config --pool-size <n>` (persisted, hot-reloaded) or `animus daemon run --pool-size <n>`",
    },
    UnenforcedRule {
        detector: UnenforcedDetector::DaemonKey(&["interval_secs"]),
        explanation: "the daemon ignores this YAML value; set the tick interval via `animus daemon config --interval-secs <n>` (persisted, hot-reloaded) or `animus daemon run --interval-secs <n>`",
    },
    UnenforcedRule {
        detector: UnenforcedDetector::DaemonKey(&["auto_run_ready"]),
        explanation: "this key was removed: the daemon is queue-only and never auto-dispatches Ready tasks; enqueue work with `animus queue enqueue` or drive it from a `schedules:` cron entry",
    },
    UnenforcedRule {
        detector: UnenforcedDetector::DaemonKey(&["auto_merge"]),
        explanation: "this key was removed: the daemon has no merge/PR automation; express commit/push/PR/merge as a command phase (a phase with a `command:` running `git`/`gh`)",
    },
    UnenforcedRule {
        detector: UnenforcedDetector::DaemonKey(&["auto_pr"]),
        explanation: "this key was removed: the daemon has no merge/PR automation; express PR creation as a command phase (a phase with a `command:` running `gh pr create`)",
    },
    UnenforcedRule {
        detector: UnenforcedDetector::DaemonKey(&["auto_commit_before_merge"]),
        explanation: "this key was removed: the daemon has no merge/PR automation; express commit/merge as a command phase (a phase with a `command:` running `git`)",
    },
    UnenforcedRule {
        detector: UnenforcedDetector::DaemonKey(&["auto_prune_worktrees"]),
        explanation: "this key was removed: the daemon has no merge/PR automation; express worktree cleanup as a command phase (a phase with a `command:` running `git worktree`)",
    },
    UnenforcedRule {
        detector: UnenforcedDetector::PhaseEvals,
        explanation: "evals parse and validate but are not yet executed by the workflow runner — phases advance regardless of this gate (enforcement lands with a future animus-workflow-runner-default release)",
    },
    // NOTE: `budget:` blocks were removed from this registry when daemon-side
    // enforcement landed (housekeeping-cadence cap sweep). Enforcement still
    // requires a running daemon; that caveat is documented in
    // `docs/reference/workflow-yaml.md#budget` instead of warned about here.
];

fn yaml_mapping_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value.as_mapping().and_then(|map| map.get(serde_yaml::Value::String(key.to_string())))
}

fn detect_daemon_keys(doc: &serde_yaml::Value, keys: &[&str], explanation: &str, out: &mut Vec<(String, String)>) {
    let Some(daemon) = yaml_mapping_get(doc, "daemon") else {
        return;
    };
    for key in keys {
        if yaml_mapping_get(daemon, key).is_some() {
            out.push((format!("daemon.{key}"), explanation.to_string()));
        }
    }
}

fn detect_phase_evals(doc: &serde_yaml::Value, explanation: &str, out: &mut Vec<(String, String)>) {
    let Some(phases) = yaml_mapping_get(doc, "phases").and_then(serde_yaml::Value::as_mapping) else {
        return;
    };
    for (phase_id, definition) in phases {
        let Some(phase_id) = phase_id.as_str() else {
            continue;
        };
        if yaml_mapping_get(definition, "evals").is_some_and(|evals| !evals.is_null()) {
            out.push((format!("phases.{phase_id}.evals"), explanation.to_string()));
        }
    }
}

/// Scan one raw YAML source for declared-but-unenforced fields. Returns one
/// warning per declaration. Unparseable YAML yields no warnings — the
/// compile pipeline reports parse errors with proper diagnostics.
pub fn unenforced_yaml_field_warnings(yaml: &str, source_label: &str) -> Vec<UnenforcedFieldWarning> {
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
        return Vec::new();
    };
    let mut hits: Vec<(String, String)> = Vec::new();
    for rule in UNENFORCED_RULES {
        match rule.detector {
            UnenforcedDetector::DaemonKey(keys) => detect_daemon_keys(&doc, keys, rule.explanation, &mut hits),
            UnenforcedDetector::PhaseEvals => detect_phase_evals(&doc, rule.explanation, &mut hits),
        }
    }
    hits.into_iter()
        .map(|(field, message)| UnenforcedFieldWarning { field, source: source_label.to_string(), message })
        .collect()
}

/// Scan every workflow YAML source of a project for declared-but-unenforced
/// fields. Read errors are ignored — the compile pipeline owns IO
/// diagnostics.
pub fn unenforced_project_yaml_warnings(project_root: &Path) -> Vec<UnenforcedFieldWarning> {
    let Ok(sources) = super::yaml_compiler::collect_project_yaml_workflow_sources(project_root) else {
        return Vec::new();
    };
    sources
        .iter()
        .flat_map(|(path, content)| unenforced_yaml_field_warnings(content, &path.display().to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// Explicit skill-reference warnings
// ---------------------------------------------------------------------------

/// A structured warning for an EXPLICIT workflow-YAML skill declaration
/// (`phases.<id>.skills` or `agents.<id>.skills`) that does not resolve
/// against the project's skill sources. Warnings never fail compile or
/// validation — at runtime a missing skill is recorded on phase metadata
/// instead of failing the run, but a typo'd name must not be a silent
/// no-op at authoring time.
///
/// Only raw-YAML declarations are scanned, which is exactly the
/// explicit/implicit split the runtime's `RequestedPhaseSkills.implicit`
/// uses: builtin persona profiles that reference pack-provided skills
/// (e.g. `animus.core-skills` names) never appear in project YAML, so
/// they get no warning here even when the pack is not installed yet.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillReferenceWarning {
    /// Dotted path of the declaration, e.g. `phases.code-review.skills`.
    pub field: String,
    /// Source file the declaration came from.
    pub source: String,
    /// The skill name that failed to resolve.
    pub skill: String,
}

impl std::fmt::Display for SkillReferenceWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: `{}` references skill '{}' which does not resolve against this project's skill sources (project / user / installed) — check `animus skill list`, or create it with `animus skill create`",
            self.source, self.field, self.skill
        )
    }
}

/// Collect `(field_path, skill_name)` pairs for every explicit `skills:`
/// entry under top-level `phases:` and `agents:` mappings of one raw YAML
/// document.
fn collect_explicit_yaml_skill_declarations(doc: &serde_yaml::Value) -> Vec<(String, String)> {
    let mut declarations = Vec::new();
    for block in ["phases", "agents"] {
        let Some(entries) = yaml_mapping_get(doc, block).and_then(serde_yaml::Value::as_mapping) else {
            continue;
        };
        for (entry_id, definition) in entries {
            let Some(entry_id) = entry_id.as_str() else {
                continue;
            };
            let Some(skills) = yaml_mapping_get(definition, "skills").and_then(serde_yaml::Value::as_sequence) else {
                continue;
            };
            for skill in skills {
                if let Some(name) = skill.as_str().map(str::trim).filter(|name| !name.is_empty()) {
                    declarations.push((format!("{block}.{entry_id}.skills"), name.to_string()));
                }
            }
        }
    }
    declarations
}

/// Scan one raw YAML source for explicit skill declarations that do not
/// resolve via `skill_resolves`. Unparseable YAML yields no warnings —
/// the compile pipeline reports parse errors with proper diagnostics.
pub fn missing_skill_yaml_warnings(
    yaml: &str,
    source_label: &str,
    skill_resolves: &dyn Fn(&str) -> bool,
) -> Vec<SkillReferenceWarning> {
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
        return Vec::new();
    };
    collect_explicit_yaml_skill_declarations(&doc)
        .into_iter()
        // Skip names that still carry an uninterpolated `${...}`
        // placeholder — they cannot be checked statically and the
        // compiler owns unset-var diagnostics.
        .filter(|(_, skill)| !skill.contains("${"))
        .filter(|(_, skill)| !skill_resolves(skill))
        .map(|(field, skill)| SkillReferenceWarning { field, source: source_label.to_string(), skill })
        .collect()
}

/// Resolve explicit skill declarations in the given YAML sources against
/// the same scoped skill-source chain the runtime uses
/// (`load_skill_sources`: agent-host < installed < user < project).
/// Best-effort: when the source chain cannot be loaded no warnings are
/// produced — a degraded environment must not turn into compile noise.
pub fn missing_skill_reference_warnings_for_sources(
    project_root: &Path,
    yaml_sources: &[(std::path::PathBuf, String)],
) -> Vec<SkillReferenceWarning> {
    if yaml_sources.is_empty() {
        return Vec::new();
    }
    let Ok(sources) = crate::skill_scoping::load_skill_sources(project_root, None) else {
        return Vec::new();
    };
    // Mirrors `skill_resolution::resolve_skill` lookup: exact name match
    // against any source in the chain (priority order is irrelevant for
    // existence).
    let skill_resolves = |name: &str| sources.iter().any(|source| source.skills.contains_key(name));
    yaml_sources
        .iter()
        .flat_map(|(path, content)| {
            let source_label = path.display().to_string();
            // The real compiler interpolates `${VAR}` before parsing, so
            // lint the interpolated content when interpolation succeeds.
            // On failure (unset required var — the compiler owns that
            // diagnostic) fall back to the raw content; any names that
            // still carry a `${` placeholder are skipped below.
            let interpolated = super::env_interp::interpolate_env(content, &source_label).ok();
            missing_skill_yaml_warnings(interpolated.as_deref().unwrap_or(content), &source_label, &skill_resolves)
        })
        .collect()
}

/// Scan every workflow YAML source of a project for explicit skill
/// declarations that do not resolve. Read errors are ignored — the
/// compile pipeline owns IO diagnostics.
pub fn missing_project_skill_reference_warnings(project_root: &Path) -> Vec<SkillReferenceWarning> {
    let Ok(yaml_sources) = super::yaml_compiler::collect_project_yaml_workflow_sources(project_root) else {
        return Vec::new();
    };
    missing_skill_reference_warnings_for_sources(project_root, &yaml_sources)
}
