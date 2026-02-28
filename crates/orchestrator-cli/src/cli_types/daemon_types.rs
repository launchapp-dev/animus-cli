use clap::{ArgAction, Args, Subcommand};

use super::{parse_positive_u64, parse_positive_usize, LogArgs, RunnerScopeArg};

#[derive(Debug, Subcommand)]
pub(crate) enum DaemonCommand {
    /// Start the daemon in detached/background mode.
    Start(DaemonStartArgs),
    /// Run the daemon in the current foreground process.
    Run(DaemonRunArgs),
    /// Stop the running daemon.
    Stop,
    /// Show daemon runtime status.
    Status,
    /// Show daemon health diagnostics.
    Health,
    /// Pause daemon scheduling.
    Pause,
    /// Resume daemon scheduling.
    Resume,
    /// Stream or tail daemon event history.
    Events(DaemonEventsArgs),
    /// Read daemon logs.
    Logs(LogArgs),
    /// Clear daemon logs.
    ClearLogs,
    /// List daemon-managed agents.
    Agents,
    /// Update daemon automation configuration.
    Config(DaemonConfigArgs),
}

#[derive(Debug, Args)]
pub(crate) struct DaemonStartArgs {
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of concurrent agents (agent pool size)."
    )]
    pub(crate) pool_size: Option<usize>,
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of concurrent agents (alias for --pool-size, deprecated)."
    )]
    pub(crate) max_agents: Option<usize>,
    #[arg(
        long,
        default_value_t = false,
        help = "Do not auto-start the runner process."
    )]
    pub(crate) skip_runner: bool,
    #[arg(
        long,
        value_name = "SCOPE",
        help = "Runner config scope: project or global."
    )]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
    #[arg(
        long,
        default_value_t = false,
        help = "Run daemon in detached/background mode."
    )]
    pub(crate) autonomous: bool,
    #[arg(
        long,
        value_name = "SECONDS",
        default_value_t = 5,
        value_parser = parse_positive_u64,
        help = "Housekeeping timer interval in seconds (agent scheduling is reactive)."
    )]
    pub(crate) interval_secs: u64,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Enable AI-generated task creation."
    )]
    pub(crate) ai_task_generation: bool,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Automatically run ready tasks."
    )]
    pub(crate) auto_run_ready: bool,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Override auto-merge behavior for daemon runs."
    )]
    pub(crate) auto_merge: Option<bool>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Override auto-PR behavior for daemon runs."
    )]
    pub(crate) auto_pr: Option<bool>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Override auto-commit-before-merge behavior for daemon runs."
    )]
    pub(crate) auto_commit_before_merge: Option<bool>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Override automatic pruning of completed task worktrees after successful merges."
    )]
    pub(crate) auto_prune_worktrees_after_merge: Option<bool>,
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
        help = "Reconcile stale task/workflow runtime state."
    )]
    pub(crate) reconcile_stale: bool,
    #[arg(
        long,
        value_name = "HOURS",
        default_value_t = 24,
        value_parser = parse_positive_u64,
        help = "Flag in-progress tasks as stale when updated_at age is at least this many hours."
    )]
    pub(crate) stale_threshold_hours: u64,
    #[arg(
        long,
        value_name = "COUNT",
        default_value_t = 2,
        value_parser = parse_positive_usize,
        help = "Maximum task workflows or phases to execute per scheduler tick. (Deprecated: use --pool-size / --max-agents to control agent concurrency)."
    )]
    pub(crate) max_tasks_per_tick: usize,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Override phase timeout in seconds."
    )]
    pub(crate) phase_timeout_secs: Option<u64>,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Override workflow idle timeout in seconds."
    )]
    pub(crate) idle_timeout_secs: Option<u64>,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonRunArgs {
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of concurrent agents (agent pool size)."
    )]
    pub(crate) pool_size: Option<usize>,
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of concurrent agents (alias for --pool-size, deprecated)."
    )]
    pub(crate) max_agents: Option<usize>,
    #[arg(
        long,
        value_name = "SECONDS",
        default_value_t = 5,
        value_parser = parse_positive_u64,
        help = "Housekeeping timer interval in seconds (agent scheduling is reactive)."
    )]
    pub(crate) interval_secs: u64,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Enable AI-generated task creation."
    )]
    pub(crate) ai_task_generation: bool,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Automatically run ready tasks."
    )]
    pub(crate) auto_run_ready: bool,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Override auto-merge behavior for this daemon run."
    )]
    pub(crate) auto_merge: Option<bool>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Override auto-PR behavior for this daemon run."
    )]
    pub(crate) auto_pr: Option<bool>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Override auto-commit-before-merge behavior for this daemon run."
    )]
    pub(crate) auto_commit_before_merge: Option<bool>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Override automatic pruning of completed task worktrees after successful merges."
    )]
    pub(crate) auto_prune_worktrees_after_merge: Option<bool>,
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
        help = "Reconcile stale task/workflow runtime state."
    )]
    pub(crate) reconcile_stale: bool,
    #[arg(
        long,
        value_name = "HOURS",
        default_value_t = 24,
        value_parser = parse_positive_u64,
        help = "Flag in-progress tasks as stale when updated_at age is at least this many hours."
    )]
    pub(crate) stale_threshold_hours: u64,
    #[arg(
        long,
        value_name = "COUNT",
        default_value_t = 2,
        value_parser = parse_positive_usize,
        help = "Maximum task workflows or phases to execute per scheduler tick. (Deprecated: use --pool-size / --max-agents to control agent concurrency)."
    )]
    pub(crate) max_tasks_per_tick: usize,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Override phase timeout in seconds."
    )]
    pub(crate) phase_timeout_secs: Option<u64>,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Override workflow idle timeout in seconds."
    )]
    pub(crate) idle_timeout_secs: Option<u64>,
    #[arg(
        long,
        default_value_t = false,
        help = "Run one scheduler tick and exit."
    )]
    pub(crate) once: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonConfigArgs {
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Persist auto-merge daemon configuration."
    )]
    pub(crate) auto_merge: Option<bool>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Persist auto-PR daemon configuration."
    )]
    pub(crate) auto_pr: Option<bool>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Persist auto-commit-before-merge daemon configuration."
    )]
    pub(crate) auto_commit_before_merge: Option<bool>,
    #[arg(
        long,
        action = ArgAction::Set,
        help = "Persist automatic pruning of completed task worktrees after successful merges."
    )]
    pub(crate) auto_prune_worktrees_after_merge: Option<bool>,
    #[arg(long, value_name = "JSON")]
    pub(crate) notification_config_json: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub(crate) notification_config_file: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_notification_config: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonEventsArgs {
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of recent events to print before follow mode."
    )]
    pub(crate) limit: Option<usize>,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Continue streaming new events until interrupted."
    )]
    pub(crate) follow: bool,
}
