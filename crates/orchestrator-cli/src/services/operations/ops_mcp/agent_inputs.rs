use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct AgentRunInput {
    #[serde(default = "default_claude")]
    pub(super) tool: String,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) prompt: Option<String>,
    #[serde(default)]
    pub(super) cwd: Option<String>,
    #[serde(default)]
    pub(super) timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) context_json: Option<String>,
    #[serde(default)]
    pub(super) runtime_contract_json: Option<String>,
    #[serde(default = "default_true")]
    pub(super) detach: bool,
    #[serde(default)]
    pub(super) run_id: Option<String>,
    #[serde(default)]
    pub(super) runner_scope: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct AgentControlInput {
    pub(super) run_id: String,
    pub(super) action: String,
    #[serde(default)]
    pub(super) runner_scope: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct AgentStatusInput {
    pub(super) run_id: String,
    #[serde(default)]
    pub(super) runner_scope: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}
