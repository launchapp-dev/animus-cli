use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::AgentToolPolicy;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillCategory {
    Implementation,
    Testing,
    Review,
    Research,
    Documentation,
    Operations,
    Planning,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SkillPrompt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directives: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SkillActivation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

impl SkillActivation {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.models.is_empty()
    }

    fn matches(&self, tool_id: &str, model_id: Option<&str>) -> bool {
        let tool_matches =
            self.tools.is_empty() || self.tools.iter().any(|candidate| candidate.eq_ignore_ascii_case(tool_id.trim()));
        if !tool_matches {
            return false;
        }

        if self.models.is_empty() {
            return true;
        }

        let Some(model_id) = model_id.map(str::trim).filter(|value| !value.is_empty()) else {
            return false;
        };
        self.models.iter().any(|candidate| candidate.eq_ignore_ascii_case(model_id))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SkillModelPreference {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SkillToolAdapter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy: Option<AgentToolPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codex_config_overrides: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_override: Option<SkillPrompt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<SkillCategory>,

    #[serde(default, skip_serializing_if = "SkillActivation::is_empty")]
    pub activation: SkillActivation,

    #[serde(default)]
    pub prompt: SkillPrompt,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy: Option<AgentToolPolicy>,

    #[serde(default)]
    pub model: SkillModelPreference,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub capabilities: BTreeMap<String, bool>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codex_config_overrides: Vec<String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub adapters: BTreeMap<String, SkillToolAdapter>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillApplicationResult {
    pub system_prompt_fragments: Vec<String>,
    pub prompt_prefixes: Vec<String>,
    pub prompt_suffixes: Vec<String>,
    pub directives: Vec<String>,
    pub extra_args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub mcp_servers: Vec<String>,
    pub tool_policy: Option<AgentToolPolicy>,
    pub codex_config_overrides: Vec<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
    pub capabilities: BTreeMap<String, bool>,
}

impl SkillApplicationResult {
    pub fn is_empty(&self) -> bool {
        self.system_prompt_fragments.is_empty()
            && self.prompt_prefixes.is_empty()
            && self.prompt_suffixes.is_empty()
            && self.directives.is_empty()
            && self.extra_args.is_empty()
            && self.env.is_empty()
            && self.mcp_servers.is_empty()
            && self.tool_policy.is_none()
            && self.codex_config_overrides.is_empty()
            && self.model.is_none()
            && self.timeout_secs.is_none()
            && self.capabilities.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    #[serde(default = "default_manifest_schema")]
    pub schema: String,
    pub skills: BTreeMap<String, SkillDefinition>,
}

fn default_manifest_schema() -> String {
    "animus.skills.v1".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillCapabilityKey {
    WritesFiles,
    MutatesState,
    RequiresCommit,
    EnforceProductChanges,
    IsResearch,
    IsUiUx,
    IsReview,
    IsTesting,
    IsRequirements,
}

pub fn parse_skill_capability_key(name: &str) -> Option<SkillCapabilityKey> {
    match name.trim().to_ascii_lowercase().as_str() {
        // Accept the legacy spellings we already emit in bundled skills and tests, but
        // reject typo-prone variants like `write_file` so bad config still fails loudly.
        "writes_files" | "write_files" | "file_write" | "file_writes" | "can_write" => {
            Some(SkillCapabilityKey::WritesFiles)
        }
        "mutates_state" | "state_mutation" | "managed_state_mutation" => Some(SkillCapabilityKey::MutatesState),
        "requires_commit" | "require_commit" => Some(SkillCapabilityKey::RequiresCommit),
        "enforce_product_changes" | "product_changes" => Some(SkillCapabilityKey::EnforceProductChanges),
        "is_research" | "research" => Some(SkillCapabilityKey::IsResearch),
        "is_ui_ux" | "ui_ux" | "ui-ux" => Some(SkillCapabilityKey::IsUiUx),
        "is_review" | "review" => Some(SkillCapabilityKey::IsReview),
        "is_testing" | "testing" => Some(SkillCapabilityKey::IsTesting),
        "is_requirements" | "requirements" => Some(SkillCapabilityKey::IsRequirements),
        _ => None,
    }
}

pub fn parse_skill_manifest(yaml: &str) -> Result<SkillManifest> {
    let manifest: SkillManifest =
        serde_yaml::from_str(yaml).map_err(|e| anyhow!("Failed to parse skill manifest: {e}"))?;
    for (key, skill) in &manifest.skills {
        validate_skill_definition(skill).map_err(|e| anyhow!("Skill '{key}' validation failed: {e}"))?;
    }
    Ok(manifest)
}

pub fn parse_skill_definition(yaml: &str) -> Result<SkillDefinition> {
    let skill: SkillDefinition =
        serde_yaml::from_str(yaml).map_err(|e| anyhow!("Failed to parse skill definition: {e}"))?;
    validate_skill_definition(&skill)?;
    Ok(skill)
}

// ---------------------------------------------------------------------------
// Non-fatal skill definition warnings
// ---------------------------------------------------------------------------

/// A structured warning for a skill definition that parses and validates but
/// contains a declaration the runtime will silently ignore (e.g. an
/// `activation.tools` value that can never match a real tool id). Warnings
/// never fail a load — existing definitions keep loading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillDefinitionWarning {
    /// Dotted path of the offending declaration, e.g. `activation.tools[0]`.
    pub field: String,
    /// One-line explanation of why the declaration is inert and how to fix it.
    pub message: String,
}

impl std::fmt::Display for SkillDefinitionWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}`: {}", self.field, self.message)
    }
}

/// THE single registry of skill-definition warning checks.
///
/// Every emission point (`animus skill info`, `animus skill list`,
/// `animus.skill.get` / `animus.skill.create` / `animus.skill.update` MCP
/// tools) reads this table via [`skill_definition_warnings`]. When a check
/// becomes obsolete (e.g. activation matching learns to normalize aliases),
/// delete its entry here and update `docs/architecture/skill-system.md`.
const SKILL_WARNING_CHECKS: &[fn(&SkillDefinition, &mut Vec<SkillDefinitionWarning>)] =
    &[warn_unknown_activation_tools, warn_unknown_adapter_tools, warn_skill_pins_model];

fn known_tool_ids_label() -> String {
    protocol::KNOWN_TOOL_IDS.join(", ")
}

/// Activation and adapter matching compare the DECLARED value literally
/// (case-insensitive, untrimmed) against the runtime tool id — see
/// [`SkillActivation::matches`] and `find_tool_adapter`. A declared value is
/// flagged when:
///
/// - it carries leading/trailing whitespace (the runtime tool id is trimmed,
///   the declared candidate is not, so a padded value never matches), or
/// - it does not normalize to a known canonical tool id — most often a typo,
///   but custom CLI tools (workflow YAML `tools:` / compiled `cli_tools`)
///   also land here, so the wording is "only matches if a custom tool with
///   this exact id is configured" rather than "never matches", or
/// - it is an alias for a canonical id (`oai` → `oai-runner`): workflow
///   phases always match against canonical ids, so the alias never activates
///   there. Ad-hoc runs pass `--tool` through verbatim, where the literal
///   alias can still match — hence the softer "will not activate on workflow
///   phases" wording rather than "never matches".
fn tool_id_warning_message(declared: &str) -> Option<String> {
    if declared != declared.trim() {
        return Some(format!(
            "'{declared}' has leading/trailing whitespace; matching compares the declared value literally, so this entry never matches — remove the padding"
        ));
    }
    let normalized = protocol::normalize_tool_id(declared);
    if !protocol::KNOWN_TOOL_IDS.contains(&normalized.as_str()) {
        return Some(format!(
            "'{declared}' is not a built-in tool id ({}) — it only matches if a custom CLI tool with this exact id is configured (workflow YAML `tools:`); otherwise the entry is silently ignored",
            known_tool_ids_label()
        ));
    }
    if !declared.eq_ignore_ascii_case(&normalized) {
        return Some(format!(
            "'{declared}' is an alias for '{normalized}'; workflow phases match canonical tool ids, so this entry will not activate there — declare '{normalized}' instead",
        ));
    }
    None
}

fn warn_unknown_activation_tools(skill: &SkillDefinition, out: &mut Vec<SkillDefinitionWarning>) {
    for (index, declared) in skill.activation.tools.iter().enumerate() {
        if let Some(message) = tool_id_warning_message(declared) {
            out.push(SkillDefinitionWarning { field: format!("activation.tools[{index}]"), message });
        }
    }
}

fn warn_unknown_adapter_tools(skill: &SkillDefinition, out: &mut Vec<SkillDefinitionWarning>) {
    for declared in skill.adapters.keys() {
        if let Some(message) = tool_id_warning_message(declared) {
            out.push(SkillDefinitionWarning { field: format!("adapters.{declared}"), message });
        }
    }
}

/// One-line warning explaining that a pinned model silently overrides the
/// model/tool of any agent that activates the skill. See
/// [`build_skill_application`], where `model.preferred`/`model.fallback` (and
/// any `adapters.<tool>.model`) flow into `SkillApplicationResult.model` and
/// then override the phase's resolved model — so an agent on `tool: oai-agent`
/// silently runs the skill's `claude-sonnet-4-6` instead.
fn pinned_model_warning_message(field: &str, model: &str) -> String {
    // A blank model is still applied verbatim (`build_skill_application` produces
    // `Some("")`), so it overrides the agent's resolved model just as silently —
    // render it explicitly rather than hiding it.
    let model_label = if model.trim().is_empty() { "<empty>".to_string() } else { format!("'{model}'") };
    format!(
        "{field} pins a model ({model_label}) — this overrides the model/tool of any agent that uses the skill; skills should be model-agnostic, so move the model to the agent profile (`agents.<name>.model`/`tool`) instead"
    )
}

fn warn_skill_pins_model(skill: &SkillDefinition, out: &mut Vec<SkillDefinitionWarning>) {
    // Warn on ANY present model field (including blank/whitespace): an empty
    // string still flows through as `Some("")` and overrides the agent's model.
    if let Some(model) = skill.model.preferred.as_deref() {
        out.push(SkillDefinitionWarning {
            field: "model.preferred".to_string(),
            message: pinned_model_warning_message("model.preferred", model),
        });
    }
    if let Some(model) = skill.model.fallback.as_deref() {
        out.push(SkillDefinitionWarning {
            field: "model.fallback".to_string(),
            message: pinned_model_warning_message("model.fallback", model),
        });
    }
    for (tool, adapter) in &skill.adapters {
        if let Some(model) = adapter.model.as_deref() {
            let field = format!("adapters.{tool}.model");
            let message = pinned_model_warning_message(&field, model);
            out.push(SkillDefinitionWarning { field, message });
        }
    }
}

/// Scan a skill definition for declarations the runtime will silently ignore.
/// Returns one warning per inert declaration; never errors. Companion to
/// [`validate_skill_definition`], which owns the fatal checks.
pub fn skill_definition_warnings(skill: &SkillDefinition) -> Vec<SkillDefinitionWarning> {
    let mut warnings = Vec::new();
    for check in SKILL_WARNING_CHECKS {
        check(skill, &mut warnings);
    }
    warnings
}

pub fn validate_skill_definition(skill: &SkillDefinition) -> Result<()> {
    if skill.name.is_empty() {
        return Err(anyhow!("Skill name must not be empty"));
    }
    if skill.name.contains(char::is_whitespace) {
        return Err(anyhow!("Skill name must not contain whitespace"));
    }
    if let Some(timeout) = skill.timeout_secs {
        if timeout == 0 {
            return Err(anyhow!("timeout_secs must be greater than zero"));
        }
    }
    if skill.activation.tools.iter().any(|value| value.trim().is_empty()) {
        return Err(anyhow!("activation.tools must not contain empty values"));
    }
    if skill.activation.models.iter().any(|value| value.trim().is_empty()) {
        return Err(anyhow!("activation.models must not contain empty values"));
    }
    for capability in skill.capabilities.keys() {
        let trimmed = capability.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("capabilities must not contain empty keys"));
        }
        if parse_skill_capability_key(trimmed).is_none() {
            return Err(anyhow!("unsupported capability override '{}'", trimmed));
        }
    }
    Ok(())
}

fn find_tool_adapter<'a>(skill: &'a SkillDefinition, tool_id: &str) -> Option<&'a SkillToolAdapter> {
    skill
        .adapters
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(tool_id.trim()))
        .map(|(_, adapter)| adapter)
}

fn build_skill_application(skill: &SkillDefinition, tool_id: Option<&str>) -> SkillApplicationResult {
    let mut result = SkillApplicationResult::default();

    if let Some(system) = &skill.prompt.system {
        result.system_prompt_fragments.push(system.clone());
    }
    if let Some(prefix) = &skill.prompt.prefix {
        result.prompt_prefixes.push(prefix.clone());
    }
    if let Some(suffix) = &skill.prompt.suffix {
        result.prompt_suffixes.push(suffix.clone());
    }
    result.directives.extend(skill.prompt.directives.clone());
    result.extra_args.extend(skill.extra_args.clone());
    result.env.extend(skill.env.clone());
    result.mcp_servers.extend(skill.mcp_servers.clone());
    result.tool_policy = skill.tool_policy.clone();
    result.codex_config_overrides.extend(skill.codex_config_overrides.clone());
    // A skill-pinned model silently overrides the model/tool the agent profile
    // resolved (a footgun: e.g. a skill's `claude-sonnet-4-6` forces agents off
    // `tool: oai-agent`). We surface this via `warn_skill_pins_model` but keep
    // the precedence unchanged for now (non-breaking). Follow-up (maintainer's
    // call): make the agent's explicit model win, or drop model support from the
    // skill schema entirely.
    result.model = skill.model.preferred.clone().or_else(|| skill.model.fallback.clone());
    result.timeout_secs = skill.timeout_secs;
    result.capabilities.extend(skill.capabilities.clone());

    if let Some(tool_id) = tool_id {
        if let Some(adapter) = find_tool_adapter(skill, tool_id) {
            if let Some(model) = &adapter.model {
                result.model = Some(model.clone());
            }
            if let Some(policy) = &adapter.tool_policy {
                result.tool_policy = Some(policy.clone());
            }
            result.extra_args.extend(adapter.extra_args.clone());
            result.env.extend(adapter.env.clone());
            result.mcp_servers.extend(adapter.mcp_servers.clone());
            result.codex_config_overrides.extend(adapter.codex_config_overrides.clone());

            if let Some(prompt_override) = &adapter.prompt_override {
                result.system_prompt_fragments.clear();
                result.prompt_prefixes.clear();
                result.prompt_suffixes.clear();
                result.directives.clear();
                if let Some(system) = &prompt_override.system {
                    result.system_prompt_fragments.push(system.clone());
                }
                if let Some(prefix) = &prompt_override.prefix {
                    result.prompt_prefixes.push(prefix.clone());
                }
                if let Some(suffix) = &prompt_override.suffix {
                    result.prompt_suffixes.push(suffix.clone());
                }
                result.directives.extend(prompt_override.directives.clone());
            }
        }
    }

    result
}

pub fn apply_skill_for_execution(
    skill: &SkillDefinition,
    tool_id: &str,
    model_id: Option<&str>,
) -> Option<SkillApplicationResult> {
    if !skill.activation.matches(tool_id, model_id) {
        return None;
    }

    Some(build_skill_application(skill, Some(tool_id)))
}

pub fn preview_skill_application(skill: &SkillDefinition) -> Option<SkillApplicationResult> {
    if !skill.activation.is_empty() {
        return None;
    }

    Some(build_skill_application(skill, None))
}

pub fn apply_skill_for_tool(skill: &SkillDefinition, tool_id: &str) -> SkillApplicationResult {
    apply_skill_for_execution(skill, tool_id, None).unwrap_or_default()
}

pub fn merge_skill_applications(results: &[SkillApplicationResult]) -> SkillApplicationResult {
    let mut merged = SkillApplicationResult::default();

    for r in results {
        merged.system_prompt_fragments.extend(r.system_prompt_fragments.clone());
        merged.prompt_prefixes.extend(r.prompt_prefixes.clone());
        merged.prompt_suffixes.extend(r.prompt_suffixes.clone());
        merged.directives.extend(r.directives.clone());
        merged.extra_args.extend(r.extra_args.clone());
        merged.env.extend(r.env.clone());
        merged.mcp_servers.extend(r.mcp_servers.clone());
        merged.codex_config_overrides.extend(r.codex_config_overrides.clone());
        merged.capabilities.extend(r.capabilities.clone());
        if r.tool_policy.is_some() {
            merged.tool_policy = r.tool_policy.clone();
        }
        if r.model.is_some() {
            merged.model = r.model.clone();
        }
        if r.timeout_secs.is_some() {
            merged.timeout_secs = r.timeout_secs;
        }
    }

    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_yaml() -> &'static str {
        "name: test-skill\n"
    }

    fn full_skill_yaml() -> &'static str {
        r#"
name: code-review
version: "1.0"
description: Automated code review skill
category: review
activation:
  tools:
    - claude
    - gemini
prompt:
  system: You are a code reviewer.
  prefix: "Review the following:"
  suffix: Provide actionable feedback.
  directives:
    - Focus on correctness
    - Check for security issues
tool_policy:
  allow:
    - "Read"
    - "Grep"
  deny:
    - "Write"
model:
  preferred: claude-sonnet-4-6
  fallback: gemini-3.1-pro-preview
mcp_servers:
  - animus
timeout_secs: 300
capabilities:
  is_review: true
  file_write: false
extra_args:
  - "--verbose"
env:
  REVIEW_MODE: strict
codex_config_overrides:
  - "max_tokens=4096"
tags:
  - review
  - quality
adapters:
  gemini:
    model: gemini-3.1-pro-preview
    extra_args:
      - "--sandbox=none"
    env:
      GEMINI_MODE: review
    mcp_servers:
      - extra-server
"#
    }

    #[test]
    fn test_parse_minimal_skill() {
        let skill = parse_skill_definition(minimal_yaml()).unwrap();
        assert_eq!(skill.name, "test-skill");
        assert!(skill.description.is_empty());
        assert!(skill.category.is_none());
        assert!(skill.version.is_none());
        assert!(skill.prompt.system.is_none());
        assert!(skill.extra_args.is_empty());
        assert!(skill.adapters.is_empty());
    }

    #[test]
    fn test_parse_full_skill() {
        let skill = parse_skill_definition(full_skill_yaml()).unwrap();
        assert_eq!(skill.name, "code-review");
        assert_eq!(skill.version.as_deref(), Some("1.0"));
        assert_eq!(skill.category, Some(SkillCategory::Review));
        assert_eq!(skill.prompt.system.as_deref(), Some("You are a code reviewer."));
        assert_eq!(skill.prompt.directives.len(), 2);
        assert_eq!(skill.model.preferred.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(skill.timeout_secs, Some(300));
        assert_eq!(skill.capabilities.get("is_review"), Some(&true));
        assert_eq!(skill.capabilities.get("file_write"), Some(&false));
        assert!(skill.adapters.contains_key("gemini"));
        assert_eq!(skill.tags, vec!["review", "quality"]);
    }

    #[test]
    fn test_parse_manifest() {
        let yaml = r#"
schema: animus.skills.v1
skills:
  review:
    name: review
    description: Review skill
    category: review
  test:
    name: test
    description: Testing skill
    category: testing
"#;
        let manifest = parse_skill_manifest(yaml).unwrap();
        assert_eq!(manifest.schema, "animus.skills.v1");
        assert_eq!(manifest.skills.len(), 2);
        assert!(manifest.skills.contains_key("review"));
        assert!(manifest.skills.contains_key("test"));
    }

    #[test]
    fn test_manifest_default_schema() {
        let yaml = r#"
skills:
  s:
    name: s
"#;
        let manifest = parse_skill_manifest(yaml).unwrap();
        assert_eq!(manifest.schema, "animus.skills.v1");
    }

    #[test]
    fn test_validate_empty_name() {
        let yaml = "name: \"\"\n";
        let err = parse_skill_definition(yaml).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn test_validate_whitespace_in_name() {
        let yaml = "name: \"has space\"\n";
        let err = parse_skill_definition(yaml).unwrap_err();
        assert!(err.to_string().contains("whitespace"));
    }

    #[test]
    fn test_validate_zero_timeout() {
        let yaml = "name: x\ntimeout_secs: 0\n";
        let err = parse_skill_definition(yaml).unwrap_err();
        assert!(err.to_string().contains("timeout_secs"));
    }

    #[test]
    fn test_validate_unknown_capability_override() {
        let yaml = "name: x\ncapabilities:\n  write_file: true\n";
        let err = parse_skill_definition(yaml).unwrap_err();
        assert!(err.to_string().contains("unsupported capability override"));
    }

    #[test]
    fn test_category_kebab_case_serde() {
        let yaml = "name: x\ncategory: documentation\n";
        let skill = parse_skill_definition(yaml).unwrap();
        assert_eq!(skill.category, Some(SkillCategory::Documentation));

        let json = serde_json::to_string(&skill.category).unwrap();
        assert!(json.contains("documentation"));
    }

    #[test]
    fn test_apply_skill_no_adapter() {
        let skill = parse_skill_definition(full_skill_yaml()).unwrap();
        let result = apply_skill_for_tool(&skill, "claude");
        assert!(result.system_prompt_fragments.iter().any(|s| s.contains("code reviewer")));
        assert_eq!(result.prompt_prefixes, vec!["Review the following:"]);
        assert_eq!(result.prompt_suffixes, vec!["Provide actionable feedback."]);
        assert_eq!(result.directives.len(), 2);
        assert_eq!(result.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(result.timeout_secs, Some(300));
        assert!(result.extra_args.contains(&"--verbose".to_string()));
        assert_eq!(result.env.get("REVIEW_MODE"), Some(&"strict".to_string()));
        assert!(result.tool_policy.is_some());
    }

    #[test]
    fn test_apply_skill_with_adapter() {
        let skill = parse_skill_definition(full_skill_yaml()).unwrap();
        let result = apply_skill_for_tool(&skill, "gemini");
        assert_eq!(result.model.as_deref(), Some("gemini-3.1-pro-preview"));
        assert!(result.extra_args.contains(&"--verbose".to_string()));
        assert!(result.extra_args.contains(&"--sandbox=none".to_string()));
        assert_eq!(result.env.get("GEMINI_MODE"), Some(&"review".to_string()));
        assert!(result.mcp_servers.contains(&"animus".to_string()));
        assert!(result.mcp_servers.contains(&"extra-server".to_string()));
    }

    #[test]
    fn test_apply_skill_adapter_prompt_override() {
        let yaml = r#"
name: override-test
prompt:
  system: Original system prompt
  directives:
    - original directive
adapters:
  claude:
    prompt_override:
      system: Overridden system prompt
      directives:
        - overridden directive
"#;
        let skill = parse_skill_definition(yaml).unwrap();
        let result = apply_skill_for_tool(&skill, "claude");
        assert_eq!(result.system_prompt_fragments.len(), 1);
        assert_eq!(result.system_prompt_fragments[0], "Overridden system prompt");
        assert!(result.prompt_prefixes.is_empty());
        assert!(result.prompt_suffixes.is_empty());
        assert_eq!(result.directives, vec!["overridden directive"]);
    }

    #[test]
    fn test_merge_skill_applications() {
        let r1 = SkillApplicationResult {
            system_prompt_fragments: vec!["prompt-a".into()],
            prompt_prefixes: vec!["prefix-a".into()],
            directives: vec!["dir-a".into()],
            model: Some("model-a".into()),
            timeout_secs: Some(60),
            env: BTreeMap::from([("A".into(), "1".into())]),
            ..Default::default()
        };
        let r2 = SkillApplicationResult {
            system_prompt_fragments: vec!["prompt-b".into()],
            prompt_suffixes: vec!["suffix-b".into()],
            directives: vec!["dir-b".into()],
            model: Some("model-b".into()),
            env: BTreeMap::from([("B".into(), "2".into())]),
            tool_policy: Some(AgentToolPolicy { allow: vec!["Read".into()], deny: vec![] }),
            ..Default::default()
        };

        let merged = merge_skill_applications(&[r1, r2]);
        assert_eq!(merged.system_prompt_fragments.len(), 2);
        assert_eq!(merged.prompt_prefixes, vec!["prefix-a"]);
        assert_eq!(merged.prompt_suffixes, vec!["suffix-b"]);
        assert_eq!(merged.directives.len(), 2);
        assert_eq!(merged.model.as_deref(), Some("model-b"));
        assert_eq!(merged.timeout_secs, Some(60));
        assert_eq!(merged.env.len(), 2);
        assert!(merged.tool_policy.is_some());
    }

    #[test]
    fn test_apply_skill_respects_activation_filters() {
        let skill = parse_skill_definition(full_skill_yaml()).unwrap();
        let result = apply_skill_for_execution(&skill, "codex", Some("codex"));
        assert!(result.is_none(), "claude/gemini-only skill should not activate for codex");
    }

    #[test]
    fn test_merge_empty() {
        let merged = merge_skill_applications(&[]);
        assert!(merged.system_prompt_fragments.is_empty());
        assert!(merged.model.is_none());
    }

    #[test]
    fn test_parse_mutates_state_skill_capability_aliases() {
        assert_eq!(parse_skill_capability_key("mutates_state"), Some(SkillCapabilityKey::MutatesState));
        assert_eq!(parse_skill_capability_key("managed_state_mutation"), Some(SkillCapabilityKey::MutatesState));
    }

    #[test]
    fn test_roundtrip_json() {
        let skill = parse_skill_definition(full_skill_yaml()).unwrap();
        let json = serde_json::to_string_pretty(&skill).unwrap();
        let deserialized: SkillDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, skill.name);
        assert_eq!(deserialized.category, skill.category);
        assert_eq!(deserialized.timeout_secs, skill.timeout_secs);
    }

    #[test]
    fn test_skip_serializing_defaults() {
        let skill = parse_skill_definition(minimal_yaml()).unwrap();
        let json = serde_json::to_string(&skill).unwrap();
        assert!(!json.contains("\"adapters\""));
        assert!(!json.contains("\"extra_args\""));
        assert!(!json.contains("\"tags\""));
        assert!(!json.contains("\"mcp_servers\""));
        assert!(!json.contains("\"capabilities\""));
        assert!(!json.contains("\"codex_config_overrides\""));
    }

    #[test]
    fn test_no_tool_warnings_for_minimal_and_full_fixtures() {
        let minimal = parse_skill_definition(minimal_yaml()).unwrap();
        assert!(skill_definition_warnings(&minimal).is_empty());

        // The full fixture's claude/gemini activation + gemini adapter are
        // canonical, so no tool-id warnings fire. It DOES pin models, so only
        // the model-pin warnings remain.
        let full = parse_skill_definition(full_skill_yaml()).unwrap();
        let fields: Vec<String> = skill_definition_warnings(&full).into_iter().map(|w| w.field).collect();
        assert_eq!(fields, ["model.preferred", "model.fallback", "adapters.gemini.model"]);
    }

    #[test]
    fn test_warning_fires_when_skill_pins_preferred_model() {
        let yaml = "name: x\nmodel:\n  preferred: claude-sonnet-4-6\n";
        let skill = parse_skill_definition(yaml).unwrap();
        let warnings = skill_definition_warnings(&skill);
        assert_eq!(warnings.len(), 1, "only the pinned model should warn: {warnings:?}");
        assert_eq!(warnings[0].field, "model.preferred");
        assert!(warnings[0].message.contains("'claude-sonnet-4-6'"), "got: {}", warnings[0].message);
        assert!(warnings[0].message.contains("overrides the model/tool"), "got: {}", warnings[0].message);
    }

    #[test]
    fn test_warning_fires_for_fallback_and_adapter_models() {
        let yaml =
            "name: x\nmodel:\n  fallback: gemini-3.1-pro-preview\nadapters:\n  claude:\n    model: claude-opus-4-6\n";
        let skill = parse_skill_definition(yaml).unwrap();
        let fields: Vec<String> = skill_definition_warnings(&skill).into_iter().map(|w| w.field).collect();
        assert_eq!(fields, ["model.fallback", "adapters.claude.model"]);
    }

    #[test]
    fn test_warning_fires_for_blank_pinned_model() {
        // A blank model is still applied as Some("") and overrides the agent, so
        // it must warn just like a real model id.
        let yaml = "name: x\nmodel:\n  preferred: \"  \"\n";
        let skill = parse_skill_definition(yaml).unwrap();
        let warnings = skill_definition_warnings(&skill);
        assert_eq!(warnings.len(), 1, "blank model should warn: {warnings:?}");
        assert_eq!(warnings[0].field, "model.preferred");
        assert!(warnings[0].message.contains("<empty>"), "got: {}", warnings[0].message);
    }

    #[test]
    fn test_no_model_pin_warning_for_model_free_skill() {
        // Activation by model is fine (it selects, it does not override); only a
        // pinned model.preferred/fallback/adapter model should warn.
        let yaml = "name: x\nactivation:\n  tools:\n    - claude\n  models:\n    - claude-sonnet-4-6\n";
        let skill = parse_skill_definition(yaml).unwrap();
        assert!(skill_definition_warnings(&skill).is_empty(), "model-free skill must not warn");
    }

    #[test]
    fn test_warning_fires_for_unknown_activation_tool() {
        let yaml = "name: x\nactivation:\n  tools:\n    - claud\n    - codex\n";
        let skill = parse_skill_definition(yaml).unwrap();
        let warnings = skill_definition_warnings(&skill);
        assert_eq!(warnings.len(), 1, "only the typo entry should warn: {warnings:?}");
        assert_eq!(warnings[0].field, "activation.tools[0]");
        assert!(warnings[0].message.contains("'claud' is not a built-in tool id"));
        assert!(warnings[0].message.contains("claude, codex, gemini, opencode, oai-runner"));
    }

    #[test]
    fn test_warning_fires_for_alias_activation_tool() {
        let yaml = "name: x\nactivation:\n  tools:\n    - minimax\n";
        let skill = parse_skill_definition(yaml).unwrap();
        let warnings = skill_definition_warnings(&skill);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("alias for 'oai-runner'"), "got: {}", warnings[0].message);
        assert!(warnings[0].message.contains("will not activate there"), "got: {}", warnings[0].message);
    }

    #[test]
    fn test_warning_fires_for_whitespace_padded_activation_tool() {
        let yaml = "name: x\nactivation:\n  tools:\n    - ' claude '\n";
        let skill = parse_skill_definition(yaml).unwrap();
        let warnings = skill_definition_warnings(&skill);
        assert_eq!(warnings.len(), 1, "padded entry should warn: {warnings:?}");
        assert!(warnings[0].message.contains("whitespace"), "got: {}", warnings[0].message);
    }

    #[test]
    fn test_no_warning_for_known_tools_in_any_case() {
        let yaml = "name: x\nactivation:\n  tools:\n    - Claude\n    - CODEX\n    - oai-runner\n";
        let skill = parse_skill_definition(yaml).unwrap();
        assert!(skill_definition_warnings(&skill).is_empty());
    }

    #[test]
    fn test_warning_fires_for_unknown_adapter_key() {
        let yaml = "name: x\nadapters:\n  geminni:\n    model: gemini-2.5-pro\n";
        let skill = parse_skill_definition(yaml).unwrap();
        let warnings = skill_definition_warnings(&skill);
        // The unknown adapter key warns, and its pinned model warns separately.
        let unknown = warnings.iter().find(|w| w.field == "adapters.geminni").expect("unknown-tool warning");
        assert!(unknown.message.contains("not a built-in tool id"));
        assert!(
            warnings.iter().any(|w| w.field == "adapters.geminni.model"),
            "adapter model pin should also warn: {warnings:?}"
        );
    }

    #[test]
    fn test_warnings_never_fail_parse_or_validate() {
        let yaml = "name: x\nactivation:\n  tools:\n    - not-a-tool\n";
        let skill = parse_skill_definition(yaml).expect("warning-bearing definition must still load");
        assert!(validate_skill_definition(&skill).is_ok());
    }

    #[test]
    fn test_manifest_validation_propagates() {
        let yaml = r#"
skills:
  bad:
    name: ""
"#;
        let err = parse_skill_manifest(yaml).unwrap_err();
        assert!(err.to_string().contains("bad"));
        assert!(err.to_string().contains("must not be empty"));
    }
}
