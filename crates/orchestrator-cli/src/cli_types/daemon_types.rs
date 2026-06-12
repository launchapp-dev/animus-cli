use clap::{ArgAction, Args, Subcommand};

use super::{parse_positive_u64, parse_positive_usize, LogArgs};

#[derive(Debug, Subcommand)]
pub(crate) enum DaemonCommand {
    /// Start the daemon in detached/background mode.
    Start(DaemonStartArgs),
    /// Run the daemon in the current foreground process.
    Run(DaemonRunArgs),
    /// Stop the running daemon.
    Stop(DaemonStopArgs),
    /// Stop the running daemon (graceful), then start it again with the
    /// supplied start flags. Starts the daemon even when it is not running.
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
    Events(DaemonEventsArgs),
    /// Read daemon logs.
    Logs(LogArgs),
    /// Stream structured log events in real-time across daemon, workflows, and runs.
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
    #[arg(long, default_value_t = false, help = "Do not auto-start the runner process.")]
    pub(crate) skip_runner: bool,
    #[arg(long, default_value_t = false, help = "Run daemon in detached/background mode.")]
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
    /// (e.g. `animus plugin update --restart-daemon`): detached/background
    /// mode with every scheduler override left to persisted config.
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
            autonomous: true,
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
}
