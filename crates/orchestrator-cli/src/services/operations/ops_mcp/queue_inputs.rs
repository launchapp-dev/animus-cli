use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct QueueEnqueueInput {
    /// DEPRECATED: use `subject_id` instead (a task is `task:TASK-001`);
    /// `subject_id` is the single dispatch selector. Still accepted and resolves
    /// exactly as before, but will be removed in a future release.
    #[serde(default)]
    pub(super) task_id: Option<String>,
    /// DEPRECATED: use `subject_id` instead (a requirement is
    /// `requirement:REQ-042`); `subject_id` is the single dispatch selector.
    /// Still accepted and resolves exactly as before, but will be removed in a
    /// future release.
    #[serde(default)]
    pub(super) requirement_id: Option<String>,
    #[serde(default)]
    pub(super) title: Option<String>,
    /// For subjects that are NOT kind=task/requirement (BaaS dynamic kinds like
    /// blog/post/etc.), pass `subject_id` (the kernel resolves the kind)
    /// instead of `task_id`. Accepts a qualified id (`blog:BLOG-001` — kind
    /// trusted) or a bare id (`BLOG-001` — kind resolved via the subject
    /// router). Mutually exclusive with `task_id` / `requirement_id` / `title`.
    #[serde(default)]
    pub(super) subject_id: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) workflow_ref: Option<String>,
    #[serde(default)]
    pub(super) input_json: Option<String>,
    /// Defer dispatch until this time: an RFC 3339 timestamp or a relative
    /// offset (`90s`, `30m`, `2h`, `3d`). The entry stays queued but is not
    /// leased until then. Omit to dispatch as soon as capacity allows.
    #[serde(default)]
    pub(super) run_at: Option<String>,
    /// Grace window after `run_at` (e.g. `10m`, `1h`; bare number = seconds).
    /// A still-pending deferred entry past `run_at + expire_after` is dropped
    /// instead of dispatched late. Requires `run_at`.
    #[serde(default)]
    pub(super) expire_after: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct QueueSubjectInput {
    #[serde(default)]
    pub(super) subject_id: Option<String>,
    #[serde(default)]
    pub(super) subject_ids: Vec<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct QueueReorderInput {
    pub(super) subject_ids: Vec<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}
