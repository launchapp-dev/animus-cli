use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct WorkflowListInput {
    #[serde(default)]
    pub(super) project_root: Option<String>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) workflow_ref: Option<String>,
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    pub(super) phase_id: Option<String>,
    #[serde(default)]
    pub(super) search: Option<String>,
    #[serde(default)]
    pub(super) sort: Option<String>,
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) offset: Option<usize>,
    #[serde(default)]
    pub(super) max_tokens: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowRunInput {
    #[serde(default)]
    pub(super) task_id: Option<String>,
    #[serde(default)]
    pub(super) requirement_id: Option<String>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) workflow_ref: Option<String>,
    #[serde(default)]
    pub(super) input_json: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct BulkWorkflowRunItem {
    pub(super) task_id: String,
    #[serde(default)]
    pub(super) workflow_ref: Option<String>,
    #[serde(default)]
    pub(super) input_json: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowRunMultipleInput {
    pub(super) runs: Vec<BulkWorkflowRunItem>,
    #[serde(default)]
    pub(super) on_error: OnError,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowDestructiveInput {
    pub(super) id: String,
    #[serde(default)]
    pub(super) confirm: Option<String>,
    #[serde(default)]
    pub(super) dry_run: bool,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowPhaseGetInput {
    pub(super) phase: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowConfigSetInput {
    /// Path to a JSON file with the full WorkflowConfig. Omit to read from stdin
    /// (not available over MCP — a file path is required here).
    pub(super) file: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowConfigAgentSetInput {
    pub(super) id: String,
    pub(super) input_json: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowConfigWorkflowSetInput {
    pub(super) input_json: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowConfigEntityRemoveInput {
    pub(super) id: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowExecuteInput {
    pub(super) task_id: String,
    #[serde(default)]
    pub(super) workflow_ref: Option<String>,
    #[serde(default)]
    pub(super) phase: Option<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) tool: Option<String>,
    #[serde(default)]
    pub(super) phase_timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) input_json: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowPhaseApproveInput {
    pub(super) workflow_id: String,
    #[serde(alias = "phase")]
    pub(super) phase_id: String,
    #[serde(default, alias = "note")]
    pub(super) feedback: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowPhaseRejectInput {
    pub(super) workflow_id: String,
    #[serde(alias = "phase")]
    pub(super) phase_id: String,
    #[serde(alias = "note", alias = "feedback")]
    pub(super) reason: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}
