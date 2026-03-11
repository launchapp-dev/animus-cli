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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillModelPreference {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<SkillCategory>,

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

#[derive(Debug, Clone, Default)]
pub struct SkillApplicationResult {
    pub system_prompt_fragments: Vec<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    #[serde(default = "default_manifest_schema")]
    pub schema: String,
    pub skills: BTreeMap<String, SkillDefinition>,
}

fn default_manifest_schema() -> String {
    "ao.skills.v1".to_string()
}

pub fn parse_skill_manifest(yaml: &str) -> Result<SkillManifest> {
    let manifest: SkillManifest =
        serde_yaml::from_str(yaml).map_err(|e| anyhow!("Failed to parse skill manifest: {e}"))?;
    for (key, skill) in &manifest.skills {
        validate_skill_definition(skill)
            .map_err(|e| anyhow!("Skill '{key}' validation failed: {e}"))?;
    }
    Ok(manifest)
}

pub fn parse_skill_definition(yaml: &str) -> Result<SkillDefinition> {
    let skill: SkillDefinition =
        serde_yaml::from_str(yaml).map_err(|e| anyhow!("Failed to parse skill definition: {e}"))?;
    validate_skill_definition(&skill)?;
    Ok(skill)
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
    Ok(())
}

pub fn apply_skill_for_tool(skill: &SkillDefinition, tool_id: &str) -> SkillApplicationResult {
    let mut result = SkillApplicationResult::default();

    if let Some(system) = &skill.prompt.system {
        result.system_prompt_fragments.push(system.clone());
    }
    if let Some(prefix) = &skill.prompt.prefix {
        result.system_prompt_fragments.push(prefix.clone());
    }
    if let Some(suffix) = &skill.prompt.suffix {
        result.system_prompt_fragments.push(suffix.clone());
    }
    result.directives.extend(skill.prompt.directives.clone());
    result.extra_args.extend(skill.extra_args.clone());
    result.env.extend(skill.env.clone());
    result.mcp_servers.extend(skill.mcp_servers.clone());
    result.tool_policy = skill.tool_policy.clone();
    result
        .codex_config_overrides
        .extend(skill.codex_config_overrides.clone());
    result.model = skill.model.preferred.clone();
    result.timeout_secs = skill.timeout_secs;
    result.capabilities.extend(skill.capabilities.clone());

    if let Some(adapter) = skill.adapters.get(tool_id) {
        if let Some(model) = &adapter.model {
            result.model = Some(model.clone());
        }
        if let Some(policy) = &adapter.tool_policy {
            result.tool_policy = Some(policy.clone());
        }
        result.extra_args.extend(adapter.extra_args.clone());
        result.env.extend(adapter.env.clone());
        result.mcp_servers.extend(adapter.mcp_servers.clone());
        result
            .codex_config_overrides
            .extend(adapter.codex_config_overrides.clone());

        if let Some(prompt_override) = &adapter.prompt_override {
            result.system_prompt_fragments.clear();
            result.directives.clear();
            if let Some(system) = &prompt_override.system {
                result.system_prompt_fragments.push(system.clone());
            }
            if let Some(prefix) = &prompt_override.prefix {
                result.system_prompt_fragments.push(prefix.clone());
            }
            if let Some(suffix) = &prompt_override.suffix {
                result.system_prompt_fragments.push(suffix.clone());
            }
            result
                .directives
                .extend(prompt_override.directives.clone());
        }
    }

    result
}

pub fn merge_skill_applications(results: &[SkillApplicationResult]) -> SkillApplicationResult {
    let mut merged = SkillApplicationResult::default();

    for r in results {
        merged
            .system_prompt_fragments
            .extend(r.system_prompt_fragments.clone());
        merged.directives.extend(r.directives.clone());
        merged.extra_args.extend(r.extra_args.clone());
        merged.env.extend(r.env.clone());
        merged.mcp_servers.extend(r.mcp_servers.clone());
        merged
            .codex_config_overrides
            .extend(r.codex_config_overrides.clone());
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
  - ao
timeout_secs: 300
capabilities:
  web_search: true
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
        assert_eq!(
            skill.prompt.system.as_deref(),
            Some("You are a code reviewer.")
        );
        assert_eq!(skill.prompt.directives.len(), 2);
        assert_eq!(skill.model.preferred.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(skill.timeout_secs, Some(300));
        assert_eq!(skill.capabilities.get("web_search"), Some(&true));
        assert_eq!(skill.capabilities.get("file_write"), Some(&false));
        assert!(skill.adapters.contains_key("gemini"));
        assert_eq!(skill.tags, vec!["review", "quality"]);
    }

    #[test]
    fn test_parse_manifest() {
        let yaml = r#"
schema: ao.skills.v1
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
        assert_eq!(manifest.schema, "ao.skills.v1");
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
        assert_eq!(manifest.schema, "ao.skills.v1");
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
        assert!(result
            .system_prompt_fragments
            .iter()
            .any(|s| s.contains("code reviewer")));
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
        assert_eq!(
            result.env.get("GEMINI_MODE"),
            Some(&"review".to_string())
        );
        assert!(result.mcp_servers.contains(&"ao".to_string()));
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
        assert_eq!(
            result.system_prompt_fragments[0],
            "Overridden system prompt"
        );
        assert_eq!(result.directives, vec!["overridden directive"]);
    }

    #[test]
    fn test_merge_skill_applications() {
        let r1 = SkillApplicationResult {
            system_prompt_fragments: vec!["prompt-a".into()],
            directives: vec!["dir-a".into()],
            model: Some("model-a".into()),
            timeout_secs: Some(60),
            env: BTreeMap::from([("A".into(), "1".into())]),
            ..Default::default()
        };
        let r2 = SkillApplicationResult {
            system_prompt_fragments: vec!["prompt-b".into()],
            directives: vec!["dir-b".into()],
            model: Some("model-b".into()),
            env: BTreeMap::from([("B".into(), "2".into())]),
            tool_policy: Some(AgentToolPolicy {
                allow: vec!["Read".into()],
                deny: vec![],
            }),
            ..Default::default()
        };

        let merged = merge_skill_applications(&[r1, r2]);
        assert_eq!(merged.system_prompt_fragments.len(), 2);
        assert_eq!(merged.directives.len(), 2);
        assert_eq!(merged.model.as_deref(), Some("model-b"));
        assert_eq!(merged.timeout_secs, Some(60));
        assert_eq!(merged.env.len(), 2);
        assert!(merged.tool_policy.is_some());
    }

    #[test]
    fn test_merge_empty() {
        let merged = merge_skill_applications(&[]);
        assert!(merged.system_prompt_fragments.is_empty());
        assert!(merged.model.is_none());
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
