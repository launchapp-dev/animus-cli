use std::collections::{BTreeMap, HashMap};

use anyhow::{anyhow, Context, Result};

use crate::agent_runtime_config::{CommandCwdMode, PhaseCommandDefinition, PhaseExecutionMode, PhaseManualDefinition};
use crate::PhaseExecutionDefinition;

use super::builtins::builtin_workflow_config;
use super::types::*;
use super::yaml_scaffold::title_case_phase_id;
use super::yaml_types::*;

pub(super) fn parse_cwd_mode(value: &str) -> Result<CommandCwdMode> {
    match value.to_ascii_lowercase().replace('-', "_").as_str() {
        "project_root" => Ok(CommandCwdMode::ProjectRoot),
        "task_root" => Ok(CommandCwdMode::TaskRoot),
        "path" => Ok(CommandCwdMode::Path),
        other => Err(anyhow!("unknown cwd_mode '{}' (expected project_root, task_root, or path)", other)),
    }
}

pub(super) fn parse_merge_strategy(value: &str) -> Result<MergeStrategy> {
    match value.to_ascii_lowercase().as_str() {
        "squash" => Ok(MergeStrategy::Squash),
        "merge" => Ok(MergeStrategy::Merge),
        "rebase" => Ok(MergeStrategy::Rebase),
        _ => Err(anyhow!("phases['merge'].strategy must be one of: squash, merge, rebase (got '{}')", value)),
    }
}

pub(super) fn yaml_phase_to_execution_definition(
    phase_id: &str,
    yaml: YamlPhaseDefinition,
) -> Result<PhaseExecutionDefinition> {
    let mode = yaml.mode;
    let mode_label = format!("{:?}", mode).to_ascii_lowercase();

    let command = match (&mode, yaml.command) {
        (PhaseExecutionMode::Command, Some(cmd)) => Some(PhaseCommandDefinition {
            program: cmd.program,
            args: cmd.args,
            env: cmd.env,
            cwd_mode: cmd.cwd_mode.as_deref().map(parse_cwd_mode).transpose()?.unwrap_or(CommandCwdMode::ProjectRoot),
            cwd_path: cmd.cwd_path,
            timeout_secs: cmd.timeout_secs,
            success_exit_codes: cmd.success_exit_codes.unwrap_or_else(|| vec![0]),
            parse_json_output: cmd.parse_json_output.unwrap_or(false),
            expected_result_kind: cmd.expected_result_kind,
            expected_schema: cmd.expected_schema,
            category: cmd.category,
            failure_pattern: cmd.failure_pattern,
            excerpt_max_chars: cmd.excerpt_max_chars,
            on_success_verdict: cmd.on_success_verdict,
            on_failure_verdict: cmd.on_failure_verdict,
            confidence: cmd.confidence,
            failure_risk: cmd.failure_risk,
        }),
        (PhaseExecutionMode::Command, None) => {
            return Err(anyhow!("phases['{}'] mode 'command' requires a command block", phase_id));
        }
        (_, Some(_)) => {
            return Err(anyhow!(
                "phases['{}'] mode '{}' must not include a command block",
                phase_id,
                mode_label.clone()
            ));
        }
        _ => None,
    };

    let manual = match (&mode, yaml.manual) {
        (PhaseExecutionMode::Manual, Some(m)) => Some(PhaseManualDefinition {
            instructions: m.instructions,
            approval_note_required: m.approval_note_required.unwrap_or(false),
            timeout_secs: m.timeout_secs,
        }),
        (PhaseExecutionMode::Manual, None) => {
            return Err(anyhow!("phases['{}'] mode 'manual' requires a manual block", phase_id));
        }
        (_, Some(_)) => {
            return Err(anyhow!("phases['{}'] mode '{}' must not include a manual block", phase_id, mode_label));
        }
        _ => None,
    };

    Ok(PhaseExecutionDefinition {
        mode,
        agent_id: yaml.agent,
        directive: yaml.directive,
        skills: yaml.skills,
        runtime: yaml.runtime,
        capabilities: yaml.capabilities,
        output_contract: yaml.output_contract,
        output_json_schema: yaml.output_json_schema,
        decision_contract: yaml.decision_contract,
        retry: yaml.retry,
        command,
        manual,
        system_prompt: yaml.system_prompt,
        default_tool: yaml.default_tool,
    })
}

pub(super) fn workflow_phase_entry_to_yaml(entry: &WorkflowPhaseEntry) -> YamlPhaseEntry {
    match entry {
        WorkflowPhaseEntry::Simple(id) => YamlPhaseEntry::Simple(id.clone()),
        WorkflowPhaseEntry::SubWorkflow(sub) => {
            YamlPhaseEntry::SubWorkflow(YamlSubWorkflowRef { workflow_ref: sub.workflow_ref.clone() })
        }
        WorkflowPhaseEntry::Rich(config) => {
            let mut map = HashMap::new();
            map.insert(
                config.id.clone(),
                YamlPhaseRichConfig {
                    max_rework_attempts: config.max_rework_attempts,
                    skip_if: config.skip_if.clone(),
                    on_verdict: config.on_verdict.clone(),
                },
            );
            YamlPhaseEntry::Rich(map)
        }
    }
}

pub(super) fn workflow_definition_to_yaml(definition: &WorkflowDefinition) -> YamlWorkflowDefinition {
    YamlWorkflowDefinition {
        id: definition.id.clone(),
        name: Some(definition.name.clone()),
        description: Some(definition.description.clone()),
        phases: definition.phases.iter().map(workflow_phase_entry_to_yaml).collect(),
        post_success: definition.post_success.clone().map(post_success_config_to_yaml),
        variables: definition.variables.clone(),
    }
}

pub(super) fn post_success_config_to_yaml(config: PostSuccessConfig) -> YamlPostSuccessConfig {
    YamlPostSuccessConfig { merge: config.merge.map(merge_config_to_yaml) }
}

pub(super) fn merge_config_to_yaml(config: MergeConfig) -> YamlMergeConfig {
    YamlMergeConfig {
        strategy: Some(match config.strategy {
            MergeStrategy::Squash => "squash".to_string(),
            MergeStrategy::Merge => "merge".to_string(),
            MergeStrategy::Rebase => "rebase".to_string(),
        }),
        target_branch: config.target_branch,
        create_pr: config.create_pr,
        auto_merge: config.auto_merge,
        cleanup_worktree: config.cleanup_worktree,
    }
}

pub(super) fn phase_execution_definition_to_yaml(definition: &PhaseExecutionDefinition) -> YamlPhaseDefinition {
    YamlPhaseDefinition {
        mode: definition.mode.clone(),
        agent: definition.agent_id.clone(),
        command: definition.command.clone().map(|command| YamlCommandDefinition {
            program: command.program,
            args: command.args,
            env: command.env,
            cwd_mode: Some(match command.cwd_mode {
                CommandCwdMode::ProjectRoot => "project_root".to_string(),
                CommandCwdMode::TaskRoot => "task_root".to_string(),
                CommandCwdMode::Path => "path".to_string(),
            }),
            cwd_path: command.cwd_path,
            timeout_secs: command.timeout_secs,
            success_exit_codes: Some(command.success_exit_codes),
            parse_json_output: Some(command.parse_json_output),
            expected_result_kind: command.expected_result_kind,
            expected_schema: command.expected_schema,
            category: command.category,
            failure_pattern: command.failure_pattern,
            excerpt_max_chars: command.excerpt_max_chars,
            on_success_verdict: command.on_success_verdict,
            on_failure_verdict: command.on_failure_verdict,
            confidence: command.confidence,
            failure_risk: command.failure_risk,
        }),
        manual: definition.manual.clone().map(|manual| YamlManualDefinition {
            instructions: manual.instructions,
            approval_note_required: Some(manual.approval_note_required),
            timeout_secs: manual.timeout_secs,
        }),
        directive: definition.directive.clone(),
        system_prompt: definition.system_prompt.clone(),
        skills: definition.skills.clone(),
        runtime: definition.runtime.clone(),
        capabilities: definition.capabilities.clone(),
        output_contract: definition.output_contract.clone(),
        output_json_schema: definition.output_json_schema.clone(),
        decision_contract: definition.decision_contract.clone(),
        retry: definition.retry.clone(),
        default_tool: definition.default_tool.clone(),
    }
}

pub(super) fn workflow_config_to_yaml_file(config: &WorkflowConfig) -> YamlWorkflowFile {
    YamlWorkflowFile {
        default_workflow_ref: Some(config.default_workflow_ref.clone()),
        phase_catalog: if config.phase_catalog.is_empty() { None } else { Some(config.phase_catalog.clone()) },
        workflows: config.workflows.iter().map(workflow_definition_to_yaml).collect(),
        phases: config
            .phase_definitions
            .iter()
            .map(|(id, definition)| (id.clone(), phase_execution_definition_to_yaml(definition)))
            .collect(),
        agents: config.agent_profiles.clone(),
        tools_allowlist: config.tools_allowlist.clone(),
        mcp_servers: config.mcp_servers.clone(),
        tools: config.tools.clone(),
        integrations: config.integrations.clone(),
        schedules: config.schedules.clone(),
        daemon: config.daemon.clone(),
    }
}

pub(super) fn yaml_phase_entry_to_workflow_phase_entry(entry: YamlPhaseEntry) -> Result<WorkflowPhaseEntry> {
    match entry {
        YamlPhaseEntry::Simple(id) => Ok(WorkflowPhaseEntry::Simple(id)),
        YamlPhaseEntry::SubWorkflow(sub) => {
            Ok(WorkflowPhaseEntry::SubWorkflow(SubWorkflowRef { workflow_ref: sub.workflow_ref }))
        }
        YamlPhaseEntry::Rich(map) => {
            if map.len() != 1 {
                return Err(anyhow!("rich phase entry must have exactly one key (the phase id), got {}", map.len()));
            }
            let (id, config) = map.into_iter().next().unwrap();
            Ok(WorkflowPhaseEntry::Rich(WorkflowPhaseConfig {
                id,
                max_rework_attempts: config.max_rework_attempts,
                on_verdict: config.on_verdict,
                skip_if: config.skip_if,
            }))
        }
    }
}

pub(super) fn yaml_workflow_to_workflow_definition(yaml: YamlWorkflowDefinition) -> Result<WorkflowDefinition> {
    let post_success = match yaml.post_success {
        Some(post_success) => Some(yaml_post_success_to_post_success_config(post_success)?),
        None => None,
    };

    let phases = yaml.phases.into_iter().map(yaml_phase_entry_to_workflow_phase_entry).collect::<Result<Vec<_>>>()?;
    Ok(WorkflowDefinition {
        id: yaml.id.clone(),
        name: yaml.name.unwrap_or_else(|| yaml.id.clone()),
        description: yaml.description.unwrap_or_default(),
        phases,
        post_success,
        variables: yaml.variables,
    })
}

pub(super) fn yaml_post_success_to_post_success_config(yaml: YamlPostSuccessConfig) -> Result<PostSuccessConfig> {
    let merge = match yaml.merge {
        Some(merge) => Some(yaml_merge_to_merge_config(merge)?),
        None => None,
    };
    Ok(PostSuccessConfig { merge })
}

pub(super) fn yaml_merge_to_merge_config(yaml: YamlMergeConfig) -> Result<MergeConfig> {
    Ok(MergeConfig {
        strategy: yaml.strategy.as_deref().map(parse_merge_strategy).transpose()?.unwrap_or_default(),
        target_branch: yaml.target_branch,
        create_pr: yaml.create_pr,
        auto_merge: yaml.auto_merge,
        cleanup_worktree: yaml.cleanup_worktree,
    })
}

pub fn parse_yaml_workflow_config_with_base(yaml_str: &str, base: &WorkflowConfig) -> Result<WorkflowConfig> {
    let yaml_file: YamlWorkflowFile = serde_yaml::from_str(yaml_str).context("failed to parse YAML workflow config")?;

    let workflows =
        yaml_file.workflows.into_iter().map(yaml_workflow_to_workflow_definition).collect::<Result<Vec<_>>>()?;

    let mut phase_definitions = BTreeMap::new();
    let mut auto_phase_catalog = BTreeMap::new();
    for (phase_id, yaml_phase) in yaml_file.phases {
        let definition = yaml_phase_to_execution_definition(&phase_id, yaml_phase)
            .with_context(|| format!("error converting YAML phase '{}'", phase_id))?;
        if !auto_phase_catalog.contains_key(&phase_id) {
            auto_phase_catalog.insert(
                phase_id.clone(),
                PhaseUiDefinition {
                    label: title_case_phase_id(&phase_id),
                    description: String::new(),
                    category: match definition.mode {
                        PhaseExecutionMode::Command => "build".to_string(),
                        PhaseExecutionMode::Manual => "manual".to_string(),
                        PhaseExecutionMode::Agent => "agent".to_string(),
                    },
                    icon: None,
                    docs_url: None,
                    tags: Vec::new(),
                    visible: true,
                },
            );
        }
        phase_definitions.insert(phase_id, definition);
    }

    let default_workflow_ref = yaml_file.default_workflow_ref.unwrap_or_default();
    let mut phase_catalog = yaml_file.phase_catalog.unwrap_or_else(|| base.phase_catalog.clone());
    for (id, ui_def) in auto_phase_catalog {
        phase_catalog.entry(id).or_insert(ui_def);
    }

    Ok(WorkflowConfig {
        schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: WORKFLOW_CONFIG_VERSION,
        default_workflow_ref,
        phase_catalog,
        workflows: if workflows.is_empty() { base.workflows.clone() } else { workflows },
        checkpoint_retention: WorkflowCheckpointRetentionConfig::default(),
        phase_definitions,
        agent_profiles: yaml_file.agents,
        tools_allowlist: yaml_file.tools_allowlist,
        mcp_servers: yaml_file.mcp_servers,
        tools: yaml_file.tools,
        integrations: yaml_file.integrations,
        schedules: yaml_file.schedules,
        daemon: yaml_file.daemon,
    })
}

pub fn parse_yaml_workflow_config(yaml_str: &str) -> Result<WorkflowConfig> {
    let base = builtin_workflow_config();
    let mut config = parse_yaml_workflow_config_with_base(yaml_str, &base)?;
    if config.default_workflow_ref.trim().is_empty() {
        config.default_workflow_ref = base.default_workflow_ref;
    }
    Ok(config)
}
