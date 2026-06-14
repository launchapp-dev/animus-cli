use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct DaemonStartInput {
    #[serde(default, alias = "max_agents")]
    pub(super) pool_size: Option<usize>,
    #[serde(default)]
    pub(super) interval_secs: Option<u64>,
    #[serde(default)]
    pub(super) stale_threshold_hours: Option<u64>,
    #[serde(default)]
    pub(super) max_tasks_per_tick: Option<usize>,
    #[serde(default)]
    pub(super) phase_timeout_secs: Option<u64>,
    /// Deprecated no-op: `daemon start` always detaches into the background.
    #[serde(default)]
    pub(super) autonomous: Option<bool>,
    #[serde(default)]
    pub(super) startup_cleanup: Option<bool>,
    #[serde(default)]
    pub(super) resume_interrupted: Option<bool>,
    #[serde(default)]
    pub(super) reconcile_stale: Option<bool>,
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
pub(super) struct DaemonObserveInput {
    /// Recent window (e.g. `15m`, `2h`, `1d`): merge daemon events + logs
    /// chronologically. Omit for the bare data-source matrix + recent tail.
    #[serde(default)]
    pub(super) since: Option<String>,
    /// Route to a specific existing surface: `logs` | `events` | `stream` |
    /// `workflow`. Omit to merge events + logs.
    #[serde(default)]
    pub(super) source: Option<String>,
    /// Scope to a workflow ID/ref where the underlying surface supports it.
    #[serde(default)]
    pub(super) workflow_id: Option<String>,
    /// Number of recent merged lines to show. Defaults to the CLI default (20).
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub(super) struct DaemonConfigSetInput {
    // Runtime-reconfigurable settings (hot-reloaded by daemon without restart)
    #[serde(default, alias = "max_agents")]
    pub(super) pool_size: Option<usize>,
    #[serde(default)]
    pub(super) interval_secs: Option<u64>,
    #[serde(default)]
    pub(super) max_tasks_per_tick: Option<usize>,
    #[serde(default)]
    pub(super) stale_threshold_hours: Option<u64>,
    #[serde(default)]
    pub(super) phase_timeout_secs: Option<u64>,
    #[serde(default)]
    pub(super) notification_config_json: Option<String>,
    #[serde(default)]
    pub(super) notification_config_file: Option<String>,
    #[serde(default)]
    pub(super) clear_notification_config: bool,
    #[serde(default)]
    pub(super) project_root: Option<String>,
}
