use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_runtime_config::{AgentProfile, PhaseExecutionMode};

use super::types::*;

pub const YAML_WORKFLOWS_DIR: &str = "workflows";
pub const GENERATED_WORKFLOW_OVERLAY_FILE_NAME: &str = "generated-workflow.yaml";
pub const GENERATED_RUNTIME_OVERLAY_FILE_NAME: &str = "generated-runtime.yaml";
pub const DEFAULT_WORKFLOW_TEMPLATE_FILE_NAME: &str = "custom.yaml";
pub const STANDARD_WORKFLOW_TEMPLATE_FILE_NAME: &str = "standard-workflow.yaml";
pub const HOTFIX_WORKFLOW_TEMPLATE_FILE_NAME: &str = "hotfix-workflow.yaml";
pub const RESEARCH_WORKFLOW_TEMPLATE_FILE_NAME: &str = "research-workflow.yaml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct YamlPhaseRichConfig {
    #[serde(default = "default_max_rework_attempts")]
    pub(super) max_rework_attempts: u32,
    #[serde(default)]
    pub(super) skip_if: Vec<String>,
    #[serde(default)]
    pub(super) on_verdict: HashMap<String, PhaseTransitionConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct YamlSubWorkflowRef {
    pub(super) workflow_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum YamlPhaseEntry {
    SubWorkflow(YamlSubWorkflowRef),
    Simple(String),
    Rich(HashMap<String, YamlPhaseRichConfig>),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) post_success: Option<YamlPostSuccessConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) variables: Vec<WorkflowVariable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct YamlPostSuccessConfig {
    #[serde(default)]
    pub(super) merge: Option<YamlMergeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct YamlMergeConfig {
    #[serde(default)]
    pub(super) strategy: Option<String>,
    #[serde(default = "default_target_branch")]
    pub(super) target_branch: String,
    #[serde(default)]
    pub(super) create_pr: bool,
    #[serde(default)]
    pub(super) auto_merge: bool,
    #[serde(default)]
    pub(super) cleanup_worktree: bool,
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
    pub(super) runtime: Option<crate::agent_runtime_config::AgentRuntimeOverrides>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) capabilities: Option<protocol::PhaseCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) output_contract: Option<crate::agent_runtime_config::PhaseOutputContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) output_json_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) decision_contract: Option<crate::agent_runtime_config::PhaseDecisionContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) retry: Option<crate::agent_runtime_config::PhaseRetryConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) default_tool: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub(super) agents: BTreeMap<String, AgentProfile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) tools_allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) mcp_servers: BTreeMap<String, McpServerDefinition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) tools: BTreeMap<String, ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) integrations: Option<IntegrationsConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) schedules: Vec<WorkflowSchedule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) daemon: Option<DaemonConfig>,
}
