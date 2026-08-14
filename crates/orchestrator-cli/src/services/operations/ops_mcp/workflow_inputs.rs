use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct WorkflowListInput {
    #[serde(default)]
    pub(super) project_root: Option<String>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) workflow_ref: Option<String>,
    /// Filter workflows linked to a subject id. Matched exactly against the id
    /// the workflow stored — built-in kinds (task/requirement) store the
    /// qualified form, so filter with `task:TASK-001`.
    #[serde(default)]
    pub(super) subject_id: Option<String>,
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
    pub(super) title: Option<String>,
    /// Subject to run the workflow for, any kind (task, requirement, or dynamic
    /// kinds like blog/post). Accepts a qualified id (`task:TASK-001` /
    /// `blog:BLOG-001` — kind trusted) or a bare id (`TASK-001` — kind resolved
    /// via the subject router). Mutually exclusive with `title`.
    #[serde(default)]
    pub(super) subject_id: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) workflow_ref: Option<String>,
    #[serde(default)]
    pub(super) input_json: Option<String>,
    /// Structured workflow input. Prefer this over `input_json`; the latter is
    /// retained for compatibility with older MCP clients.
    #[serde(default)]
    pub(super) input: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) vars: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) tool: Option<String>,
    #[serde(default)]
    pub(super) phase_timeout_secs: Option<u64>,
    /// Durable actor/workspace-scoped operation key. Reusing the same key and
    /// effective request replays the canonical workflow; changed input conflicts.
    #[serde(default)]
    pub(super) idempotency_key: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(super) struct BulkWorkflowRunItem {
    /// Subject to run the workflow for. Accepts a qualified id (`task:TASK-001`)
    /// or a bare id (`TASK-001`, kind resolved via the subject router).
    pub(super) subject_id: String,
    #[serde(default)]
    pub(super) workflow_ref: Option<String>,
    #[serde(default)]
    pub(super) input_json: Option<String>,
    #[serde(default)]
    pub(super) input: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) vars: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) tool: Option<String>,
    #[serde(default)]
    pub(super) phase_timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) idempotency_key: Option<String>,
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
    /// Structured raw WorkflowConfig source model. Prefer this over `file`.
    #[serde(default)]
    pub(super) config: Option<Value>,
    /// Compatibility path to a JSON file containing the raw WorkflowConfig.
    /// Mutually exclusive with `config`.
    #[serde(default)]
    pub(super) file: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowConfigAgentSetInput {
    pub(super) id: String,
    /// Structured agent profile overlay. Prefer this over `input_json`.
    #[serde(default)]
    pub(super) profile: Option<Value>,
    /// Compatibility JSON string. Mutually exclusive with `profile`.
    #[serde(default)]
    pub(super) input_json: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct WorkflowConfigWorkflowSetInput {
    /// Structured workflow definition. Prefer this over `input_json`.
    #[serde(default)]
    pub(super) workflow: Option<Value>,
    /// Compatibility JSON string. Mutually exclusive with `workflow`.
    #[serde(default)]
    pub(super) input_json: Option<String>,
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
    /// Subject to execute the workflow for. Accepts a qualified id
    /// (`task:TASK-001`) or a bare id (`TASK-001`, kind resolved via the subject
    /// router).
    pub(super) subject_id: String,
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
    /// Structured workflow input. Prefer this over `input_json`.
    #[serde(default)]
    pub(super) input: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) vars: std::collections::HashMap<String, String>,
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
