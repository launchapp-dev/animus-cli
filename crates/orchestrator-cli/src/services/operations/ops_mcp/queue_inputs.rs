use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(super) struct QueueEnqueueInput {
    #[serde(default)]
    pub(super) title: Option<String>,
    /// Subject to enqueue for any kind (task, requirement, or dynamic kinds like
    /// blog/post). Accepts a qualified id (`task:TASK-001` / `blog:BLOG-001` —
    /// kind trusted) or a bare id (`TASK-001` — kind resolved via the subject
    /// router). Mutually exclusive with `title`.
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
    /// Stable producer key for durable enqueue. Exact retries return the
    /// original receipt; changed content with the same key fails closed.
    #[serde(default)]
    pub(super) idempotency_key: Option<String>,
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
