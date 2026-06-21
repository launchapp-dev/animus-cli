use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::agent_types::PhaseExecutionDefinition;
use crate::agent_types::{
    AgentProfileOverlay, CommandCwdMode, EvalCheck, EvalsConfig, PhaseCommandDefinition, PhaseExecutionMode,
    PhaseManualDefinition,
};

use crate::builtins::builtin_workflow_config;
use crate::parse::merge_yaml_into_config;
use crate::workflow_types::*;
use crate::yaml_diagnostic::wrap_serde_yaml_error;
use crate::yaml_types::title_case_phase_id;
use crate::yaml_types::*;

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
    profile: &mut AgentProfileOverlay,
    registry: &BTreeMap<String, crate::yaml_types::ModelRegistryEntry>,
) {
    let models = profile.models.as_deref().unwrap_or_default();
    if models.is_empty() {
        return;
    }

    let mut resolved_models: Vec<String> = Vec::with_capacity(models.len());
    let mut resolved_tools: Vec<Option<String>> = Vec::with_capacity(models.len());

    for name in models {
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

    // Set primary model (and tool) from the first resolved entry
    let primary_model = resolved_models.remove(0);
    profile.tool = match resolved_tools.remove(0) {
        Some(tool) => Some(tool),
        // The `models:` chain is authoritative for routing: when the
        // selected primary has no registry tool, derive it from the model
        // id so a base profile's tool cannot route the new model to the
        // wrong provider.
        None => Some(protocol::tool_for_model_id(&primary_model).to_string()),
    };
    profile.model = Some(primary_model);

    // Remaining resolved entries become fallbacks
    if !resolved_models.is_empty() {
        profile.fallback_models = Some(resolved_models);
        // Build fallback_tools: use explicit tool if provided, else empty (auto-derived at runtime)
        profile.fallback_tools = Some(resolved_tools.into_iter().flatten().collect());
    } else {
        // A single-entry `models:` list is authoritative for the whole
        // chain: explicitly clear inherited fallbacks unless the profile
        // declared its own `fallback_models` / `fallback_tools` alongside.
        if profile.fallback_models.is_none() {
            profile.fallback_models = Some(Vec::new());
        }
        if profile.fallback_tools.is_none() {
            profile.fallback_tools = Some(Vec::new());
        }
    }

    // Clear the name list after expansion to avoid double-expansion
    profile.models = None;
}

pub(super) fn parse_cwd_mode(value: &str) -> Result<CommandCwdMode> {
    match value.to_ascii_lowercase().replace('-', "_").as_str() {
        "project_root" => Ok(CommandCwdMode::ProjectRoot),
        "task_root" => Ok(CommandCwdMode::TaskRoot),
        "path" => Ok(CommandCwdMode::Path),
        other => Err(anyhow!("unknown cwd_mode '{}' (expected project_root, task_root, or path)", other)),
    }
}

// TODO(codex-p2): pack asset resolution does not yet rewrite
// `evals.checks[].command` when it points at a pack-relative asset (e.g.
// `assets/review-check.sh`). The current pack resolver only rewrites
// phase `command.program` and tool executables. When the out-of-tree
// workflow-runner adopts `run_evals`, extend the pack resolver in
// `workflow_config::resolution` to walk eval check commands as well so
// installed pack eval checks spawn from the pack root rather than the
// phase worktree.
fn yaml_evals_to_evals_config(yaml: YamlEvalsConfig) -> EvalsConfig {
    EvalsConfig {
        pass_threshold: yaml.pass_threshold,
        on_fail: yaml.on_fail,
        max_reworks: yaml.max_reworks,
        checks: yaml
            .checks
            .into_iter()
            .map(|c| EvalCheck {
                id: c.id,
                kind: c.kind,
                command: c.command,
                args: c.args,
                working_dir: c.working_dir,
                timeout_secs: c.timeout_secs,
                expected_exit: c.expected_exit,
                agent: c.agent,
                prompt: c.prompt,
            })
            .collect(),
    }
}

fn evals_config_to_yaml(config: EvalsConfig) -> YamlEvalsConfig {
    YamlEvalsConfig {
        pass_threshold: config.pass_threshold,
        on_fail: config.on_fail,
        max_reworks: config.max_reworks,
        checks: config
            .checks
            .into_iter()
            .map(|c| YamlEvalCheck {
                id: c.id,
                kind: c.kind,
                command: c.command,
                args: c.args,
                working_dir: c.working_dir,
                timeout_secs: c.timeout_secs,
                expected_exit: c.expected_exit,
                agent: c.agent,
                prompt: c.prompt,
            })
            .collect(),
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
        evals: yaml.evals.map(yaml_evals_to_evals_config),
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
                    budget: config.budget.clone(),
                },
            );
            YamlPhaseEntry::Rich(map)
        }
    }
}

pub(crate) fn workflow_definition_to_yaml(definition: &WorkflowDefinition) -> YamlWorkflowDefinition {
    YamlWorkflowDefinition {
        id: definition.id.clone(),
        name: Some(definition.name.clone()),
        description: Some(definition.description.clone()),
        phases: definition.phases.iter().map(workflow_phase_entry_to_yaml).collect(),
        variables: definition.variables.clone(),
        worktree: definition.worktree.clone().map(YamlPhaseWorktree::Full),
        budget: definition.budget.clone(),
    }
}

pub(crate) fn phase_execution_definition_to_yaml(definition: &PhaseExecutionDefinition) -> YamlPhaseDefinition {
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
        evals: definition.evals.clone().map(evals_config_to_yaml),
    }
}

pub(crate) fn workflow_config_to_yaml_file(config: &WorkflowConfig) -> YamlWorkflowFile {
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
                budget: config.budget,
            }))
        }
    }
}

pub(super) fn yaml_workflow_to_workflow_definition(yaml: YamlWorkflowDefinition) -> Result<WorkflowDefinition> {
    let phases = yaml.phases.into_iter().map(yaml_phase_entry_to_workflow_phase_entry).collect::<Result<Vec<_>>>()?;
    let worktree = yaml.worktree.map(WorktreeConfig::from_yaml).transpose()?;
    Ok(WorkflowDefinition {
        id: yaml.id.clone(),
        name: yaml.name.unwrap_or_else(|| yaml.id.clone()),
        description: yaml.description.unwrap_or_default(),
        phases,
        variables: yaml.variables,
        worktree,
        budget: yaml.budget,
    })
}

fn source_label(source_path: Option<&Path>) -> String {
    source_path.map(|p| p.display().to_string()).unwrap_or_else(|| "<in-memory>".to_string())
}

fn reject_removed_post_success_merge(yaml_str: &str, source_path: Option<&Path>) -> Result<()> {
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(yaml_str) else {
        return Ok(());
    };

    let mut post_success_merge_found = false;
    if let Some(workflows) = doc.get("workflows").and_then(serde_yaml::Value::as_sequence) {
        for workflow in workflows {
            if workflow.get("post_success").and_then(|ps| ps.get("merge")).is_some() {
                post_success_merge_found = true;
                break;
            }
        }
    }

    if post_success_merge_found {
        let location = yaml_str
            .lines()
            .enumerate()
            .find(|(_, line)| line.trim_start().starts_with("merge:"))
            .or_else(|| yaml_str.lines().enumerate().find(|(_, line)| line.trim_start().starts_with("post_success:")))
            .map(|(idx, _)| format!("{}:{}", source_label(source_path), idx + 1))
            .unwrap_or_else(|| source_label(source_path));

        return Err(anyhow!(
            "`post_success.merge` was removed in v0.5.x ({location}): Animus no longer performs \
             git operations as runner automation. Express commit/push/PR/merge as command phases \
             (a phase with a `command:` running `git`/`gh`). See docs/reference/workflow-yaml.md."
        ));
    }

    if doc
        .get("integrations")
        .and_then(|integrations| integrations.get("git"))
        .and_then(|git| git.get("auto_merge"))
        .is_some()
    {
        let location = yaml_str
            .lines()
            .enumerate()
            .find(|(_, line)| line.trim_start().starts_with("auto_merge:"))
            .map(|(idx, _)| format!("{}:{}", source_label(source_path), idx + 1))
            .unwrap_or_else(|| source_label(source_path));

        return Err(anyhow!(
            "`integrations.git.auto_merge` was removed in v0.5.x ({location}): Animus no longer \
             merges to main autonomously. Express commit/push/PR/merge as command phases (a phase \
             with a `command:` running `git`/`gh`). See docs/reference/workflow-yaml.md."
        ));
    }

    Ok(())
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

pub fn resolve_agent_system_prompt_files_confined_to_pack(
    agent_profiles: &mut BTreeMap<String, AgentProfileOverlay>,
    yaml_str: &str,
    source_path: &Path,
    pack_root: &Path,
) -> Result<()> {
    resolve_agent_system_prompt_files_internal(agent_profiles, yaml_str, Some(source_path), Some(pack_root))
}

fn resolve_agent_system_prompt_files_internal(
    agent_profiles: &mut BTreeMap<String, AgentProfileOverlay>,
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

        let inline_set = profile.system_prompt.as_deref().is_some_and(|prompt| !prompt.trim().is_empty());
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

        profile.system_prompt = Some(contents);
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
    parse_yaml_workflow_config_internal(yaml_str, base, source_path, None, None, &BTreeMap::new())
}

/// Variant used by the YAML compiler after `${VAR}` / `${secret.X}`
/// interpolation. `original` is the pre-interpolation file content; parse
/// diagnostics render their source excerpt from it so resolved secret
/// values never appear in error output. `resolved_secrets` maps each
/// substituted secret name to its resolved value so error text built from
/// the post-interpolation content can be redacted before surfacing.
pub(crate) fn parse_yaml_workflow_config_with_base_source_and_original(
    yaml_str: &str,
    base: &WorkflowConfig,
    source_path: Option<&Path>,
    original: &str,
    resolved_secrets: &BTreeMap<String, String>,
) -> Result<WorkflowConfig> {
    parse_yaml_workflow_config_internal(yaml_str, base, source_path, None, Some(original), resolved_secrets)
}

pub(crate) fn parse_yaml_workflow_config_confined_to_pack(
    yaml_str: &str,
    base: &WorkflowConfig,
    source_path: &Path,
    pack_root: &Path,
    original: &str,
    resolved_secrets: &BTreeMap<String, String>,
) -> Result<WorkflowConfig> {
    parse_yaml_workflow_config_internal(
        yaml_str,
        base,
        Some(source_path),
        Some(pack_root),
        Some(original),
        resolved_secrets,
    )
}

/// Known top-level YAML keys recognized by `YamlWorkflowFile`. Used when
/// `serde_yaml` surfaces an `unknown field` diagnostic at the top level
/// to suggest the closest valid key.
///
/// NOTE: `YamlWorkflowFile` does not currently use `deny_unknown_fields`,
/// so silent typos at the top level (e.g. `phasess:`) are not surfaced
/// today — they are simply ignored. The suggestion table is still wired
/// up because unknown-field errors do bubble up from nested structs that
/// DO deny unknown fields (e.g. `YamlSubWorkflowRef`).
const KNOWN_FIELD_KEYS: &[&str] = &[
    "default_workflow_ref",
    "phase_catalog",
    "workflows",
    "phases",
    "agents",
    "agent_channels",
    "models",
    "tools_allowlist",
    "mcp_servers",
    "phase_mcp_bindings",
    "tools",
    "integrations",
    "schedules",
    "triggers",
    "daemon",
    "secrets",
    "workflow_ref",
    "mode",
    "agent",
    "command",
    "manual",
    "directive",
    "system_prompt",
    "skills",
    "runtime",
    "capabilities",
    "output_contract",
    "output_json_schema",
    "decision_contract",
    "retry",
    "default_tool",
    "idempotency",
    "worktree",
    "evals",
];

/// Inspect the raw serde_yaml error message and upgrade the diagnostic
/// with a more specific code, an expected-shape list, and (when possible)
/// a "did you mean" suggestion derived from Levenshtein distance.
fn enrich_diagnostic(
    mut diag: crate::yaml_diagnostic::YamlDiagnostic,
    yaml_str: &str,
) -> crate::yaml_diagnostic::YamlDiagnostic {
    use crate::yaml_diagnostic::closest_match;
    let msg = diag.message.clone();
    if let Some(field) = parse_unknown_field_name(&msg) {
        diag.code = "yaml.unknown_field".to_string();
        if let Some(suggestion) = closest_match(&field, KNOWN_FIELD_KEYS, 2) {
            diag.suggestion = Some(suggestion.to_string());
            diag.message = format!("unknown field `{}`", field);
        }
        // serde_yaml reports the location of the enclosing sequence/mapping
        // start, not the offending key. For a list entry such as
        // `- id: review`, the caret lands on the previous valid entry
        // (`- build`). Re-anchor the diagnostic on the line that actually
        // declares `<field>:` so the caret points at the broken entry.
        if let Some(start_line) = diag.line {
            if let Some((line, col_start, col_end)) = locate_field_line(yaml_str, &field, start_line) {
                diag.line = Some(line);
                diag.col = Some(col_start);
                diag.excerpt = None;
                diag = diag.with_excerpt_from(yaml_str, line, col_start, col_end);
            }
        }
    } else if msg.contains("invalid `worktree:`") {
        diag.code = "yaml.invalid_worktree".to_string();
        diag.expected = vec![
            "string: \"auto\" | \"required\" | \"skip\"".to_string(),
            "boolean: true (= auto) | false (= skip)".to_string(),
            "map: { mode: <string>, cleanup: <bool>, base_ref: <string> }".to_string(),
        ];
        if let Some(s) = parse_did_you_mean_from_message(&msg) {
            diag.suggestion = Some(s);
        }
        if let Some(start_line) = diag.line {
            if let Some((line, col_start, col_end)) = locate_field_line(yaml_str, "worktree", start_line) {
                diag.line = Some(line);
                diag.col = Some(col_start);
                diag.excerpt = None;
                diag = diag.with_excerpt_from(yaml_str, line, col_start, col_end);
            }
        }
    } else if msg.contains("invalid phase entry") || msg.contains("rich phase entry") {
        diag.code = "yaml.invalid_phase_entry".to_string();
        diag.expected = vec![
            "string phase id (e.g. `- impl`)".to_string(),
            "sub-workflow ref: { workflow_ref: <name> }".to_string(),
            "rich config: { <phase_id>: { max_rework_attempts: N, ... } }".to_string(),
        ];
    } else if msg.contains("missing field") {
        diag.code = "yaml.missing_field".to_string();
    }
    // If we have an excerpt with only a single-char underline at column N
    // but the focal line contains a YAML key:value where we can widen the
    // span to the full value, do so for better UX.
    if let (Some(line), Some(col)) = (diag.line, diag.col) {
        diag = widen_excerpt_to_value(diag, yaml_str, line, col);
    }
    diag
}

/// Extract a quoted field name from serde_yaml's "unknown field `xxx`" /
/// "unknown variant `xxx`" messages.
/// Search `yaml_str` starting from `start_line` (1-based) for a line whose
/// trimmed prefix is `<field>:`. Returns the 1-based line plus 1-based
/// column start / column end (exclusive) covering `<field>: <value>`.
fn locate_field_line(yaml_str: &str, field: &str, start_line: usize) -> Option<(usize, usize, usize)> {
    let key = format!("{}:", field);
    let lines: Vec<&str> = yaml_str.lines().collect();
    let start_idx = start_line.saturating_sub(1).min(lines.len());
    let matches = |line: &str| -> Option<usize> {
        // Match `<field>:` either as the bare key or as the first key of a
        // YAML sequence entry (`- <field>:`). The caret column is the start
        // of the key itself (after any `- ` marker) so it underlines the
        // offending field, not the list bullet.
        let trimmed = line.trim_start();
        let leading_ws = line.len() - trimmed.len();
        if trimmed.starts_with(&key) {
            return Some(leading_ws);
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            if rest.trim_start().starts_with(&key) {
                let after_marker = rest.len() - rest.trim_start().len();
                return Some(leading_ws + 2 + after_marker);
            }
        }
        None
    };
    for (offset, line) in lines.iter().enumerate().skip(start_idx) {
        if let Some(col) = matches(line) {
            return Some((offset + 1, col + 1, line.chars().count() + 1));
        }
    }
    for offset in (0..start_idx).rev() {
        if let Some(col) = matches(lines[offset]) {
            return Some((offset + 1, col + 1, lines[offset].chars().count() + 1));
        }
    }
    None
}

fn parse_unknown_field_name(msg: &str) -> Option<String> {
    let needle = msg.find("unknown field `").map(|i| i + "unknown field `".len())?;
    let rest = &msg[needle..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

fn parse_did_you_mean_from_message(msg: &str) -> Option<String> {
    let i = msg.find("did you mean `").map(|i| i + "did you mean `".len())?;
    let rest = &msg[i..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

fn widen_excerpt_to_value(
    mut diag: crate::yaml_diagnostic::YamlDiagnostic,
    yaml_str: &str,
    line: usize,
    col: usize,
) -> crate::yaml_diagnostic::YamlDiagnostic {
    let Some(focal_line) = yaml_str.lines().nth(line.saturating_sub(1)) else {
        return diag;
    };
    let line_len = focal_line.chars().count();
    if col >= line_len {
        return diag;
    }
    let end = line_len + 1;
    diag.excerpt = None;
    diag.with_excerpt_from(yaml_str, line, col, end)
}

/// Replace a diagnostic's excerpt with one rendered from the ORIGINAL
/// pre-interpolation content. The line/column were computed on the
/// interpolated text, so they can drift when substituted values contain
/// newlines, but excerpt lines always show the raw `${VAR}` / `${secret.X}`
/// references instead of resolved values.
fn rebuild_excerpt_from_original(
    mut diag: crate::yaml_diagnostic::YamlDiagnostic,
    original: &str,
) -> crate::yaml_diagnostic::YamlDiagnostic {
    diag.excerpt = None;
    let Some(line) = diag.line else {
        return diag;
    };
    let col = diag.col.unwrap_or(1);
    diag.with_excerpt_from(original, line, col, 0)
}

/// Replace each resolved secret VALUE occurring in `message` with
/// `[redacted:<name>]`. `resolved_secrets` maps resolved value → redaction
/// label (secret name or keychain env-var name), so colliding labels can
/// never drop a value from the map. Values shorter than 4 characters are
/// skipped: they are too likely to occur incidentally in unrelated
/// diagnostic text (line numbers, short words), which would mangle the
/// message without meaningfully protecting the secret.
fn redact_resolved_secret_values(message: &str, resolved_secrets: &BTreeMap<String, String>) -> String {
    let mut redacted = message.to_string();
    // Longest value first so an overlapping shorter secret cannot split a
    // longer one and leave its tail in the diagnostic.
    let mut entries: Vec<(&String, &String)> = resolved_secrets.iter().filter(|(value, _)| value.len() >= 4).collect();
    entries.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
    for (value, name) in entries {
        let marker = format!("[redacted:{}]", name);
        redacted = redacted.replace(value.as_str(), &marker);
        // serde quotes offending scalars with Rust `{:?}` escaping
        // (backslashes, quotes, control characters), so redact that
        // rendering of the value too.
        let escaped = format!("{:?}", value);
        let escaped_inner = &escaped[1..escaped.len() - 1];
        if escaped_inner != value {
            redacted = redacted.replace(escaped_inner, &marker);
        }
    }
    redacted
}

fn parse_yaml_workflow_config_internal(
    yaml_str: &str,
    base: &WorkflowConfig,
    source_path: Option<&Path>,
    pack_root: Option<&Path>,
    original: Option<&str>,
    resolved_secrets: &BTreeMap<String, String>,
) -> Result<WorkflowConfig> {
    parse_yaml_workflow_config_unredacted(yaml_str, base, source_path, pack_root, original).map_err(|err| {
        let rendered = format!("{:#}", err);
        let redacted = redact_resolved_secret_values(&rendered, resolved_secrets);
        if redacted == rendered {
            err
        } else {
            anyhow!("{}", redacted)
        }
    })
}

fn parse_yaml_workflow_config_unredacted(
    yaml_str: &str,
    base: &WorkflowConfig,
    source_path: Option<&Path>,
    pack_root: Option<&Path>,
    original: Option<&str>,
) -> Result<WorkflowConfig> {
    reject_removed_post_success_merge(yaml_str, source_path)?;

    let yaml_file: YamlWorkflowFile = match serde_yaml::from_str(yaml_str) {
        Ok(file) => file,
        Err(err) => {
            let mut diag = enrich_diagnostic(wrap_serde_yaml_error(&err, yaml_str, source_path), yaml_str);
            if let Some(original) = original.filter(|original| *original != yaml_str) {
                diag = rebuild_excerpt_from_original(diag, original);
            }
            // Preserve the typed `YamlDiagnostic` in the error chain (its
            // Display still renders the full rustc-style caret via `{:#}`),
            // so downstream consumers — notably `workflow config validate` —
            // can downcast and surface message + line/col + code structurally
            // instead of only seeing a flattened string.
            return Err(anyhow::Error::new(diag));
        }
    };

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
    use crate::agent_types::AgentProfileOverlay;

    fn make_test_registry() -> BTreeMap<String, crate::yaml_types::ModelRegistryEntry> {
        let mut registry = BTreeMap::new();
        registry.insert(
            "claude-opus".to_string(),
            crate::yaml_types::ModelRegistryEntry {
                model: "claude-sonnet-4-20250514".to_string(),
                tool: Some("claude".to_string()),
            },
        );
        registry.insert(
            "gpt4o".to_string(),
            crate::yaml_types::ModelRegistryEntry { model: "gpt-4o".to_string(), tool: Some("oai-runner".to_string()) },
        );
        registry.insert(
            "o4-mini".to_string(),
            crate::yaml_types::ModelRegistryEntry { model: "o4-mini".to_string(), tool: None },
        );
        registry
    }

    fn make_empty_profile() -> AgentProfileOverlay {
        AgentProfileOverlay {
            description: Some("test".to_string()),
            system_prompt: Some("test prompt".to_string()),
            ..AgentProfileOverlay::default()
        }
    }

    #[test]
    fn model_registry_resolves_primary_and_fallbacks() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.models = Some(vec!["claude-opus".to_string(), "gpt4o".to_string()]);

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(profile.tool.as_deref(), Some("claude"));
        assert_eq!(profile.fallback_models.clone().unwrap_or_default(), vec!["gpt-4o"]);
        assert_eq!(profile.fallback_tools.clone().unwrap_or_default(), vec!["oai-runner"]);
        assert!(profile.models.is_none(), "name list should be cleared after expansion");
    }

    #[test]
    fn model_registry_resolves_single_entry_as_primary_only() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.models = Some(vec!["o4-mini".to_string()]);

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("o4-mini"));
        assert_eq!(
            profile.tool.as_deref(),
            Some(protocol::tool_for_model_id("o4-mini")),
            "no explicit tool in registry → derived from the model id"
        );
        assert!(profile.fallback_models.as_deref().unwrap_or_default().is_empty());
        assert!(profile.fallback_tools.as_deref().unwrap_or_default().is_empty());
    }

    #[test]
    fn model_registry_non_registry_name_treated_as_literal_model_id() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.models = Some(vec!["claude-opus".to_string(), "deepseek-v3".to_string()]);

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(profile.fallback_models.clone().unwrap_or_default(), vec!["deepseek-v3"]);
        // deepseek-v3 isn't in registry, so no explicit fallback_tool
        assert!(profile.fallback_tools.as_deref().unwrap_or_default().is_empty());
    }

    #[test]
    fn model_registry_empty_list_leaves_profile_unchanged() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.model = Some("existing-model".to_string());

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("existing-model"));
        assert!(profile.fallback_models.as_deref().unwrap_or_default().is_empty());
    }

    #[test]
    fn model_registry_preserves_existing_model_when_models_empty() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.model = Some("hardcoded-model".to_string());
        profile.fallback_models = Some(vec!["hardcoded-fallback".to_string()]);

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("hardcoded-model"));
        assert_eq!(profile.fallback_models.clone().unwrap_or_default(), vec!["hardcoded-fallback"]);
    }

    #[test]
    fn model_registry_skips_empty_name_entries() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.models = Some(vec!["".to_string(), "claude-opus".to_string(), "  ".to_string()]);

        resolve_agent_model_references(&mut profile, &registry);

        assert_eq!(profile.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert!(profile.fallback_models.as_deref().unwrap_or_default().is_empty());
    }

    #[test]
    fn model_registry_tool_override_takes_precedence_over_profile_tool() {
        let registry = make_test_registry();
        let mut profile = make_empty_profile();
        profile.models = Some(vec!["claude-opus".to_string()]);
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
        assert_eq!(swe.fallback_models.clone().unwrap_or_default(), vec!["gpt-4o"]);
        assert_eq!(swe.fallback_tools.clone().unwrap_or_default(), vec!["oai-runner"]);
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
        assert!(architect.memory.clone().unwrap_or_default().enabled);
        assert!(architect.communication.clone().unwrap_or_default().enabled);
        assert!(config.agent_channels.contains_key("engineering"));
    }

    #[test]
    fn yaml_agent_approval_policy_parses() {
        let yaml = r#"
agents:
  swe:
    system_prompt: Build the change.
    approval_policy:
      auto_allow: ["git.*", "cargo *"]
      auto_deny: ["*force*"]
      default: ask
phases:
  impl:
    mode: agent
    agent: swe
"#;
        let config = parse_yaml_workflow_config(yaml).expect("parse yaml");
        let swe = config.agent_profiles.get("swe").expect("swe agent");
        let policy = swe.approval_policy.clone().expect("approval policy");
        assert_eq!(policy.auto_allow, vec!["git.*".to_string(), "cargo *".to_string()]);
        assert_eq!(policy.auto_deny, vec!["*force*".to_string()]);
        assert_eq!(policy.default, crate::agent_types::ApprovalPolicyDefault::Ask);
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
        assert_eq!(swe.fallback_models.clone().unwrap_or_default(), vec!["gpt-4o", "o4-mini"]);
        assert_eq!(swe.fallback_tools.clone().unwrap_or_default(), vec!["oai-runner"]);
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
        assert_eq!(swe.fallback_models.clone().unwrap_or_default(), vec!["gpt-4o", "o4-mini"]);
        // Only secondary has explicit tool; tertiary has none → only "oai-runner" in fallback_tools
        assert_eq!(swe.fallback_tools.clone().unwrap_or_default(), vec!["oai-runner"]);
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
        assert!(swe.fallback_models.as_deref().unwrap_or_default().is_empty());
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
        assert_eq!(analyst.system_prompt.as_deref(), Some(prompt_body));
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
        assert_eq!(analyst.system_prompt.as_deref(), Some("absolute prompt body"));
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
        assert_eq!(config.agent_profiles.get("analyst").unwrap().system_prompt.as_deref(), Some(body));
    }

    #[test]
    fn agent_profile_serde_roundtrip_with_system_prompt_file() {
        let mut profile = make_empty_profile();
        profile.system_prompt = None;
        profile.system_prompt_file = Some("prompts/agent.md".to_string());

        let json = serde_json::to_string(&profile).expect("serialize");
        assert!(json.contains("system_prompt_file"), "field should be present: {json}");
        let back: AgentProfileOverlay = serde_json::from_str(&json).expect("deserialize");
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
