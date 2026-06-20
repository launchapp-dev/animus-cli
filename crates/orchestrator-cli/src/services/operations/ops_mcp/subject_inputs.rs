use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectListInput {
    pub(super) kind: String,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) limit: Option<u32>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectGetInput {
    pub(super) kind: String,
    pub(super) id: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectCreateInput {
    pub(super) kind: String,
    pub(super) title: String,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) labels: Vec<String>,
    #[serde(default)]
    pub(super) body: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectUpdateInput {
    pub(super) kind: String,
    pub(super) id: String,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) labels: Vec<String>,
    /// Free-form body / description (markdown). Set this to write long-form
    /// content onto the subject (e.g. an agent's findings) — it renders in
    /// detail views.
    #[serde(default)]
    pub(super) body: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectBatchCreateItem {
    pub(super) title: String,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) labels: Vec<String>,
    #[serde(default)]
    pub(super) body: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectBatchCreateInput {
    pub(super) kind: String,
    pub(super) items: Vec<SubjectBatchCreateItem>,
    #[serde(default)]
    pub(super) on_error: OnError,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectBatchUpdateItem {
    pub(super) id: String,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) labels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectBatchUpdateInput {
    pub(super) kind: String,
    pub(super) items: Vec<SubjectBatchUpdateItem>,
    #[serde(default)]
    pub(super) on_error: OnError,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectNextInput {
    pub(super) kind: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectStatusInput {
    pub(super) kind: String,
    pub(super) id: String,
    pub(super) status: String,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}
