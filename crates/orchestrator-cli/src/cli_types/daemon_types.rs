use clap::{ArgAction, Args, Subcommand};

use super::{parse_positive_u64, parse_positive_usize, LogArgs};

#[derive(Debug, Subcommand)]
pub(crate) enum DaemonCommand {
    /// Start the daemon as a detached background process.
    Start(DaemonStartArgs),
    /// Run the daemon in the current foreground process (dev/debug).
    Run(DaemonRunArgs),
    /// Stop the running daemon.
    Stop(DaemonStopArgs),
    /// Stop the running daemon (graceful), then start it again detached
    /// with the supplied start flags. Starts the daemon even when it is
    /// not running.
    Restart(DaemonRestartArgs),
    /// Show daemon runtime status.
    Status,
    /// Show daemon health diagnostics.
    Health,
    /// Pause daemon scheduling.
    Pause,
    /// Resume daemon scheduling.
    Resume,
    /// Print recent daemon event history; pass --follow to stream.
    ///
    /// Scoped to the current project root by default; pass --all-projects for
    /// the fleet-wide view across every project on this host.
    ///
    /// Not sure which surface you need? Run `animus daemon observe` for the
    /// routing matrix.
    Events(DaemonEventsArgs),
    /// Read daemon logs.
    ///
    /// Not sure which surface you need? Run `animus daemon observe` for the
    /// routing matrix.
    Logs(LogArgs),
    /// Stream structured log events in real-time across daemon, workflows, and runs.
    ///
    /// Not sure which surface you need? Run `animus daemon observe` for the
    /// routing matrix.
    Stream(DaemonStreamArgs),
    /// Clear daemon logs.
    ClearLogs,
    /// List daemon-managed agents.
    Agents,
    /// Update daemon automation configuration.
    Config(DaemonConfigArgs),
    /// Report plugin preflight status (which required plugins are installed,
    /// which are missing, and the fix commands).
    Preflight(DaemonPreflightArgs),
    /// Print daemon observability metrics (counters, gauges, histograms);
    /// subcommands manage opt-in anonymous usage telemetry.
    Metrics(DaemonMetricsCommandArgs),
    /// One observability front-door: routes to the right log/event surface.
    /// Bare invocation prints a data-source matrix plus a recent merged tail;
    /// flags delegate to the existing `events`/`logs`/`stream` handlers.
    Observe(DaemonObserveArgs),
}

/// Which underlying observability surface `daemon observe --source` routes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ObserveSource {
    /// Daemon event history (`daemon events`): queue/workflow lifecycle records.
    Events,
    /// Daemon/workflow/run structured logs (`daemon logs`).
    Logs,
    /// Live structured log stream (`daemon stream`).
    Stream,
    /// Workflow lifecycle events filtered to a workflow (`daemon stream`).
    Workflow,
}

impl ObserveSource {
    /// The kebab-case `--source` token a user types for this variant, so
    /// error messages echo the user's input (`stream`) rather than the Rust
    /// enum's `Debug` form (`Stream`).
    pub(crate) fn as_token(self) -> &'static str {
        match self {
            ObserveSource::Events => "events",
            ObserveSource::Logs => "logs",
            ObserveSource::Stream => "stream",
            ObserveSource::Workflow => "workflow",
        }
    }
}

/// `daemon observe` is a routing front-door, not a new data path. It reuses the
/// `events` / `logs` / `stream` handlers; no flag introduces its own reader.
#[derive(Debug, Args)]
pub(crate) struct DaemonObserveArgs {
    #[arg(
        long,
        action = ArgAction::SetTrue,
        help = "Follow live: delegate to the `daemon stream --pretty` handler."
    )]
    pub(crate) follow: bool,
    #[arg(
        long,
        value_name = "DURATION",
        help = "Recent window (e.g. 15m, 2h, 1d): merge daemon events + logs chronologically, labeling each line's source."
    )]
    pub(crate) since: Option<String>,
    #[arg(
        long,
        value_enum,
        value_name = "SOURCE",
        help = "Route to a specific existing surface: logs | events | stream | workflow."
    )]
    pub(crate) source: Option<ObserveSource>,
    #[arg(
        long,
        value_name = "ID",
        help = "Scope to a workflow ID/ref where the underlying surface supports filtering."
    )]
    pub(crate) workflow: Option<String>,
    #[arg(
        long,
        value_name = "COUNT",
        default_value_t = 20,
        value_parser = parse_positive_usize,
        help = "Number of recent merged lines to show in the bare/window views."
    )]
    pub(crate) limit: usize,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonSchedulerArgs {
    #[arg(
        long,
        visible_alias = "max-agents",
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of concurrent agents (agent pool size)."
    )]
    pub(crate) pool_size: Option<usize>,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Fallback heartbeat sweep interval in seconds. Dispatch is event-driven (nudges, cron deadlines, completions); this only bounds how long out-of-band state edits wait and paces housekeeping. When omitted, the daemon uses persisted config or built-in defaults."
    )]
    pub(crate) interval_secs: Option<u64>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Enable or disable automatic dispatch of ready tasks. When omitted, the daemon uses persisted config or workflow YAML defaults."
    )]
    pub(crate) auto_run_ready: Option<bool>,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Run startup cleanup before scheduling."
    )]
    pub(crate) startup_cleanup: bool,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Attempt to resume interrupted workflows."
    )]
    pub(crate) resume_interrupted: bool,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "On startup, recover tasks stuck in-progress and workflow runs left behind by an interrupted daemon."
    )]
    pub(crate) reconcile_stale: bool,
    #[arg(
        long,
        value_name = "HOURS",
        value_parser = parse_positive_u64,
        help = "Treat a task as stuck when it has been in-progress without updates for at least this many hours. When omitted, the daemon uses persisted config or built-in defaults."
    )]
    pub(crate) stale_threshold_hours: Option<u64>,
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum new workflows to dispatch per scheduler tick. When omitted, the daemon uses persisted config or built-in defaults."
    )]
    pub(crate) max_tasks_per_tick: Option<usize>,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Override phase timeout in seconds."
    )]
    pub(crate) phase_timeout_secs: Option<u64>,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonStartArgs {
    #[command(flatten)]
    pub(crate) scheduler: DaemonSchedulerArgs,
    #[arg(
        long,
        hide = true,
        default_value_t = false,
        help = "Deprecated no-op (the agent-runner sidecar was removed in v0.5.3; provider plugins handle CLI invocation)."
    )]
    pub(crate) skip_runner: bool,
    #[arg(
        long,
        hide = true,
        default_value_t = false,
        help = "Deprecated no-op: detached/background mode is now the default for `daemon start`. Use `daemon run` for foreground."
    )]
    pub(crate) autonomous: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Auto-install any plugins that preflight finds missing, using the daemon's recommended defaults."
    )]
    pub(crate) auto_install: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Skip the daemon's startup plugin preflight entirely. Escape hatch for dev iteration."
    )]
    pub(crate) skip_preflight: bool,
}

impl DaemonStartArgs {
    /// Start flags used when the restart is initiated programmatically
    /// (e.g. `animus plugin update --restart-daemon`): every scheduler
    /// override left to persisted config. `daemon start` always detaches,
    /// so no detach flag is needed.
    pub(crate) fn detached_defaults() -> Self {
        Self {
            scheduler: DaemonSchedulerArgs {
                pool_size: None,
                interval_secs: None,
                auto_run_ready: None,
                startup_cleanup: true,
                resume_interrupted: true,
                reconcile_stale: true,
                stale_threshold_hours: None,
                max_tasks_per_tick: None,
                phase_timeout_secs: None,
            },
            skip_runner: false,
            autonomous: false,
            auto_install: false,
            skip_preflight: false,
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct DaemonRestartArgs {
    #[command(flatten)]
    pub(crate) start: DaemonStartArgs,
    #[arg(
        long,
        value_name = "SECONDS",
        default_value_t = 60,
        value_parser = parse_positive_u64,
        help = "Maximum seconds to wait for in-flight agents to finish before force-stopping the old daemon."
    )]
    pub(crate) shutdown_timeout_secs: u64,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonRunArgs {
    #[command(flatten)]
    pub(crate) scheduler: DaemonSchedulerArgs,
    #[arg(long, hide = true, default_value_t = false)]
    pub(crate) skip_runner: bool,
    #[arg(long, default_value_t = false, help = "Run one scheduler tick and exit.")]
    pub(crate) once: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Auto-install any plugins that preflight finds missing, using the daemon's recommended defaults."
    )]
    pub(crate) auto_install: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Skip the daemon's startup plugin preflight entirely. Escape hatch for dev iteration."
    )]
    pub(crate) skip_preflight: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonPreflightArgs {
    #[arg(
        long,
        default_value_t = false,
        help = "Auto-install any plugins that preflight finds missing, using the daemon's recommended defaults."
    )]
    pub(crate) auto_install: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonConfigArgs {
    // Runtime-reconfigurable settings (hot-reloaded by daemon without restart)
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Set agent pool size (max concurrent agents). Hot-reloaded by running daemon."
    )]
    pub(crate) pool_size: Option<usize>,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Set the fallback heartbeat sweep interval in seconds (dispatch is event-driven; this paces housekeeping). Hot-reloaded by running daemon."
    )]
    pub(crate) interval_secs: Option<u64>,
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Set max new workflows to dispatch per scheduler tick (queue cap). Hot-reloaded by running daemon."
    )]
    pub(crate) max_tasks_per_tick: Option<usize>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Enable or disable automatic dispatch of ready tasks."
    )]
    pub(crate) auto_run_ready: Option<bool>,
    #[arg(
        long,
        value_name = "HOURS",
        value_parser = parse_positive_u64,
        help = "Set stale-task threshold in hours."
    )]
    pub(crate) stale_threshold_hours: Option<u64>,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Set phase timeout override in seconds."
    )]
    pub(crate) phase_timeout_secs: Option<u64>,
    #[arg(
        long,
        value_name = "MINUTES",
        help = "Minutes an in-progress phase may produce no output before its agent is marked SILENT in the dashboard. Set 0 to disable. Hot-reloaded by running daemon."
    )]
    pub(crate) silent_threshold_mins: Option<u64>,
    #[arg(long, value_name = "JSON")]
    pub(crate) notification_config_json: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub(crate) notification_config_file: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_notification_config: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonStopArgs {
    #[arg(
        long,
        value_name = "SECONDS",
        default_value_t = 60,
        value_parser = parse_positive_u64,
        help = "Maximum seconds to wait for in-flight agents to finish before force-stopping."
    )]
    pub(crate) shutdown_timeout_secs: u64,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonStreamArgs {
    #[arg(long, help = "Filter by category prefix (e.g. 'llm', 'schedule', 'phase').")]
    pub(crate) cat: Option<String>,
    #[arg(long, help = "Minimum log level: debug, info, warn, error.")]
    pub(crate) level: Option<String>,
    #[arg(long, help = "Filter to a specific workflow ID or workflow ref.")]
    pub(crate) workflow: Option<String>,
    #[arg(long, help = "Filter to a specific run ID.")]
    pub(crate) run: Option<String>,
    #[arg(long, default_value_t = 20, help = "Number of recent entries to show before streaming.")]
    pub(crate) tail: usize,
    #[arg(long, action = ArgAction::SetTrue, help = "Print recent entries and exit without streaming.")]
    pub(crate) no_follow: bool,
    #[arg(long, action = ArgAction::SetTrue, help = "Pretty-print with colors and formatting instead of raw JSON.")]
    pub(crate) pretty: bool,
    #[arg(
        long,
        action = ArgAction::SetTrue,
        help = "In pretty mode, render full message bodies (LLM output, command stdout) as formatted markdown blocks instead of a truncated preview."
    )]
    pub(crate) full: bool,
}

/// Bare `animus daemon metrics` keeps the live-counters display (the common
/// read path); the subcommands control opt-in anonymous usage telemetry
/// (folded in from the deleted top-level `animus metrics` group in v0.6).
#[derive(Debug, Args)]
pub(crate) struct DaemonMetricsCommandArgs {
    #[command(subcommand)]
    pub(crate) command: Option<DaemonMetricsSubcommand>,
    #[command(flatten)]
    pub(crate) display: DaemonMetricsArgs,
}

#[derive(Debug, Subcommand)]
pub(crate) enum DaemonMetricsSubcommand {
    /// Show the current opt-in state, install id, and pending event count.
    Status,
    /// Opt in to anonymous usage telemetry (skips re-prompting on first run).
    Enable,
    /// Opt out of anonymous usage telemetry. Drops any buffered events.
    Disable,
    /// Force-send any buffered events to the configured endpoint. Debug helper.
    Flush,
    /// Sweep every repo-scoped metrics dir for orphaned/oversized `flushing-*`
    /// snapshots and oversized `pending.jsonl`, reclaiming disk. Safe to run any
    /// time — guards against the runaway buffer that once grew to multi-GB.
    Cleanup,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonMetricsArgs {
    #[arg(long, action = ArgAction::SetTrue, help = "Continuously refresh and reprint the snapshot.")]
    pub(crate) watch: bool,
    #[arg(
        long,
        value_name = "SECONDS",
        default_value_t = 5,
        value_parser = parse_positive_u64,
        help = "Refresh interval in seconds when --watch is set."
    )]
    pub(crate) interval_secs: u64,
    #[arg(long, action = ArgAction::SetTrue, help = "Render a human-readable table instead of raw JSON.")]
    pub(crate) pretty: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonEventsArgs {
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of recent events to print (the initial batch when --follow is set)."
    )]
    pub(crate) limit: Option<usize>,
    #[arg(
        long,
        action = ArgAction::Set,
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        help = "Continue streaming new events until interrupted; without it the command prints and exits."
    )]
    pub(crate) follow: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Show events for every project root on this host instead of just the current project."
    )]
    pub(crate) all_projects: bool,
}
