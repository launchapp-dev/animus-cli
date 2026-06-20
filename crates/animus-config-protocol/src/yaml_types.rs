use std::collections::{BTreeMap, HashMap};

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::yaml_diagnostic::closest_match;

use crate::agent_types::{
    default_eval_expected_exit, default_eval_pass_threshold, AgentProfileOverlay, EvalKind, EvalOnFail, Idempotency,
    PhaseExecutionMode,
};

use crate::workflow_types::*;

pub const YAML_WORKFLOWS_DIR: &str = "workflows";
pub const GENERATED_WORKFLOW_OVERLAY_FILE_NAME: &str = "generated-workflow.yaml";
pub const GENERATED_RUNTIME_OVERLAY_FILE_NAME: &str = "generated-runtime.yaml";
pub const DEFAULT_WORKFLOW_TEMPLATE_FILE_NAME: &str = "custom.yaml";
pub const STANDARD_WORKFLOW_TEMPLATE_FILE_NAME: &str = "standard-workflow.yaml";
pub const HOTFIX_WORKFLOW_TEMPLATE_FILE_NAME: &str = "hotfix-workflow.yaml";
pub const RESEARCH_WORKFLOW_TEMPLATE_FILE_NAME: &str = "research-workflow.yaml";

/// A named model+tool entry in the top-level `models:` registry.
/// Agents reference these by name in their `models:` list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRegistryEntry {
    /// The model identifier (e.g. "o4-mini", "claude-sonnet-4-20250514").
    pub model: String,
    /// Optional explicit tool override. When omitted, the tool is
    /// auto-derived from the model ID via `tool_for_model_id()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct YamlPhaseRichConfig {
    #[serde(default = "default_max_rework_attempts")]
    pub(super) max_rework_attempts: u32,
    #[serde(default)]
    pub(super) skip_if: Vec<String>,
    #[serde(default)]
    pub(super) on_verdict: HashMap<String, PhaseTransitionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) budget: Option<BudgetConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct YamlSubWorkflowRef {
    pub(super) workflow_ref: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(super) enum YamlPhaseEntry {
    SubWorkflow(YamlSubWorkflowRef),
    Simple(String),
    Rich(HashMap<String, YamlPhaseRichConfig>),
}

impl<'de> Deserialize<'de> for YamlPhaseEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match value {
            serde_yaml::Value::String(id) => Ok(YamlPhaseEntry::Simple(id)),
            serde_yaml::Value::Mapping(_) => {
                let v = value.clone();
                let sub_err = match serde_yaml::from_value::<YamlSubWorkflowRef>(v) {
                    Ok(sub) => return Ok(YamlPhaseEntry::SubWorkflow(sub)),
                    Err(e) => e,
                };
                let rich_err = match serde_yaml::from_value::<HashMap<String, YamlPhaseRichConfig>>(value.clone()) {
                    Ok(map) => {
                        if map.len() != 1 {
                            return Err(de::Error::custom(format!(
                                "rich phase entry must be a single-key map `{{ <phase_id>: {{ ... }} }}`; got {} keys",
                                map.len()
                            )));
                        }
                        return Ok(YamlPhaseEntry::Rich(map));
                    }
                    Err(e) => e,
                };
                if value_looks_like_sub_workflow_ref_typo(&value) {
                    return Err(de::Error::custom(format!("invalid sub-workflow ref: {}", sub_err)));
                }
                Err(de::Error::custom(format!(
                    "invalid phase entry shape: {}. expected one of: \
                     a string phase id (e.g. `- impl`); \
                     a sub-workflow ref (`{{ workflow_ref: <name> }}`); \
                     or a rich config single-key map (`{{ <phase_id>: {{ max_rework_attempts: N, ... }} }}`)",
                    rich_err
                )))
            }
            other => Err(de::Error::custom(format!(
                "invalid phase entry: expected a string phase id, a sub-workflow ref map, or a rich config map; got {}",
                value_kind(&other)
            ))),
        }
    }
}

/// Heuristic: only flag an entry as a sub-workflow ref typo when the
/// shape closely resembles `{ workflow_ref: <string> }` — i.e. exactly
/// one key, the key starts with `workflow_re`, and its value is a scalar
/// string. This avoids stealing legitimate rich phase IDs like
/// `workflow_setup:` whose value is a map.
fn value_looks_like_sub_workflow_ref_typo(value: &serde_yaml::Value) -> bool {
    let serde_yaml::Value::Mapping(map) = value else {
        return false;
    };
    if map.len() != 1 {
        return false;
    }
    let Some((k, v)) = map.iter().next() else {
        return false;
    };
    let serde_yaml::Value::String(key) = k else {
        return false;
    };
    let is_string_value = matches!(v, serde_yaml::Value::String(_));
    if !is_string_value {
        return false;
    }
    let lower = key.to_ascii_lowercase();
    lower != "workflow_ref" && (lower.starts_with("workflow_re") || lower == "workflowref")
}

fn value_kind(v: &serde_yaml::Value) -> &'static str {
    match v {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "boolean",
        serde_yaml::Value::Number(_) => "number",
        serde_yaml::Value::String(_) => "string",
        serde_yaml::Value::Sequence(_) => "sequence",
        serde_yaml::Value::Mapping(_) => "mapping",
        serde_yaml::Value::Tagged(_) => "tagged",
    }
}

/// Permissive YAML representation of a worktree block.
///
/// Authors may write either the long form (`worktree: { mode: skip, ... }`),
/// the short form (`worktree: skip`), or the boolean shorthand
/// (`worktree: false` -> skip, `worktree: true` -> auto). The parser
/// normalizes all three into `WorktreeConfig`.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum YamlPhaseWorktree {
    /// `worktree: false` / `worktree: true` boolean shorthand.
    Bool(bool),
    /// `worktree: skip` short-form scalar (auto / required / skip).
    Mode(String),
    /// `worktree: { mode: ..., cleanup: ..., base_ref: ... }` long-form map.
    Full(WorktreeConfig),
}

const WORKTREE_VALID_MODES: &[&str] = &["auto", "required", "skip"];

impl<'de> Deserialize<'de> for YamlPhaseWorktree {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match value {
            serde_yaml::Value::Bool(flag) => Ok(YamlPhaseWorktree::Bool(flag)),
            serde_yaml::Value::String(scalar) => {
                let trimmed = scalar.trim();
                let lower = trimmed.to_ascii_lowercase();
                if WORKTREE_VALID_MODES.iter().any(|m| *m == lower) {
                    Ok(YamlPhaseWorktree::Mode(scalar))
                } else {
                    let suggestion = match lower.as_str() {
                        "yes" | "on" | "enabled" | "enable" | "true" => Some("auto".to_string()),
                        "no" | "off" | "disabled" | "disable" | "false" | "none" => Some("skip".to_string()),
                        "needed" | "must" | "force" | "mandatory" => Some("required".to_string()),
                        _ => closest_match(&lower, WORKTREE_VALID_MODES, 2).map(|s| s.to_string()),
                    };
                    let mut msg = format!(
                        "invalid `worktree:` value `{}`: expected one of: \
                         a string \"auto\" | \"required\" | \"skip\"; \
                         a boolean `true` (= auto) | `false` (= skip); \
                         or a map {{ mode: <string>, cleanup: <bool>, base_ref: <string> }}",
                        scalar
                    );
                    if let Some(s) = suggestion {
                        msg.push_str(&format!(". did you mean `{}`?", s));
                    }
                    Err(de::Error::custom(msg))
                }
            }
            serde_yaml::Value::Mapping(_) => {
                let v = value.clone();
                let parsed: Result<WorktreeConfig, _> = serde_yaml::from_value(v);
                match parsed {
                    Ok(cfg) => Ok(YamlPhaseWorktree::Full(cfg)),
                    Err(e) => {
                        let mut hint = String::new();
                        if let serde_yaml::Value::Mapping(map) = &value {
                            if let Some(serde_yaml::Value::String(s)) = map
                                .iter()
                                .find(|(k, _)| matches!(k, serde_yaml::Value::String(s) if s == "mode"))
                                .map(|(_, v)| v)
                            {
                                {
                                    let lower = s.trim().to_ascii_lowercase();
                                    if !WORKTREE_VALID_MODES.iter().any(|m| *m == lower) {
                                        let sugg = closest_match(&lower, WORKTREE_VALID_MODES, 2);
                                        match sugg {
                                            Some(s2) => hint = format!(
                                                ". `mode: {}` is not valid (expected auto | required | skip); did you mean `{}`?",
                                                s, s2
                                            ),
                                            None => hint = format!(
                                                ". `mode: {}` is not valid (expected auto | required | skip)",
                                                s
                                            ),
                                        }
                                    }
                                }
                            }
                        }
                        Err(de::Error::custom(format!(
                            "invalid `worktree:` map: {}{}. expected {{ mode: <auto|required|skip>, cleanup: <bool>, base_ref: <string> }}",
                            e, hint
                        )))
                    }
                }
            }
            other => Err(de::Error::custom(format!(
                "invalid `worktree:` value: expected a string, boolean, or map; got {}",
                value_kind(&other)
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct YamlWorkflowDefinition {
    pub(super) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) phases: Vec<YamlPhaseEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) variables: Vec<WorkflowVariable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) worktree: Option<YamlPhaseWorktree>,
    pub(super) budget: Option<BudgetConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct YamlCommandDefinition {
    pub(super) program: String,
    #[serde(default)]
    pub(super) args: Vec<String>,
    #[serde(default)]
    pub(super) env: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) cwd_mode: Option<String>,
    #[serde(default)]
    pub(super) cwd_path: Option<String>,
    #[serde(default)]
    pub(super) timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) success_exit_codes: Option<Vec<i32>>,
    #[serde(default)]
    pub(super) parse_json_output: Option<bool>,
    #[serde(default)]
    pub(super) expected_result_kind: Option<String>,
    #[serde(default)]
    pub(super) expected_schema: Option<Value>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) failure_pattern: Option<String>,
    #[serde(default)]
    pub(super) excerpt_max_chars: Option<usize>,
    #[serde(default)]
    pub(super) on_success_verdict: Option<String>,
    #[serde(default)]
    pub(super) on_failure_verdict: Option<String>,
    #[serde(default)]
    pub(super) confidence: Option<f32>,
    #[serde(default)]
    pub(super) failure_risk: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct YamlManualDefinition {
    pub(super) instructions: String,
    #[serde(default)]
    pub(super) approval_note_required: Option<bool>,
    #[serde(default)]
    pub(super) timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct YamlEvalCheck {
    pub(super) id: String,
    pub(super) kind: EvalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) timeout_secs: Option<u64>,
    #[serde(default = "default_eval_expected_exit")]
    pub(super) expected_exit: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct YamlEvalsConfig {
    #[serde(default = "default_eval_pass_threshold")]
    pub(super) pass_threshold: f32,
    #[serde(default)]
    pub(super) on_fail: EvalOnFail,
    #[serde(default)]
    pub(super) max_reworks: u32,
    #[serde(default)]
    pub(super) checks: Vec<YamlEvalCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct YamlPhaseDefinition {
    pub(super) mode: PhaseExecutionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "agent_id")]
    pub(super) agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) command: Option<YamlCommandDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) manual: Option<YamlManualDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) directive: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) runtime: Option<crate::agent_types::AgentRuntimeOverrides>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) capabilities: Option<protocol::PhaseCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) output_contract: Option<crate::agent_types::PhaseOutputContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) output_json_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) decision_contract: Option<crate::agent_types::PhaseDecisionContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) retry: Option<crate::agent_types::PhaseRetryConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) default_tool: Option<String>,
    #[serde(default)]
    pub(super) idempotency: Idempotency,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) worktree: Option<YamlPhaseWorktree>,
    pub(super) evals: Option<YamlEvalsConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct YamlWorkflowFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) default_workflow_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) phase_catalog: Option<BTreeMap<String, PhaseUiDefinition>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) workflows: Vec<YamlWorkflowDefinition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) phases: BTreeMap<String, YamlPhaseDefinition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) agents: BTreeMap<String, AgentProfileOverlay>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) agent_channels: BTreeMap<String, AgentChannelConfig>,
    /// Top-level model registry. Agents reference entries by name in their
    /// `models:` list to build primary + fallback chains.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) models: BTreeMap<String, ModelRegistryEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) tools_allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) mcp_servers: BTreeMap<String, McpServerDefinition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) phase_mcp_bindings: BTreeMap<String, PhaseMcpBinding>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) tools: BTreeMap<String, ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) integrations: Option<IntegrationsConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) schedules: Vec<WorkflowSchedule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) triggers: Vec<WorkflowTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) daemon: Option<DaemonConfig>,
    /// Top-level declarative secret references. Each entry maps a logical
    /// secret name to a process env var; reference values with
    /// `${secret.<name>}` in any YAML scalar.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) secrets: BTreeMap<String, SecretRef>,
}

/// Title-case a phase id (`code-review` -> `Code Review`) for default UI labels.
/// Pure helper used by the YAML parser; the kernel's `yaml_scaffold` re-exports it.
pub fn title_case_phase_id(phase_id: &str) -> String {
    phase_id
        .split(['-', '_'])
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut label = first.to_ascii_uppercase().to_string();
                    label.push_str(chars.as_str());
                    label
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
