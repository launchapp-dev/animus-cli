use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct DaemonStartInput {
    #[serde(default)]
    pub(super) pool_size: Option<usize>,
    #[serde(default)]
    pub(super) max_agents: Option<usize>,
    #[serde(default)]
    pub(super) interval_secs: Option<u64>,
    #[serde(default)]
    pub(super) stale_threshold_hours: Option<u64>,
    #[serde(default)]
    pub(super) max_tasks_per_tick: Option<usize>,
    #[serde(default)]
    pub(super) phase_timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) skip_runner: Option<bool>,
    #[serde(default)]
    pub(super) autonomous: Option<bool>,
    #[serde(default)]
    pub(super) auto_run_ready: Option<bool>,
    #[serde(default)]
    pub(super) auto_merge: Option<bool>,
    #[serde(default)]
    pub(super) auto_pr: Option<bool>,
    #[serde(default)]
    pub(super) auto_commit_before_merge: Option<bool>,
    #[serde(default)]
    pub(super) auto_prune_worktrees_after_merge: Option<bool>,
    #[serde(default)]
    pub(super) startup_cleanup: Option<bool>,
    #[serde(default)]
    pub(super) resume_interrupted: Option<bool>,
    #[serde(default)]
    pub(super) reconcile_stale: Option<bool>,
    #[serde(default)]
    pub(super) runner_scope: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct DaemonEventsInput {
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct DaemonLogsInput {
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) search: Option<String>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct DaemonConfigInput {
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct DaemonConfigSetInput {
    #[serde(default)]
    pub(super) auto_merge: Option<bool>,
    #[serde(default)]
    pub(super) auto_pr: Option<bool>,
    #[serde(default)]
    pub(super) auto_commit_before_merge: Option<bool>,
    #[serde(default)]
    pub(super) auto_prune_worktrees_after_merge: Option<bool>,
    #[serde(default)]
    pub(super) auto_run_ready: Option<bool>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}
