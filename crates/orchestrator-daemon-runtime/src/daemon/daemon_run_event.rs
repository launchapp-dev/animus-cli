use crate::ProjectTickSummary;

#[derive(Debug, Clone)]
pub struct DiscoveredPluginSummary {
    pub name: String,
    pub version: String,
    pub plugin_kind: String,
    pub source: &'static str,
    pub path: String,
}

#[derive(Debug, Clone)]
pub enum DaemonRunEvent {
    Startup {
        project_root: String,
        daemon_pid: u32,
    },
    Status {
        project_root: String,
        status: String,
    },
    StartupCleanup {
        project_root: String,
    },
    PluginsDiscovered {
        project_root: String,
        plugins: Vec<DiscoveredPluginSummary>,
    },
    PluginsDiscoveryFailed {
        project_root: String,
        error: String,
    },
    OrphanDetection {
        project_root: String,
        orphaned_workflows_recovered: usize,
    },
    YamlCompileSucceeded {
        project_root: String,
        source_files: usize,
        output_path: String,
        phase_definitions: usize,
        agent_profiles: usize,
    },
    YamlCompileFailed {
        project_root: String,
        error: String,
    },
    TickSummary {
        summary: ProjectTickSummary,
    },
    TickError {
        project_root: String,
        message: String,
    },
    GracefulShutdown {
        project_root: String,
        timeout_secs: Option<u64>,
    },
    Draining {
        project_root: String,
        trigger: String,
    },
    NotificationRuntimeError {
        project_root: Option<String>,
        stage: String,
        message: String,
    },
    ConfigReloaded {
        project_root: String,
        setting: String,
    },
    TriggerPluginsStarted {
        project_root: String,
        plugin_count: usize,
    },
    TriggerPluginStartFailed {
        project_root: String,
        plugin_name: String,
        error: String,
    },
    TriggerPluginEvent {
        project_root: String,
        plugin_name: String,
        event_id: String,
        trigger_id: Option<String>,
        routed: bool,
    },
    TriggerPluginRestart {
        project_root: String,
        plugin_name: String,
        attempt: u32,
        delay_ms: u64,
    },
    TriggerPluginCrashed {
        project_root: String,
        plugin_name: String,
        attempts: u32,
        error: String,
    },
    /// Resolved which log storage backend the daemon will route through
    /// for this run. Emitted once at startup, after plugin discovery
    /// completes. `plugin_name` is `None` when the in-tree fallback is
    /// active.
    LogStorageDispatchResolved {
        project_root: String,
        plugin_name: Option<String>,
        candidate_count: usize,
        disable_env_set: bool,
        warnings: Vec<String>,
    },
    /// Resolved which subject-backend plugins the daemon will route
    /// `<kind>/<verb>` calls through for this run. Emitted once at
    /// startup, after subject-plugin discovery completes. `plugin_count`
    /// is the number of installed subject_backend plugins that
    /// initialized successfully; `kinds` summarizes the manifest-declared
    /// subject_kind capabilities seen across them. Unlike log storage,
    /// there is no in-tree fallback — when `plugin_count == 0`, every
    /// `animus subject <verb>` call returns `NotFound` with the missing
    /// kind named.
    SubjectRouterResolved {
        project_root: String,
        plugin_count: usize,
        kinds: Vec<String>,
        disable_env_set: bool,
        warnings: Vec<String>,
    },
    /// Resolved the daemon's control RPC server for this run.
    ///
    /// Emitted once at startup, after the InProcessSurface is built and
    /// either bound to a Unix socket or skipped due to
    /// `ANIMUS_DAEMON_DISABLE_CONTROL_SERVER` being set. `socket_path`
    /// carries the bound path so operators (and downstream CLI / MCP
    /// migrations in C6 / C7 / C8) can locate the server in
    /// `events.jsonl`. `disable_env_set = true` means the server was
    /// deliberately not started; in that case `socket_path` is the path
    /// that *would* have been used.
    ControlServerResolved {
        project_root: String,
        socket_path: std::path::PathBuf,
        disable_env_set: bool,
        warnings: Vec<String>,
    },
    Shutdown {
        project_root: String,
        daemon_pid: u32,
    },
    PluginPreflight {
        project_root: String,
        satisfied: Vec<String>,
        auto_installed: Vec<String>,
        missing: Vec<String>,
        skipped: bool,
        auto_install: bool,
    },
    /// v0.5 P2 #6.2: startup-time agent orphan scan summary. Emitted once
    /// per daemon start after the existing workflow orphan-recovery pass,
    /// before the daemon begins dispatching new work. Carries one
    /// `OrphanAgentDetected` / `OrphanAgentCleanup` / `OrphanAgentRecordUnparseable`
    /// child event already emitted via [`DaemonRunEvent::OrphanAgentDetected`],
    /// [`DaemonRunEvent::OrphanAgentCleanup`], and
    /// [`DaemonRunEvent::OrphanAgentRecordUnparseable`].
    OrphanAgentScan {
        project_root: String,
        detected_count: usize,
        cleaned_count: usize,
        unparseable_count: usize,
        unix_scan_supported: bool,
    },
    OrphanAgentDetected {
        project_root: String,
        agent_session_id: String,
        pid: u32,
        subject_id: String,
        subject_kind: String,
        workflow_ref: String,
        task_id: Option<String>,
        command_line: Vec<String>,
        started_at: String,
        record_path: String,
    },
    OrphanAgentCleanup {
        project_root: String,
        agent_session_id: String,
        pid: u32,
        record_path: String,
    },
    OrphanAgentRecordUnparseable {
        project_root: String,
        record_path: String,
        error: String,
    },
    /// v0.5.1 P2 #6.2 round-3: the daemon successfully reconnected to a
    /// detected orphan's reattach socket and is now forwarding the
    /// runner's workflow_events into the in-process broadcaster. Subscribers
    /// resume receiving phase-boundary events for this run.
    OrphanAgentReattached {
        project_root: String,
        agent_session_id: String,
        pid: u32,
        socket_path: String,
    },
    /// v0.5.1 P2 #6.2 round-3: the daemon found a live orphan with a
    /// `stdio_socket_path` but could not connect to it. The orphan is still
    /// running and its decisions.jsonl continues to grow; operators can
    /// inspect the log directly or wait for the next daemon restart to
    /// retry. Common causes: runner not yet bound (race between daemon
    /// start and runner bind), socket file permissions, or non-Unix host.
    OrphanAgentReattachFailed {
        project_root: String,
        agent_session_id: String,
        pid: u32,
        socket_path: Option<String>,
        error: String,
    },
}
