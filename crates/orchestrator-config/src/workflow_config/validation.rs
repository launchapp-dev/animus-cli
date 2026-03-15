use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::agent_runtime_config::{AgentRuntimeConfig, PhaseExecutionMode};
use crate::skill_resolution::{resolve_skills, resolve_skills_for_project};
use crate::skill_scoping::load_builtin_skills;

use super::types::*;

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

pub fn validate_workflow_and_runtime_configs(workflow: &WorkflowConfig, runtime: &AgentRuntimeConfig) -> Result<()> {
    validate_workflow_config(workflow)?;

    let mut errors = Vec::new();
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

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

pub fn validate_workflow_config(config: &WorkflowConfig) -> Result<()> {
    validate_workflow_config_with_project_root(config, None)
}

fn validate_skill_references(
    field_path: &str,
    skills: &[String],
    project_root: Option<&Path>,
    errors: &mut Vec<String>,
) {
    let mut requested_skills = Vec::with_capacity(skills.len());
    for skill_name in skills {
        let trimmed = skill_name.trim();
        if trimmed.is_empty() {
            errors.push(format!("{field_path} must not contain empty values"));
            return;
        }
        requested_skills.push(trimmed.to_string());
    }

    let result = if let Some(project_root) = project_root {
        resolve_skills_for_project(&requested_skills, project_root).map(|_| ())
    } else {
        load_builtin_skills().and_then(|builtin| resolve_skills(&requested_skills, &[builtin]).map(|_| ()))
    };

    if let Err(error) = result {
        errors.push(format!("{field_path} validation failed: {error}"));
    }
}

pub fn validate_workflow_config_with_project_root(config: &WorkflowConfig, project_root: Option<&Path>) -> Result<()> {
    let mut errors = Vec::new();

    if config.schema.trim() != WORKFLOW_CONFIG_SCHEMA_ID {
        errors.push(format!("schema must be '{}' (got '{}')", WORKFLOW_CONFIG_SCHEMA_ID, config.schema));
    }

    if config.version != WORKFLOW_CONFIG_VERSION {
        errors.push(format!("version must be {} (got {})", WORKFLOW_CONFIG_VERSION, config.version));
    }

    if config.default_workflow_ref.trim().is_empty() {
        errors.push("default_workflow_ref must not be empty".to_string());
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

    if config.workflows.is_empty() {
        errors.push("workflows must include at least one workflow".to_string());
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

        if let Some(post_success) = &workflow.post_success {
            if let Some(merge) = &post_success.merge {
                if merge.target_branch.trim().is_empty() {
                    errors.push(format!(
                        "workflow '{}' post_success.merge.target_branch must not be empty",
                        workflow_ref
                    ));
                }

                if !merge_strategy_is_valid(&merge.strategy) {
                    errors.push(format!("workflow '{}' post_success.merge.strategy is not supported", workflow_ref));
                }
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

    if config.workflows.iter().all(|workflow| !workflow.id.eq_ignore_ascii_case(config.default_workflow_ref.as_str())) {
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
    }

    for (name, definition) in &config.mcp_servers {
        if name.trim().is_empty() {
            errors.push("mcp_servers contains an empty server name".to_string());
            continue;
        }
        if definition.command.trim().is_empty() {
            errors.push(format!("mcp_servers['{}'].command must not be empty", name));
        }
        if definition.args.iter().any(|arg| arg.trim().is_empty()) {
            errors.push(format!("mcp_servers['{}'].args must not contain empty values", name));
        }
        if definition.tools.iter().any(|tool| tool.trim().is_empty()) {
            errors.push(format!("mcp_servers['{}'].tools must not contain empty values", name));
        }
        if definition.transport.as_deref().is_some_and(|transport| transport.trim().is_empty()) {
            errors.push(format!("mcp_servers['{}'].transport must not be empty when set", name));
        }
        if definition.env.iter().any(|(key, value)| key.trim().is_empty() || value.trim().is_empty()) {
            errors.push(format!("mcp_servers['{}'].env must not contain empty keys or values", name));
        }
    }

    for (agent_id, profile) in &config.agent_profiles {
        validate_skill_references(
            format!("agent_profiles['{}'].skills", agent_id).as_str(),
            &profile.skills,
            project_root,
            &mut errors,
        );
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

    for (agent_id, profile) in &config.agent_profiles {
        for server in &profile.mcp_servers {
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
