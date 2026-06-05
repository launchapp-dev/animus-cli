use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::agent_runtime_config::{
    AgentProfile, CommandCwdMode, PhaseCommandDefinition, PhaseExecutionMode, PhaseManualDefinition,
};
use crate::PhaseExecutionDefinition;

use super::builtins::builtin_workflow_config;
use super::types::*;
use super::yaml_compiler::merge_yaml_into_config;
use super::yaml_scaffold::title_case_phase_id;
use super::yaml_types::*;

const SYSTEM_PROMPT_FILE_MAX_BYTES: u64 = 1024 * 1024;

/// Resolve an agent's `models:` name list against the model registry,
/// expanding named references into concrete `model` + `fallback_models` and
/// `tool` + `fallback_tools` values.
///
/// When `models` is non-empty:
/// - `models[0]` becomes the primary `model` (and optionally `tool`).
/// - `models[1..]` become `fallback_models` (and optionally `fallback_tools`).
///
/// When `models` is empty, existing `model`/`fallback_models` are left intact.
pub fn resolve_agent_model_references(
    profile: &mut crate::agent_runtime_config::AgentProfile,
    registry: &BTreeMap<String, super::yaml_types::ModelRegistryEntry>,
) {
    if profile.models.is_empty() {
        return;
    }

    let mut resolved_models: Vec<String> = Vec::with_capacity(profile.models.len());
    let mut resolved_tools: Vec<Option<String>> = Vec::with_capacity(profile.models.len());

    for name in &profile.models {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(entry) = registry.get(trimmed) {
            let model = entry.model.trim().to_string();
            let tool = entry.tool.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(ToOwned::to_owned);
            resolved_models.push(model);
            resolved_tools.push(tool);
        } else {
            // Treat bare strings that aren't in the registry as literal model IDs
            resolved_models.push(trimmed.to_string());
            resolved_tools.push(None);
        }
    }

    if resolved_models.is_empty() {
        return;
    }

    // Set primary model (and optional tool) from the first resolved entry
    profile.model = Some(resolved_models.remove(0));
    if let Some(tool) = resolved_tools.remove(0) {
        profile.tool = Some(tool);
    }

    // Remaining resolved entries become fallbacks
    if !resolved_models.is_empty() {
        profile.fallback_models = resolved_models;
        // Build fallback_tools: use explicit tool if provided, else empty (auto-derived at runtime)
        profile.fallback_tools = resolved_tools.into_iter().flatten().collect();
    }

    // Clear the name list after expansion to avoid double-expansion
    profile.models.clear();
}

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
        idempotency: yaml.idempotency,
        worktree: yaml.worktree.map(WorktreeConfig::from_yaml).transpose()?,
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
        worktree: definition.worktree.clone().map(YamlPhaseWorktree::Full),
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
        idempotency: definition.idempotency,
        worktree: definition.worktree.clone().map(YamlPhaseWorktree::Full),
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
        agent_channels: config.agent_channels.clone(),
        models: BTreeMap::new(),
        tools_allowlist: config.tools_allowlist.clone(),
        mcp_servers: config.mcp_servers.clone(),
        phase_mcp_bindings: config.phase_mcp_bindings.clone(),
        tools: config.tools.clone(),
        integrations: config.integrations.clone(),
        schedules: config.schedules.clone(),
        triggers: config.triggers.clone(),
        daemon: config.daemon.clone(),
        secrets: config.secrets.clone(),
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
    let worktree = yaml.worktree.map(WorktreeConfig::from_yaml).transpose()?;
    Ok(WorkflowDefinition {
        id: yaml.id.clone(),
        name: yaml.name.unwrap_or_else(|| yaml.id.clone()),
        description: yaml.description.unwrap_or_default(),
        phases,
        post_success,
        variables: yaml.variables,
        worktree,
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

fn source_label(source_path: Option<&Path>) -> String {
    source_path.map(|p| p.display().to_string()).unwrap_or_else(|| "<in-memory>".to_string())
}

fn resolve_system_prompt_file_path(raw: &str, source_path: Option<&Path>) -> PathBuf {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }
    match source_path.and_then(Path::parent) {
        Some(parent) => parent.join(candidate),
        None => candidate.to_path_buf(),
    }
}

fn find_field_line_in_agent(yaml_str: &str, agent_id: &str, field_name: &str) -> Option<usize> {
    let mut in_agents = false;
    let mut agents_indent: Option<usize> = None;
    let mut in_target_agent = false;
    let mut agent_indent: Option<usize> = None;

    for (idx, line) in yaml_str.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();

        if !in_agents {
            if trimmed.starts_with("agents:") && indent == 0 {
                in_agents = true;
                agents_indent = Some(indent);
            }
            continue;
        }

        if let Some(top_indent) = agents_indent {
            if indent <= top_indent && !trimmed.starts_with("agents:") {
                in_agents = false;
                in_target_agent = false;
                agent_indent = None;
                continue;
            }
        }

        if !in_target_agent {
            let key = trimmed.split(':').next().unwrap_or("");
            if key == agent_id {
                in_target_agent = true;
                agent_indent = Some(indent);
            }
            continue;
        }

        if let Some(target_indent) = agent_indent {
            if indent <= target_indent {
                in_target_agent = false;
                agent_indent = None;
                let key = trimmed.split(':').next().unwrap_or("");
                if key == agent_id {
                    in_target_agent = true;
                    agent_indent = Some(indent);
                }
                continue;
            }
            if trimmed.starts_with(&format!("{}:", field_name)) {
                return Some(idx + 1);
            }
        }
    }
    None
}

pub(crate) fn resolve_agent_system_prompt_files_confined_to_pack(
    agent_profiles: &mut BTreeMap<String, AgentProfile>,
    yaml_str: &str,
    source_path: &Path,
    pack_root: &Path,
) -> Result<()> {
    resolve_agent_system_prompt_files_internal(agent_profiles, yaml_str, Some(source_path), Some(pack_root))
}

fn resolve_agent_system_prompt_files_internal(
    agent_profiles: &mut BTreeMap<String, AgentProfile>,
    yaml_str: &str,
    source_path: Option<&Path>,
    pack_root: Option<&Path>,
) -> Result<()> {
    let pack_root_canonical = pack_root
        .map(|root| {
            fs::canonicalize(root).map_err(|err| {
                anyhow!(
                    "pack root '{}' could not be canonicalized for system_prompt_file confinement: {}",
                    root.display(),
                    err,
                )
            })
        })
        .transpose()?;

    for (agent_id, profile) in agent_profiles.iter_mut() {
        let Some(raw_path) = profile.system_prompt_file.clone() else {
            continue;
        };
        let trimmed = raw_path.trim();
        if trimmed.is_empty() {
            return Err(anyhow!(
                "workflow YAML at {} agent '{}' system_prompt_file must not be empty",
                source_label(source_path),
                agent_id,
            ));
        }

        let inline_set = !profile.system_prompt.trim().is_empty();
        if inline_set {
            let line = find_field_line_in_agent(yaml_str, agent_id, "system_prompt_file");
            let line_suffix = line.map(|l| format!(" line {}", l)).unwrap_or_default();
            return Err(anyhow!(
                "workflow YAML at {}{} agent '{}' sets both system_prompt and system_prompt_file; choose one",
                source_label(source_path),
                line_suffix,
                agent_id,
            ));
        }

        if pack_root.is_some() {
            let candidate = Path::new(trimmed);
            if candidate.is_absolute() {
                return Err(anyhow!(
                    "pack workflow at {} agent '{}' system_prompt_file '{}' must be a relative path inside the pack root",
                    source_label(source_path),
                    agent_id,
                    raw_path,
                ));
            }
            if candidate.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                return Err(anyhow!(
                    "pack workflow at {} agent '{}' system_prompt_file '{}' must not contain '..' segments",
                    source_label(source_path),
                    agent_id,
                    raw_path,
                ));
            }
        }

        let resolved = resolve_system_prompt_file_path(trimmed, source_path);

        if let Some(root_canonical) = pack_root_canonical.as_deref() {
            let resolved_canonical = fs::canonicalize(&resolved).map_err(|err| {
                anyhow!(
                    "pack workflow at {} agent '{}' system_prompt_file '{}' could not be canonicalized: {}",
                    source_label(source_path),
                    agent_id,
                    resolved.display(),
                    err,
                )
            })?;
            if !resolved_canonical.starts_with(root_canonical) {
                return Err(anyhow!(
                    "pack workflow at {} agent '{}' system_prompt_file '{}' resolves to '{}' which is outside the pack root '{}'",
                    source_label(source_path),
                    agent_id,
                    raw_path,
                    resolved_canonical.display(),
                    root_canonical.display(),
                ));
            }
        }

        let metadata = fs::metadata(&resolved).map_err(|err| {
            anyhow!(
                "workflow YAML at {} agent '{}' system_prompt_file '{}' could not be read: {}",
                source_label(source_path),
                agent_id,
                resolved.display(),
                err,
            )
        })?;

        if metadata.len() > SYSTEM_PROMPT_FILE_MAX_BYTES {
            return Err(anyhow!(
                "workflow YAML at {} agent '{}' system_prompt_file '{}' is {} bytes; maximum is {} bytes (1 MiB)",
                source_label(source_path),
                agent_id,
                resolved.display(),
                metadata.len(),
                SYSTEM_PROMPT_FILE_MAX_BYTES,
            ));
        }

        let bytes = fs::read(&resolved).map_err(|err| {
            anyhow!(
                "workflow YAML at {} agent '{}' system_prompt_file '{}' could not be read: {}",
                source_label(source_path),
                agent_id,
                resolved.display(),
                err,
            )
        })?;

        let contents = String::from_utf8(bytes).map_err(|err| {
            anyhow!(
                "workflow YAML at {} agent '{}' system_prompt_file '{}' is not valid UTF-8: {}",
                source_label(source_path),
                agent_id,
                resolved.display(),
                err,
            )
        })?;

        profile.system_prompt = contents;
        profile.system_prompt_file = None;
    }
    Ok(())
}

pub fn parse_yaml_workflow_config_with_base(yaml_str: &str, base: &WorkflowConfig) -> Result<WorkflowConfig> {
    parse_yaml_workflow_config_with_base_and_source(yaml_str, base, None)
}

pub fn parse_yaml_workflow_config_with_base_and_source(
    yaml_str: &str,
    base: &WorkflowConfig,
    source_path: Option<&Path>,
) -> Result<WorkflowConfig> {
    parse_yaml_workflow_config_internal(yaml_str, base, source_path, None)
}

pub(crate) fn parse_yaml_workflow_config_confined_to_pack(
    yaml_str: &str,
    base: &WorkflowConfig,
    source_path: &Path,
    pack_root: &Path,
) -> Result<WorkflowConfig> {
    parse_yaml_workflow_config_internal(yaml_str, base, Some(source_path), Some(pack_root))
}

fn parse_yaml_workflow_config_internal(
    yaml_str: &str,
    base: &WorkflowConfig,
    source_path: Option<&Path>,
    pack_root: Option<&Path>,
) -> Result<WorkflowConfig> {
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
    let mut phase_catalog = yaml_file.phase_catalog.unwrap_or_default();
    for (id, ui_def) in auto_phase_catalog {
        phase_catalog.entry(id).or_insert(ui_def);
    }

    // Resolve agent model references against the top-level models registry.
    let mut agent_profiles = yaml_file.agents;
    resolve_agent_system_prompt_files_internal(&mut agent_profiles, yaml_str, source_path, pack_root)?;
    if !yaml_file.models.is_empty() {
        for profile in agent_profiles.values_mut() {
            resolve_agent_model_references(profile, &yaml_file.models);
        }
    }

    let overlay = WorkflowConfig {
        schema: WORKFLOW_CONFIG_SCHEMA_ID.to_string(),
        version: WORKFLOW_CONFIG_VERSION,
        default_workflow_ref,
        phase_catalog,
        workflows,
        checkpoint_retention: WorkflowCheckpointRetentionConfig::default(),
        phase_definitions,
        agent_profiles,
        agent_channels: yaml_file.agent_channels,
        tools_allowlist: yaml_file.tools_allowlist,
        mcp_servers: yaml_file.mcp_servers,
        phase_mcp_bindings: yaml_file.phase_mcp_bindings,
        tools: yaml_file.tools,
        integrations: yaml_file.integrations,
        schedules: yaml_file.schedules,
        triggers: yaml_file.triggers,
        daemon: yaml_file.daemon,
        secrets: yaml_file.secrets,
    };

    Ok(merge_yaml_into_config(base.clone(), overlay))
}

pub fn parse_yaml_workflow_config(yaml_str: &str) -> Result<WorkflowConfig> {
    let base = builtin_workflow_config();
    let mut config = parse_yaml_workflow_config_with_base(yaml_str, &base)?;
    if config.default_workflow_ref.trim().is_empty() {
        config.default_workflow_ref = base.default_workflow_ref;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime_config::AgentProfile;

    fn make_test_registry() -> BTreeMap<String, super::super::yaml_types::ModelRegistryEntry> {
        let mut registry = BTreeMap::new();
        registry.insert(
            "claude-opus".to_string(),
            super::super::yaml_types::ModelRegistryEntry {
                model: "claude-sonnet-4-20250514".to_string(),
                tool: Some("claude".to_string()),
            },
        );
        registry.insert(
            "gpt4o".to_string(),
            super::super::yaml_types::ModelRegistryEntry {
                model: "gpt-4o".to_string(),
                tool: Some("oai-runner".to_string()),
            },
        );
        registry.insert(
            "o4-mini".to_string(),
            super::super::yaml_types::ModelRegistryEntry { model: "o4-mini".to_string(), tool: None },
        );
        registry
    }

    fn make_empty_profile() -> AgentProfile {
        AgentProfile {
            name: None,
            description: "test".to_string(),
            system_prompt: "test prompt".to_string(),
            system_prompt_file: None,
            role: None,
            persona: None,
            memory: Default::default(),
            communication: Default::default(),
            mcp_servers: vec![],
            tool_policy: Default::default(),
            skills: vec![],
            capabilities: BTreeMap::new(),
            mcp_server_configs: None,
            structured_capabilities: None,
            project_overrides: None,
            models: vec![],
            tool: None,
            tool_profile: None,
            model: None,
            fallback_models: vec![],
            fallback_tools: vec![],
            reasoning_effort: None,
            web_search: None,
            network_access: None,
            timeout_secs: None,
            max_attempts: None,
            extra_args: vec![],
            codex_config_overrides: vec![],
            max_continuations: None,
        }
    }

    #[test]
    fn model_registry_resolves_primary_and_fallbacks() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.models = vec!["claude-opus".to_string(), "gpt4o".to_string()];

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(profile.tool.as_deref(), Some("claude"));
        assert_eq!(profile.fallback_models, vec!["gpt-4o"]);
        assert_eq!(profile.fallback_tools, vec!["oai-runner"]);
        assert!(profile.models.is_empty(), "name list should be cleared after expansion");
    }

    #[test]
    fn model_registry_resolves_single_entry_as_primary_only() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.models = vec!["o4-mini".to_string()];

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("o4-mini"));
        assert!(profile.tool.is_none(), "no explicit tool in registry → no override");
        assert!(profile.fallback_models.is_empty());
        assert!(profile.fallback_tools.is_empty());
    }

    #[test]
    fn model_registry_non_registry_name_treated_as_literal_model_id() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.models = vec!["claude-opus".to_string(), "deepseek-v3".to_string()];

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(profile.fallback_models, vec!["deepseek-v3"]);
        // deepseek-v3 isn't in registry, so no explicit fallback_tool
        assert!(profile.fallback_tools.is_empty());
    }

    #[test]
    fn model_registry_empty_list_leaves_profile_unchanged() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.model = Some("existing-model".to_string());

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("existing-model"));
        assert!(profile.fallback_models.is_empty());
    }

    #[test]
    fn model_registry_preserves_existing_model_when_models_empty() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.model = Some("hardcoded-model".to_string());
        profile.fallback_models = vec!["hardcoded-fallback".to_string()];

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("hardcoded-model"));
        assert_eq!(profile.fallback_models, vec!["hardcoded-fallback"]);
    }

    #[test]
    fn model_registry_skips_empty_name_entries() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.models = vec!["".to_string(), "claude-opus".to_string(), "  ".to_string()];

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert!(profile.fallback_models.is_empty());
    }

    #[test]
    fn model_registry_tool_override_takes_precedence_over_profile_tool() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.models = vec!["claude-opus".to_string()];
        profile.tool = Some("original-tool".to_string());

        resolve_agent_model_references(&mut profile, &registry);

        // Registry tool should override profile tool for primary model
        assert_eq!(profile.tool.as_deref(), Some("claude"));
    }

    #[test]
    fn yaml_models_section_compiles_into_agent_profiles() {
        let yaml = r#"
models:
  claude-opus:
    model: claude-sonnet-4-20250514
    tool: claude
  gpt4o:
    model: gpt-4o
    tool: oai-runner

agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."
    models:
      - claude-opus
      - gpt4o

phases:
  impl:
    mode: agent
    agent: swe
    directive: "Implement."
"#;
        let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
        let swe = config.agent_profiles.get("swe").expect("swe agent should exist");
        assert_eq!(swe.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(swe.tool.as_deref(), Some("claude"));
        assert_eq!(swe.fallback_models, vec!["gpt-4o"]);
        assert_eq!(swe.fallback_tools, vec!["oai-runner"]);
    }

    #[test]
    fn yaml_agent_persona_memory_and_channels_parse() {
        let yaml = r#"
agents:
  architect:
    name: Mira
    system_prompt: Keep designs explicit.
    persona:
      style: direct
      traits: [skeptical, concise]
      instructions: Prefer small interfaces.
    memory:
      enabled: true
      scope: project
      max_context_chars: 1200
      write_policy: explicit
    communication:
      enabled: true
      channels: [engineering]
      can_message: [implementer]
  implementer:
    system_prompt: Build the change.
agent_channels:
  engineering:
    participants: [architect, implementer]
    max_context_chars: 2000
phases:
  design:
    mode: agent
    agent: architect
workflows:
- id: test
  phases: [design]
"#;
        let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
        let architect = config.agent_profiles.get("architect").expect("architect agent");
        assert_eq!(architect.name.as_deref(), Some("Mira"));
        assert!(architect.memory.enabled);
        assert!(architect.communication.enabled);
        assert!(config.agent_channels.contains_key("engineering"));
    }

    #[test]
    fn yaml_fallback_tools_field_parses_in_agent_profile() {
        let yaml = r#"
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."
    model: claude-sonnet-4-20250514
    fallback_models:
      - gpt-4o
      - o4-mini
    fallback_tools:
      - oai-runner

phases:
  impl:
    mode: agent
    agent: swe
    directive: "Implement."
"#;
        let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
        let swe = config.agent_profiles.get("swe").expect("swe agent should exist");
        assert_eq!(swe.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(swe.fallback_models, vec!["gpt-4o", "o4-mini"]);
        assert_eq!(swe.fallback_tools, vec!["oai-runner"]);
    }

    #[test]
    fn yaml_fallback_tools_in_phase_runtime() {
        let yaml = r#"
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."

phases:
  impl:
    mode: agent
    agent: swe
    directive: "Implement."
    runtime:
      model: claude-sonnet-4-20250514
      fallback_models:
        - gpt-4o
        - o4-mini
      fallback_tools:
        - oai-runner
"#;
        let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
        let impl_phase = config.phase_definitions.get("impl").expect("impl phase should exist");
        let runtime = impl_phase.runtime.as_ref().expect("runtime should exist");
        assert_eq!(runtime.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(runtime.fallback_models, vec!["gpt-4o", "o4-mini"]);
        assert_eq!(runtime.fallback_tools, vec!["oai-runner"]);
    }

    #[test]
    fn yaml_models_and_fallback_tools_combined() {
        let yaml = r#"
models:
  primary:
    model: claude-sonnet-4-20250514
    tool: claude
  secondary:
    model: gpt-4o
    tool: oai-runner
  tertiary:
    model: o4-mini

agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."
    models:
      - primary
      - secondary
      - tertiary

phases:
  impl:
    mode: agent
    agent: swe
    directive: "Implement."
"#;
        let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
        let swe = config.agent_profiles.get("swe").expect("swe agent should exist");
        assert_eq!(swe.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(swe.tool.as_deref(), Some("claude"));
        assert_eq!(swe.fallback_models, vec!["gpt-4o", "o4-mini"]);
        // Only secondary has explicit tool; tertiary has none → only "oai-runner" in fallback_tools
        assert_eq!(swe.fallback_tools, vec!["oai-runner"]);
    }

    #[test]
    fn yaml_without_models_section_parses_without_error() {
        let yaml = r#"
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."

phases:
  impl:
    mode: agent
    agent: swe
    directive: "Implement."
"#;
        let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
        let swe = config.agent_profiles.get("swe").expect("swe agent should exist");
        assert!(swe.model.is_none());
        assert!(swe.fallback_models.is_empty());
    }

    #[test]
    fn system_prompt_file_inlines_relative_path_into_compiled_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let prompts_dir = temp.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("create prompts dir");
        let prompt_body = "You are the macro analyst.\nKeep evidence cited.\n";
        std::fs::write(prompts_dir.join("macro.md"), prompt_body).expect("write prompt");

        let yaml_path = temp.path().join("workflows.yaml");
        let yaml = r#"
agents:
  analyst:
    description: "Macro analyst"
    system_prompt_file: prompts/macro.md

phases:
  research:
    mode: agent
    agent: analyst
    directive: "Research."
"#;
        std::fs::write(&yaml_path, yaml).expect("write yaml");

        let base = builtin_workflow_config();
        let config = parse_yaml_workflow_config_with_base_and_source(yaml, &base, Some(&yaml_path))
            .expect("parse yaml with source path");
        let analyst = config.agent_profiles.get("analyst").expect("analyst agent");
        assert_eq!(analyst.system_prompt, prompt_body);
        assert!(analyst.system_prompt_file.is_none(), "system_prompt_file should be consumed");
    }

    #[test]
    fn system_prompt_file_resolves_absolute_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let abs_prompt = temp.path().join("absolute-prompt.md");
        std::fs::write(&abs_prompt, "absolute prompt body").expect("write prompt");

        let yaml = format!(
            r#"
agents:
  analyst:
    description: "Analyst"
    system_prompt_file: "{}"

phases:
  research:
    mode: agent
    agent: analyst
    directive: "Research."
"#,
            abs_prompt.display()
        );

        let yaml_path = temp.path().join("nested").join("workflows.yaml");
        let base = builtin_workflow_config();
        let config = parse_yaml_workflow_config_with_base_and_source(&yaml, &base, Some(&yaml_path))
            .expect("parse yaml with absolute path");
        let analyst = config.agent_profiles.get("analyst").expect("analyst agent");
        assert_eq!(analyst.system_prompt, "absolute prompt body");
    }

    #[test]
    fn system_prompt_file_and_inline_system_prompt_are_mutually_exclusive() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("p.md"), "from file").expect("write prompt");

        let yaml = r#"
agents:
  analyst:
    description: "Analyst"
    system_prompt: "inline prompt"
    system_prompt_file: p.md

phases:
  research:
    mode: agent
    agent: analyst
    directive: "Research."
"#;
        let yaml_path = temp.path().join("workflows.yaml");
        std::fs::write(&yaml_path, yaml).expect("write yaml");

        let base = builtin_workflow_config();
        let err = parse_yaml_workflow_config_with_base_and_source(yaml, &base, Some(&yaml_path))
            .expect_err("mutual exclusion error");
        let msg = format!("{:#}", err);
        assert!(msg.contains("analyst"), "missing agent id: {msg}");
        assert!(msg.contains("system_prompt"), "missing field name: {msg}");
        assert!(msg.contains(&yaml_path.display().to_string()), "missing source path: {msg}");
        assert!(msg.contains("line"), "missing line number: {msg}");
    }

    #[test]
    fn system_prompt_file_missing_file_errors_with_resolved_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let yaml = r#"
agents:
  analyst:
    description: "Analyst"
    system_prompt_file: prompts/missing.md

phases:
  research:
    mode: agent
    agent: analyst
    directive: "Research."
"#;
        let yaml_path = temp.path().join("workflows.yaml");
        let base = builtin_workflow_config();
        let err = parse_yaml_workflow_config_with_base_and_source(yaml, &base, Some(&yaml_path))
            .expect_err("missing file should error");
        let msg = format!("{:#}", err);
        let resolved = temp.path().join("prompts").join("missing.md");
        assert!(msg.contains(&resolved.display().to_string()), "missing resolved path: {msg}");
        assert!(msg.contains("analyst"), "missing agent id: {msg}");
    }

    #[test]
    fn system_prompt_file_oversize_errors_with_size_cap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let prompt_path = temp.path().join("big.md");
        let bytes = vec![b'a'; (SYSTEM_PROMPT_FILE_MAX_BYTES + 1) as usize];
        std::fs::write(&prompt_path, &bytes).expect("write big prompt");

        let yaml = r#"
agents:
  analyst:
    description: "Analyst"
    system_prompt_file: big.md

phases:
  research:
    mode: agent
    agent: analyst
    directive: "Research."
"#;
        let yaml_path = temp.path().join("workflows.yaml");
        let base = builtin_workflow_config();
        let err = parse_yaml_workflow_config_with_base_and_source(yaml, &base, Some(&yaml_path))
            .expect_err("oversize should error");
        let msg = format!("{:#}", err);
        assert!(msg.contains("1 MiB") || msg.contains("maximum"), "missing size cap: {msg}");
        assert!(msg.contains(&prompt_path.display().to_string()), "missing resolved path: {msg}");
    }

    #[test]
    fn system_prompt_file_non_utf8_errors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let prompt_path = temp.path().join("binary.md");
        std::fs::write(&prompt_path, &[0xffu8, 0xfe, 0xfd][..]).expect("write binary");

        let yaml = r#"
agents:
  analyst:
    description: "Analyst"
    system_prompt_file: binary.md

phases:
  research:
    mode: agent
    agent: analyst
    directive: "Research."
"#;
        let yaml_path = temp.path().join("workflows.yaml");
        let base = builtin_workflow_config();
        let err = parse_yaml_workflow_config_with_base_and_source(yaml, &base, Some(&yaml_path))
            .expect_err("non-utf8 should error");
        let msg = format!("{:#}", err);
        assert!(msg.contains("UTF-8") || msg.contains("utf-8"), "missing utf-8 marker: {msg}");
    }

    #[test]
    fn system_prompt_file_preserves_whitespace_verbatim() {
        let temp = tempfile::tempdir().expect("tempdir");
        let prompt_path = temp.path().join("ws.md");
        let body = "\n\n  leading and trailing whitespace  \n\n";
        std::fs::write(&prompt_path, body).expect("write prompt");

        let yaml = r#"
agents:
  analyst:
    description: "Analyst"
    system_prompt_file: ws.md

phases:
  research:
    mode: agent
    agent: analyst
    directive: "Research."
"#;
        let yaml_path = temp.path().join("workflows.yaml");
        let base = builtin_workflow_config();
        let config = parse_yaml_workflow_config_with_base_and_source(yaml, &base, Some(&yaml_path))
            .expect("parse should succeed");
        assert_eq!(config.agent_profiles.get("analyst").unwrap().system_prompt, body);
    }

    #[test]
    fn agent_profile_serde_roundtrip_with_system_prompt_file() {
        let mut profile = make_empty_profile();
        profile.system_prompt = String::new();
        profile.system_prompt_file = Some("prompts/agent.md".to_string());

        let json = serde_json::to_string(&profile).expect("serialize");
        assert!(json.contains("system_prompt_file"), "field should be present: {json}");
        let back: AgentProfile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.system_prompt_file.as_deref(), Some("prompts/agent.md"));
    }

    #[test]
    fn agent_profile_serde_omits_system_prompt_file_when_none() {
        let profile = make_empty_profile();
        let json = serde_json::to_string(&profile).expect("serialize");
        assert!(!json.contains("system_prompt_file"), "field should be skipped when None: {json}");
    }

    #[test]
    fn existing_yaml_without_system_prompt_file_parses_identically() {
        let yaml = r#"
agents:
  swe:
    description: "Software engineer"
    system_prompt: "You are a SWE."

phases:
  impl:
    mode: agent
    agent: swe
    directive: "Implement."
"#;
        let with_source = parse_yaml_workflow_config(yaml).expect("parse without source");
        let temp = tempfile::tempdir().expect("tempdir");
        let yaml_path = temp.path().join("workflows.yaml");
        let base = builtin_workflow_config();
        let with_path =
            parse_yaml_workflow_config_with_base_and_source(yaml, &base, Some(&yaml_path)).expect("parse with source");
        assert_eq!(
            with_source.agent_profiles.get("swe").unwrap().system_prompt,
            with_path.agent_profiles.get("swe").unwrap().system_prompt
        );
    }
}
