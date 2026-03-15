use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct RequirementListInput {
    #[serde(default)]
    pub(super) project_root: Option<String>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default, rename = "type")]
    pub(super) requirement_type: Option<String>,
    #[serde(default)]
    pub(super) tag: Vec<String>,
    #[serde(default)]
    pub(super) linked_task_id: Option<String>,
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
pub(super) struct RequirementGetInput {
    pub(super) id: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct RequirementCreateInput {
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default, rename = "type")]
    pub(super) requirement_type: Option<String>,
    #[serde(default)]
    pub(super) source: Option<String>,
    #[serde(default)]
    pub(super) acceptance_criterion: Vec<String>,
    #[serde(default)]
    pub(super) input_json: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct RequirementUpdateInput {
    pub(super) id: String,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default, rename = "type")]
    pub(super) requirement_type: Option<String>,
    #[serde(default)]
    pub(super) source: Option<String>,
    #[serde(default)]
    pub(super) linked_task_id: Vec<String>,
    #[serde(default)]
    pub(super) acceptance_criterion: Vec<String>,
    #[serde(default)]
    pub(super) replace_acceptance_criteria: bool,
    #[serde(default)]
    pub(super) input_json: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct RequirementDeleteInput {
    pub(super) id: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct RequirementRefineInput {
    #[serde(default, rename = "id")]
    pub(super) requirement_ids: Vec<String>,
    #[serde(default)]
    pub(super) focus: Option<String>,
    #[serde(default)]
    pub(super) use_ai: Option<bool>,
    #[serde(default)]
    pub(super) tool: Option<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) start_runner: Option<bool>,
    #[serde(default)]
    pub(super) input_json: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}
