use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubjectListInput {
    pub(super) kind: String,
    #[serde(default)]
    pub(super) status: Option<String>,
    /// Max subjects per page. Defaults to a bounded page; pass 0 to remove the
    /// per-page cap. The result carries `total` + `next_cursor`.
    #[serde(default)]
    pub(super) limit: Option<u32>,
    /// Opaque cursor from a prior result's `next_cursor`, to fetch the next page.
    #[serde(default)]
    pub(super) cursor: Option<String>,
    /// Case-insensitive substring filter on the subject title — look a subject up
    /// by name (e.g. a repo's owner/repo) without paging. The full set is fetched
    /// and filtered, so the match is found even on a large board.
    #[serde(default)]
    pub(super) query: Option<String>,
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
