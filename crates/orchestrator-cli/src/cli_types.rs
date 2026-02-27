use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

const INPUT_JSON_PRECEDENCE_HELP: &str =
    "JSON payload for this command. When provided, values in this payload override individual CLI flags.";
const TASK_TYPE_HELP: &str =
    "Task type: feature|bugfix|hotfix|refactor|docs|test|chore|experiment.";
const TASK_TYPE_FILTER_HELP: &str =
    "Task type filter: feature|bugfix|hotfix|refactor|docs|test|chore|experiment.";
const TASK_STATUS_HELP: &str =
    "Task status: backlog|todo|ready|in-progress|in_progress|blocked|on-hold|on_hold|done|cancelled.";
const TASK_STATUS_FILTER_HELP: &str =
    "Status filter: backlog|todo|ready|in-progress|in_progress|blocked|on-hold|on_hold|done|cancelled.";
const TASK_PRIORITY_HELP: &str = "Task priority: critical|high|medium|low.";
const TASK_PRIORITY_FILTER_HELP: &str = "Priority filter: critical|high|medium|low.";
const REQUIREMENT_PRIORITY_HELP: &str = "Requirement priority: must|should|could|wont|won't.";
const REQUIREMENT_STATUS_HELP: &str =
    "Requirement status: draft|refined|planned|in-progress|in_progress|done.";
const DEPENDENCY_TYPE_HELP: &str =
    "Dependency type: blocks-by|blocks_by|blocked-by|blocked_by|related-to|related_to.";

fn parse_positive_u64(value: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| "must be a whole number".to_string())?;
    if parsed == 0 {
        return Err("must be greater than 0".to_string());
    }
    Ok(parsed)
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| "must be a whole number".to_string())?;
    if parsed == 0 {
        return Err("must be greater than 0".to_string());
    }
    Ok(parsed)
}

#[derive(Debug, Parser)]
#[command(name = "ao", about = "Agent Orchestrator CLI", version)]
pub(crate) struct Cli {
    #[arg(
        long,
        global = true,
        help = "Emit machine-readable JSON output using the ao.cli.v1 envelope."
    )]
    pub(crate) json: bool,
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Project root directory. Overrides PROJECT_ROOT and default root resolution."
    )]
    pub(crate) project_root: Option<String>,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Show the installed `ao` version.
    Version,
    /// Manage daemon lifecycle and automation settings.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Run and inspect agent executions.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Manage project registration and metadata.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Manage tasks, dependencies, and status.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Run and control workflow execution.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Draft and refine project vision artifacts.
    Vision {
        #[command(subcommand)]
        command: VisionCommand,
    },
    /// Draft and manage project requirements.
    Requirements {
        #[command(subcommand)]
        command: RequirementsCommand,
    },
    /// Manage architecture entities, edges, and graph metadata.
    Architecture {
        #[command(subcommand)]
        command: ArchitectureCommand,
    },
    /// Generate or execute task plans from requirements.
    Execute {
        #[command(subcommand)]
        command: ExecuteCommand,
    },
    /// Planning facade for vision and requirements workflows.
    Planning {
        #[command(subcommand)]
        command: PlanningCommand,
    },
    /// Record and inspect review decisions and handoffs.
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
    /// Run and inspect QA evaluations and approvals.
    Qa {
        #[command(subcommand)]
        command: QaCommand,
    },
    /// Inspect and search execution history.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Inspect and retry recorded operational errors.
    Errors {
        #[command(subcommand)]
        command: ErrorsCommand,
    },
    /// Apply operational task controls such as cancel, priority, and deadline.
    TaskControl {
        #[command(subcommand)]
        command: TaskControlCommand,
    },
    /// Manage Git repositories, worktrees, and confirmation requests.
    Git {
        #[command(subcommand)]
        command: GitCommand,
    },
    /// Search, install, update, and publish versioned skills.
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    /// Inspect model availability, validation, and evaluations.
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// Inspect runner health and orphaned runs.
    Runner {
        #[command(subcommand)]
        command: RunnerCommand,
    },
    /// Inspect run output and artifacts.
    Output {
        #[command(subcommand)]
        command: OutputCommand,
    },
    /// Run the AO MCP service endpoint.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Serve and open the AO web UI.
    Web {
        #[command(subcommand)]
        command: WebCommand,
    },
    /// Guided onboarding and configuration wizard.
    Setup(SetupArgs),
    /// Launch the terminal UI.
    Tui(TuiArgs),
    /// Run environment and configuration diagnostics.
    Doctor(DoctorArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    /// Start the MCP server in the current process.
    Serve,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WebCommand {
    /// Start the AO web server.
    Serve(WebServeArgs),
    /// Open the AO web UI URL in a browser.
    Open(WebOpenArgs),
}

#[derive(Debug, Args)]
pub(crate) struct WebServeArgs {
    #[arg(
        long,
        value_name = "HOST",
        default_value = "127.0.0.1",
        help = "Host interface to bind the web server."
    )]
    pub(crate) host: String,
    #[arg(
        long,
        value_name = "PORT",
        default_value_t = 4173,
        help = "Port to bind the web server."
    )]
    pub(crate) port: u16,
    #[arg(
        long,
        default_value_t = false,
        help = "Open the web UI in a browser after startup."
    )]
    pub(crate) open: bool,
    #[arg(long, value_name = "PATH", help = "Override static assets directory.")]
    pub(crate) assets_dir: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Serve API endpoints only without static assets."
    )]
    pub(crate) api_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WebOpenArgs {
    #[arg(
        long,
        value_name = "HOST",
        default_value = "127.0.0.1",
        help = "Host name for the web URL."
    )]
    pub(crate) host: String,
    #[arg(
        long,
        value_name = "PORT",
        default_value_t = 4173,
        help = "Port for the web URL."
    )]
    pub(crate) port: u16,
    #[arg(
        long,
        value_name = "PATH",
        default_value = "/",
        help = "Path to open, such as / or /runs."
    )]
    pub(crate) path: String,
}

#[derive(Debug, Args)]
pub(crate) struct TuiArgs {
    #[arg(
        long,
        value_name = "MODEL_ID",
        help = "Model id to use for interactive runs."
    )]
    pub(crate) model: Option<String>,
    #[arg(
        long,
        value_name = "TOOL",
        help = "CLI provider, such as codex or claude."
    )]
    pub(crate) tool: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Run without full-screen UI rendering."
    )]
    pub(crate) headless: bool,
    #[arg(
        long,
        value_name = "TEXT",
        help = "Optional initial prompt to pre-fill in the UI."
    )]
    pub(crate) prompt: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SetupArgs {
    #[arg(
        long,
        help = "Run without prompts. Requires explicit --auto-merge, --auto-pr, and --auto-commit-before-merge values."
    )]
    pub(crate) non_interactive: bool,
    #[arg(long, help = "Preview setup changes without writing config.")]
    pub(crate) plan: bool,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_merge: Option<bool>,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_pr: Option<bool>,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_commit_before_merge: Option<bool>,
    #[arg(long, help = "Apply safe doctor remediations after setup changes.")]
    pub(crate) doctor_fix: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    #[arg(long, help = "Apply safe local remediations for doctor findings.")]
    pub(crate) fix: bool,
}

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

#[derive(Debug, Subcommand)]
pub(crate) enum AgentCommand {
    /// Start an agent run.
    Run(AgentRunArgs),
    /// Control an existing agent run.
    Control(AgentControlArgs),
    /// Read status for a run id.
    Status(AgentStatusArgs),
    /// Check model availability/status through the runner.
    ModelStatus(AgentModelStatusArgs),
    /// Inspect runner process availability.
    RunnerStatus(AgentRunnerStatusArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentRunArgs {
    #[arg(
        long,
        value_name = "RUN_ID",
        help = "Run identifier. Omit to auto-generate a UUID."
    )]
    pub(crate) run_id: Option<String>,
    #[arg(
        long,
        value_name = "TOOL",
        default_value = "codex",
        help = "CLI provider to execute, for example codex or claude."
    )]
    pub(crate) tool: String,
    #[arg(
        long,
        value_name = "MODEL",
        default_value = "codex",
        help = "Model identifier passed to the selected tool."
    )]
    pub(crate) model: String,
    #[arg(long, value_name = "TEXT", help = "Prompt text to send to the agent.")]
    pub(crate) prompt: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Working directory for the run. Must resolve inside the project root."
    )]
    pub(crate) cwd: Option<String>,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        help = "Run timeout in seconds."
    )]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, value_name = "JSON", help = "Agent context JSON object.")]
    pub(crate) context_json: Option<String>,
    #[arg(long, value_name = "JSON", help = "Runtime contract JSON override.")]
    pub(crate) runtime_contract_json: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Submit run and return immediately without streaming events."
    )]
    pub(crate) detach: bool,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Stream run events to stdout."
    )]
    pub(crate) stream: bool,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Persist run event logs under .ao/runs."
    )]
    pub(crate) save_jsonl: bool,
    #[arg(
        long,
        value_name = "PATH",
        help = "Override the base directory used for persisted run logs."
    )]
    pub(crate) jsonl_dir: Option<String>,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Start the runner automatically when required."
    )]
    pub(crate) start_runner: bool,
    #[arg(
        long,
        value_enum,
        value_name = "SCOPE",
        help = "Runner config scope: project or global."
    )]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentControlArgs {
    #[arg(long, value_name = "RUN_ID", help = "Run identifier.")]
    pub(crate) run_id: String,
    #[arg(
        long,
        value_enum,
        value_name = "ACTION",
        help = "Control action: pause, resume, or terminate."
    )]
    pub(crate) action: AgentControlActionArg,
    #[arg(
        long,
        default_value_t = false,
        help = "Start the runner automatically when required."
    )]
    pub(crate) start_runner: bool,
    #[arg(
        long,
        value_enum,
        value_name = "SCOPE",
        help = "Runner config scope: project or global."
    )]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum AgentControlActionArg {
    Pause,
    Resume,
    Terminate,
}

#[derive(Debug, Args)]
pub(crate) struct AgentStatusArgs {
    #[arg(long, value_name = "RUN_ID", help = "Run identifier.")]
    pub(crate) run_id: String,
    #[arg(
        long,
        value_name = "PATH",
        help = "Override the base directory used to read persisted run logs."
    )]
    pub(crate) jsonl_dir: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Start the runner automatically when required."
    )]
    pub(crate) start_runner: bool,
    #[arg(
        long,
        value_enum,
        value_name = "SCOPE",
        help = "Runner config scope: project or global."
    )]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentModelStatusArgs {
    #[arg(
        long = "model",
        value_name = "MODEL_ID",
        help = "Model identifier to check. Repeat to query multiple models."
    )]
    pub(crate) models: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Start the runner automatically when required."
    )]
    pub(crate) start_runner: bool,
    #[arg(
        long,
        value_enum,
        value_name = "SCOPE",
        help = "Runner config scope: project or global."
    )]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Debug, Args)]
pub(crate) struct AgentRunnerStatusArgs {
    #[arg(
        long,
        default_value_t = false,
        help = "Start the runner automatically when required."
    )]
    pub(crate) start_runner: bool,
    #[arg(
        long,
        value_enum,
        value_name = "SCOPE",
        help = "Runner config scope: project or global."
    )]
    pub(crate) runner_scope: Option<RunnerScopeArg>,
}

#[derive(Debug, Args)]
pub(crate) struct DaemonStartArgs {
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of concurrent agents."
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
        value_enum,
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
        help = "Scheduler interval in seconds."
    )]
    pub(crate) interval_secs: u64,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Include projects from the global daemon registry."
    )]
    pub(crate) include_registry: bool,
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
        help = "Reconcile daemon/project registry entries before scheduling."
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
        value_name = "COUNT",
        default_value_t = 2,
        value_parser = parse_positive_usize,
        help = "Maximum task workflows or phases to execute per scheduler tick."
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
        value_name = "SECONDS",
        default_value_t = 5,
        value_parser = parse_positive_u64,
        help = "Scheduler interval in seconds."
    )]
    pub(crate) interval_secs: u64,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Include projects from the global daemon registry."
    )]
    pub(crate) include_registry: bool,
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
        help = "Reconcile daemon/project registry entries before scheduling."
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
        value_name = "COUNT",
        default_value_t = 2,
        value_parser = parse_positive_usize,
        help = "Maximum task workflows or phases to execute per scheduler tick."
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

#[derive(Debug, Args)]
pub(crate) struct LogArgs {
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of recent log lines to return."
    )]
    pub(crate) limit: Option<usize>,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum RunnerScopeArg {
    Project,
    Global,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProjectCommand {
    /// List registered projects.
    List,
    /// Show the active project.
    Active,
    /// Get a project by id.
    Get(IdArgs),
    /// Create a new project entry.
    Create(ProjectCreateArgs),
    /// Mark a project as active.
    Load(IdArgs),
    /// Rename a project.
    Rename(ProjectRenameArgs),
    /// Archive a project.
    Archive(IdArgs),
    /// Remove a project.
    Remove(IdArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ProjectCreateArgs {
    #[arg(long, value_name = "NAME", help = "Human-friendly project name.")]
    pub(crate) name: String,
    #[arg(
        long,
        value_name = "PATH",
        help = "Filesystem path to the project root."
    )]
    pub(crate) path: String,
    #[arg(
        long,
        value_name = "TYPE",
        help = "Project type: web-app|mobile-app|desktop-app|full-stack-platform|library|infrastructure|other (aliases accepted)."
    )]
    pub(crate) project_type: Option<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct IdArgs {
    #[arg(long, value_name = "ID", help = "Entity identifier.")]
    pub(crate) id: String,
}

#[derive(Debug, Args)]
pub(crate) struct ProjectRenameArgs {
    #[arg(long, value_name = "ID", help = "Project identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "NAME", help = "Updated project name.")]
    pub(crate) name: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TaskCommand {
    /// List tasks with optional filters.
    List(TaskListArgs),
    /// List tasks sorted by priority/urgency.
    Prioritized,
    /// Get the next ready task.
    Next,
    /// Show task statistics.
    Stats,
    /// Get a task by id.
    Get(IdArgs),
    /// Create a task.
    Create(TaskCreateArgs),
    /// Update a task.
    Update(TaskUpdateArgs),
    /// Delete a task (confirmation required).
    Delete(TaskDeleteArgs),
    /// Assign a generic assignee string to a task.
    Assign(TaskAssignArgs),
    /// Assign an agent role to a task.
    AssignAgent(TaskAssignAgentArgs),
    /// Assign a human user to a task.
    AssignHuman(TaskAssignHumanArgs),
    /// Add a checklist item.
    ChecklistAdd(TaskChecklistAddArgs),
    /// Mark a checklist item complete/incomplete.
    ChecklistUpdate(TaskChecklistUpdateArgs),
    /// Add a task dependency edge.
    DependencyAdd(TaskDependencyAddArgs),
    /// Remove a task dependency edge.
    DependencyRemove(TaskDependencyRemoveArgs),
    /// Set task status.
    Status(TaskStatusArgs),
}

#[derive(Debug, Args)]
pub(crate) struct TaskListArgs {
    #[arg(long, value_name = "TYPE", help = TASK_TYPE_FILTER_HELP)]
    pub(crate) task_type: Option<String>,
    #[arg(long, value_name = "STATUS", help = TASK_STATUS_FILTER_HELP)]
    pub(crate) status: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_FILTER_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(
        long,
        value_name = "ASSIGNEE_TYPE",
        help = "Assignee type filter: agent|human|unassigned."
    )]
    pub(crate) assignee_type: Option<String>,
    #[arg(
        long,
        value_name = "TAG",
        help = "Match tasks that include all provided tags. Repeat to require multiple tags."
    )]
    pub(crate) tag: Vec<String>,
    #[arg(
        long,
        value_name = "REQ_ID",
        help = "Filter tasks linked to a requirement id."
    )]
    pub(crate) linked_requirement: Option<String>,
    #[arg(
        long,
        value_name = "ENTITY_ID",
        help = "Filter tasks linked to an architecture entity id."
    )]
    pub(crate) linked_architecture_entity: Option<String>,
    #[arg(
        long,
        value_name = "TEXT",
        help = "Case-insensitive text search over task title and description."
    )]
    pub(crate) search: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskCreateArgs {
    #[arg(long, value_name = "TITLE", help = "Task title.")]
    pub(crate) title: String,
    #[arg(
        long,
        value_name = "TEXT",
        default_value = "",
        help = "Task description."
    )]
    pub(crate) description: String,
    #[arg(long, value_name = "TYPE", help = TASK_TYPE_HELP)]
    pub(crate) task_type: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(
        long = "linked-architecture-entity",
        value_name = "ENTITY_ID",
        help = "Link architecture entity ids to the new task. Repeat to add multiple ids."
    )]
    pub(crate) linked_architecture_entity: Vec<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskUpdateArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TITLE", help = "Updated task title.")]
    pub(crate) title: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Updated task description.")]
    pub(crate) description: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(long, value_name = "STATUS", help = TASK_STATUS_HELP)]
    pub(crate) status: Option<String>,
    #[arg(
        long,
        value_name = "ASSIGNEE",
        help = "Updated assignee value for the task."
    )]
    pub(crate) assignee: Option<String>,
    #[arg(
        long = "linked-architecture-entity",
        value_name = "ENTITY_ID",
        help = "Architecture entity ids to link. Repeat to add multiple ids."
    )]
    pub(crate) linked_architecture_entity: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace all linked architecture entities with the provided --linked-architecture-entity values."
    )]
    pub(crate) replace_linked_architecture_entities: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDeleteArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "TASK_ID",
        help = "Confirmation token; must match --id."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview deletion payload without mutating task state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TaskAssignArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "ASSIGNEE", help = "Assignee value.")]
    pub(crate) assignee: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskAssignAgentArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "ROLE", help = "Agent role identifier.")]
    pub(crate) role: String,
    #[arg(long, value_name = "MODEL", help = "Optional model override.")]
    pub(crate) model: Option<String>,
    #[arg(
        long,
        value_name = "USER",
        default_value = "ao-cli",
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskAssignHumanArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "USER_ID", help = "Human user id.")]
    pub(crate) user_id: String,
    #[arg(
        long,
        value_name = "USER",
        default_value = "ao-cli",
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskChecklistAddArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TEXT", help = "Checklist item text.")]
    pub(crate) description: String,
    #[arg(
        long,
        value_name = "USER",
        default_value = "ao-cli",
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskChecklistUpdateArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "ITEM_ID", help = "Checklist item identifier.")]
    pub(crate) item_id: String,
    #[arg(long, help = "Set to true to mark complete, false to mark incomplete.")]
    pub(crate) completed: bool,
    #[arg(
        long,
        value_name = "USER",
        default_value = "ao-cli",
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDependencyAddArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "DEPENDENCY_ID",
        help = "Dependency task identifier."
    )]
    pub(crate) dependency_id: String,
    #[arg(long, value_name = "TYPE", help = DEPENDENCY_TYPE_HELP)]
    pub(crate) dependency_type: String,
    #[arg(
        long,
        value_name = "USER",
        default_value = "ao-cli",
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDependencyRemoveArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "DEPENDENCY_ID",
        help = "Dependency task identifier."
    )]
    pub(crate) dependency_id: String,
    #[arg(
        long,
        value_name = "USER",
        default_value = "ao-cli",
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskStatusArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "STATUS", help = TASK_STATUS_HELP)]
    pub(crate) status: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowCommand {
    /// List workflows.
    List,
    /// Get workflow details.
    Get(IdArgs),
    /// Show workflow decisions.
    Decisions(IdArgs),
    /// List and inspect workflow checkpoints.
    Checkpoints {
        #[command(subcommand)]
        command: WorkflowCheckpointCommand,
    },
    /// Start a workflow for a task.
    Run(WorkflowRunArgs),
    /// Resume a paused workflow.
    Resume(IdArgs),
    /// Check whether a workflow can be resumed.
    ResumeStatus(IdArgs),
    /// Pause an active workflow (confirmation required).
    Pause(WorkflowPauseArgs),
    /// Cancel a workflow (confirmation required).
    Cancel(WorkflowCancelArgs),
    /// Manual actions for a specific workflow phase.
    Phase {
        #[command(subcommand)]
        command: WorkflowPhaseCommand,
    },
    /// Manage workflow phase definitions.
    Phases {
        #[command(subcommand)]
        command: WorkflowPhasesCommand,
    },
    /// Manage workflow pipeline definitions.
    Pipelines {
        #[command(subcommand)]
        command: WorkflowPipelinesCommand,
    },
    /// Read and validate workflow configuration.
    Config {
        #[command(subcommand)]
        command: WorkflowConfigCommand,
    },
    /// Read and update workflow state machine configuration.
    StateMachine {
        #[command(subcommand)]
        command: WorkflowStateMachineCommand,
    },
    /// Read and update workflow agent runtime configuration.
    AgentRuntime {
        #[command(subcommand)]
        command: WorkflowAgentRuntimeCommand,
    },
    /// Update a pipeline by id.
    UpdatePipeline(WorkflowPipelineUpdateArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPhaseCommand {
    /// Approve a pending phase gate.
    Approve(WorkflowPhaseApproveArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPhasesCommand {
    /// List configured workflow phases.
    List,
    /// Get a workflow phase by id.
    Get(WorkflowPhaseGetArgs),
    /// Create or replace a workflow phase definition.
    Upsert(WorkflowPhaseUpsertArgs),
    /// Remove a workflow phase definition (confirmation required).
    Remove(WorkflowPhaseRemoveArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowPipelinesCommand {
    /// List configured workflow pipelines.
    List,
    /// Create or replace a workflow pipeline definition.
    Upsert(WorkflowPipelineUpsertArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowConfigCommand {
    /// Read resolved workflow config.
    Get,
    /// Validate workflow config shape and references.
    Validate,
    /// Migrate legacy workflow config to v2.
    MigrateV2,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowStateMachineCommand {
    /// Read workflow state-machine config.
    Get,
    /// Validate workflow state-machine config.
    Validate,
    /// Replace workflow state-machine config JSON.
    Set(WorkflowStateMachineSetArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowAgentRuntimeCommand {
    /// Read workflow agent-runtime config.
    Get,
    /// Validate workflow agent-runtime config.
    Validate,
    /// Replace workflow agent-runtime config JSON.
    Set(WorkflowAgentRuntimeSetArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum VisionCommand {
    /// Draft a project vision.
    Draft(VisionDraftArgs),
    /// Refine the existing project vision.
    Refine(VisionRefineArgs),
    /// Read the current project vision.
    Get,
}

#[derive(Debug, Args)]
pub(crate) struct VisionDraftArgs {
    #[arg(long)]
    pub(crate) project_name: Option<String>,
    #[arg(long, default_value = "")]
    pub(crate) problem: String,
    #[arg(long)]
    pub(crate) target_user: Vec<String>,
    #[arg(long)]
    pub(crate) goal: Vec<String>,
    #[arg(long)]
    pub(crate) constraint: Vec<String>,
    #[arg(long)]
    pub(crate) value_proposition: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) use_ai_complexity: bool,
    #[arg(long, default_value = "codex")]
    pub(crate) tool: String,
    #[arg(long, default_value = "gpt-5.3-codex")]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) allow_heuristic_fallback: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct VisionRefineArgs {
    #[arg(long)]
    pub(crate) focus: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) use_ai: bool,
    #[arg(long, default_value = "codex")]
    pub(crate) tool: String,
    #[arg(long, default_value = "gpt-5.3-codex")]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) allow_heuristic_fallback: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) preserve_core: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RequirementsCommand {
    /// Draft requirements from project context.
    Draft(RequirementsDraftArgs),
    /// List requirements.
    List,
    /// Get a requirement by id.
    Get(IdArgs),
    /// Refine existing requirements.
    Refine(RequirementsRefineArgs),
    /// Create a requirement.
    Create(RequirementCreateArgs),
    /// Update a requirement.
    Update(RequirementUpdateArgs),
    /// Delete a requirement.
    Delete(IdArgs),
    /// View or replace the requirement dependency graph.
    Graph {
        #[command(subcommand)]
        command: RequirementGraphCommand,
    },
    /// Manage requirement mockups and linked assets.
    Mockups {
        #[command(subcommand)]
        command: MockupCommand,
    },
    /// Scan and apply requirement recommendations.
    Recommendations {
        #[command(subcommand)]
        command: RecommendationCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ArchitectureCommand {
    /// Read architecture graph and metadata.
    Get,
    /// Replace architecture graph JSON.
    Set(ArchitectureSetArgs),
    /// Suggest architecture links for a task.
    Suggest(ArchitectureSuggestArgs),
    /// Manage architecture entities.
    Entity {
        #[command(subcommand)]
        command: ArchitectureEntityCommand,
    },
    /// Manage architecture edges.
    Edge {
        #[command(subcommand)]
        command: ArchitectureEdgeCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ArchitectureSetArgs {
    #[arg(long)]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct ArchitectureSuggestArgs {
    #[arg(long)]
    pub(crate) task_id: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ArchitectureEntityCommand {
    /// List architecture entities.
    List,
    /// Get an architecture entity by id.
    Get(IdArgs),
    /// Create an architecture entity.
    Create(ArchitectureEntityCreateArgs),
    /// Update an architecture entity.
    Update(ArchitectureEntityUpdateArgs),
    /// Delete an architecture entity.
    Delete(IdArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ArchitectureEntityCreateArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) kind: Option<String>,
    #[arg(long)]
    pub(crate) description: Option<String>,
    #[arg(long = "code-path")]
    pub(crate) code_path: Vec<String>,
    #[arg(long = "tag")]
    pub(crate) tag: Vec<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ArchitectureEntityUpdateArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) name: Option<String>,
    #[arg(long)]
    pub(crate) kind: Option<String>,
    #[arg(long)]
    pub(crate) description: Option<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) clear_description: bool,
    #[arg(long = "code-path")]
    pub(crate) code_path: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) replace_code_paths: bool,
    #[arg(long = "tag")]
    pub(crate) tag: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) replace_tags: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ArchitectureEdgeCommand {
    /// List architecture edges.
    List,
    /// Create an architecture edge.
    Create(ArchitectureEdgeCreateArgs),
    /// Delete an architecture edge.
    Delete(IdArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ArchitectureEdgeCreateArgs {
    #[arg(long)]
    pub(crate) id: Option<String>,
    #[arg(long)]
    pub(crate) from: String,
    #[arg(long)]
    pub(crate) to: String,
    #[arg(long)]
    pub(crate) relation: String,
    #[arg(long)]
    pub(crate) rationale: Option<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementsDraftArgs {
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) include_codebase_scan: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) append_only: bool,
    #[arg(long, default_value_t = 0)]
    pub(crate) max_requirements: usize,
    #[arg(long, default_value = "single-agent")]
    pub(crate) draft_strategy: String,
    #[arg(long, default_value_t = 1)]
    pub(crate) po_parallelism: usize,
    #[arg(long, default_value_t = 2)]
    pub(crate) quality_repair_attempts: usize,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) allow_heuristic_complexity: bool,
    #[arg(long, default_value = "codex")]
    pub(crate) tool: String,
    #[arg(long, default_value = "gpt-5.3-codex")]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementsRefineArgs {
    #[arg(long = "id")]
    pub(crate) requirement_ids: Vec<String>,
    #[arg(long)]
    pub(crate) focus: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) use_ai: bool,
    #[arg(long, default_value = "codex")]
    pub(crate) tool: String,
    #[arg(long, default_value = "gpt-5.3-codex")]
    pub(crate) model: String,
    #[arg(long)]
    pub(crate) timeout_secs: Option<u64>,
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub(crate) start_runner: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementCreateArgs {
    #[arg(long, value_name = "TITLE", help = "Requirement title.")]
    pub(crate) title: String,
    #[arg(
        long,
        value_name = "TEXT",
        default_value = "",
        help = "Requirement description."
    )]
    pub(crate) description: String,
    #[arg(long, value_name = "PRIORITY", help = REQUIREMENT_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(
        long,
        value_name = "SOURCE",
        help = "Optional source describing where this requirement originated."
    )]
    pub(crate) source: Option<String>,
    #[arg(
        long = "acceptance-criterion",
        value_name = "TEXT",
        help = "Acceptance criteria text. Repeat to provide multiple criteria."
    )]
    pub(crate) acceptance_criterion: Vec<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequirementUpdateArgs {
    #[arg(long, value_name = "REQ_ID", help = "Requirement identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TITLE", help = "Updated requirement title.")]
    pub(crate) title: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Updated requirement description.")]
    pub(crate) description: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = REQUIREMENT_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(long, value_name = "STATUS", help = REQUIREMENT_STATUS_HELP)]
    pub(crate) status: Option<String>,
    #[arg(
        long,
        value_name = "SOURCE",
        help = "Updated source describing where this requirement originated."
    )]
    pub(crate) source: Option<String>,
    #[arg(
        long = "linked-task-id",
        value_name = "TASK_ID",
        help = "Task ids linked to this requirement. Repeat to add multiple ids."
    )]
    pub(crate) linked_task_id: Vec<String>,
    #[arg(
        long = "acceptance-criterion",
        value_name = "TEXT",
        help = "Acceptance criteria text. Repeat to provide multiple criteria."
    )]
    pub(crate) acceptance_criterion: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace all acceptance criteria with the provided --acceptance-criterion values."
    )]
    pub(crate) replace_acceptance_criteria: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RequirementGraphCommand {
    /// Read the requirement graph.
    Get,
    /// Replace the requirement graph with provided JSON.
    Save(RequirementGraphSaveArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RequirementGraphSaveArgs {
    #[arg(
        long,
        value_name = "JSON",
        help = "Complete requirement graph JSON payload to persist."
    )]
    pub(crate) input_json: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum MockupCommand {
    /// List requirement mockups.
    List,
    /// Create a mockup record.
    Create(MockupCreateArgs),
    /// Link a mockup to requirements or flows.
    Link(MockupLinkArgs),
    /// Get a mockup file by relative path.
    GetFile(MockupFileArgs),
}

#[derive(Debug, Args)]
pub(crate) struct MockupCreateArgs {
    #[arg(long)]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) description: Option<String>,
    #[arg(long)]
    pub(crate) mockup_type: Option<String>,
    #[arg(long = "requirement-id")]
    pub(crate) requirement_id: Vec<String>,
    #[arg(long = "flow-id")]
    pub(crate) flow_id: Vec<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct MockupLinkArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long = "requirement-id")]
    pub(crate) requirement_id: Vec<String>,
    #[arg(long = "flow-id")]
    pub(crate) flow_id: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct MockupFileArgs {
    #[arg(long)]
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) relative_path: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RecommendationCommand {
    /// Run recommendation scan over current project context.
    Scan(RecommendationScanArgs),
    /// List saved recommendation reports.
    List,
    /// Apply a recommendation report.
    Apply(RecommendationApplyArgs),
    /// Read recommendation config.
    ConfigGet,
    /// Update recommendation config.
    ConfigUpdate(RecommendationConfigUpdateArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RecommendationScanArgs {
    #[arg(long)]
    pub(crate) mode: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RecommendationApplyArgs {
    #[arg(long)]
    pub(crate) report_id: String,
    #[arg(long)]
    pub(crate) mode: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RecommendationConfigUpdateArgs {
    #[arg(long)]
    pub(crate) mode: Option<String>,
    #[arg(long)]
    pub(crate) enabled: Option<bool>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ExecuteCommand {
    /// Generate task execution plan from requirements.
    Plan(ExecutePlanArgs),
    /// Generate and immediately run workflows from requirements.
    Run(ExecuteRunArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ExecutePlanArgs {
    #[arg(long = "id")]
    pub(crate) requirement_ids: Vec<String>,
    #[arg(long)]
    pub(crate) pipeline_id: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) ai_task_generation: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) include_wont: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ExecuteRunArgs {
    #[arg(long = "id")]
    pub(crate) requirement_ids: Vec<String>,
    #[arg(long)]
    pub(crate) pipeline_id: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) ai_task_generation: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) include_wont: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PlanningCommand {
    /// Vision planning commands.
    Vision {
        #[command(subcommand)]
        command: PlanningVisionCommand,
    },
    /// Requirements planning commands.
    Requirements {
        #[command(subcommand)]
        command: PlanningRequirementsCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum PlanningVisionCommand {
    /// Draft a project vision.
    Draft(VisionDraftArgs),
    /// Refine existing project vision.
    Refine(VisionRefineArgs),
    /// Read current project vision.
    Get,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PlanningRequirementsCommand {
    /// Draft requirements from project context.
    Draft(RequirementsDraftArgs),
    /// List requirements.
    List,
    /// Get a requirement by id.
    Get(IdArgs),
    /// Refine requirements.
    Refine(RequirementsRefineArgs),
    /// Execute requirements planning into tasks/workflows.
    Execute(PlanningExecuteArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PlanningExecuteArgs {
    #[arg(long = "id")]
    pub(crate) requirement_ids: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub(crate) start_workflows: bool,
    #[arg(long)]
    pub(crate) pipeline_id: Option<String>,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) ai_task_generation: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) include_wont: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ReviewCommand {
    /// Compute review status for an entity.
    Entity(ReviewEntityArgs),
    /// Record a review decision.
    Record(ReviewRecordArgs),
    /// Compute review status for a task.
    TaskStatus(TaskIdArgs),
    /// Compute review status for a requirement.
    RequirementStatus(IdArgs),
    /// Record a handoff between roles for a run.
    Handoff(ReviewHandoffArgs),
    /// Record dual-approval for a task.
    DualApprove(ReviewDualApproveArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ReviewEntityArgs {
    #[arg(long)]
    pub(crate) entity_type: String,
    #[arg(long)]
    pub(crate) entity_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct ReviewRecordArgs {
    #[arg(long)]
    pub(crate) entity_type: String,
    #[arg(long)]
    pub(crate) entity_id: String,
    #[arg(long)]
    pub(crate) reviewer_role: String,
    #[arg(long)]
    pub(crate) decision: String,
    #[arg(long)]
    pub(crate) rationale: Option<String>,
    #[arg(long)]
    pub(crate) source: Option<String>,
    #[arg(long)]
    pub(crate) content_hash: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ReviewHandoffArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) target_role: String,
    #[arg(long)]
    pub(crate) question: String,
    #[arg(long)]
    pub(crate) context_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ReviewDualApproveArgs {
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) rationale: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum QaCommand {
    /// Evaluate QA gates for a workflow phase.
    Evaluate(QaEvaluateArgs),
    /// Get QA evaluation result for a workflow phase.
    Get(QaPhaseArgs),
    /// List QA evaluations for a workflow.
    List(QaWorkflowArgs),
    /// Manage QA approvals.
    Approval {
        #[command(subcommand)]
        command: QaApprovalCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct QaEvaluateArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
    #[arg(long)]
    pub(crate) phase_id: String,
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) worktree_path: Option<String>,
    #[arg(long)]
    pub(crate) gates_json: Option<String>,
    #[arg(long)]
    pub(crate) metrics_json: Option<String>,
    #[arg(long)]
    pub(crate) metadata_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct QaPhaseArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
    #[arg(long)]
    pub(crate) phase_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct QaWorkflowArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum QaApprovalCommand {
    /// Add a QA gate approval.
    Add(QaApprovalAddArgs),
    /// List QA gate approvals.
    List(QaApprovalListArgs),
}

#[derive(Debug, Args)]
pub(crate) struct QaApprovalAddArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
    #[arg(long)]
    pub(crate) phase_id: String,
    #[arg(long)]
    pub(crate) gate_id: String,
    #[arg(long)]
    pub(crate) approved_by: String,
    #[arg(long)]
    pub(crate) comment: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct QaApprovalListArgs {
    #[arg(long)]
    pub(crate) workflow_id: String,
    #[arg(long)]
    pub(crate) gate_id: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum HistoryCommand {
    /// List history records for a task.
    Task(HistoryTaskArgs),
    /// Get a history record by id.
    Get(IdArgs),
    /// List recent history records.
    Recent(HistoryRecentArgs),
    /// Search history records.
    Search(HistorySearchArgs),
    /// Remove old history records.
    Cleanup(HistoryCleanupArgs),
}

#[derive(Debug, Args)]
pub(crate) struct HistoryTaskArgs {
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct HistoryRecentArgs {
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct HistorySearchArgs {
    #[arg(long)]
    pub(crate) task_id: Option<String>,
    #[arg(long)]
    pub(crate) workflow_id: Option<String>,
    #[arg(long)]
    pub(crate) status: Option<String>,
    #[arg(long)]
    pub(crate) started_after: Option<String>,
    #[arg(long)]
    pub(crate) started_before: Option<String>,
    #[arg(long)]
    pub(crate) limit: Option<usize>,
    #[arg(long)]
    pub(crate) offset: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct HistoryCleanupArgs {
    #[arg(long, default_value_t = 30)]
    pub(crate) days: i64,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ErrorsCommand {
    /// List recorded errors.
    List(ErrorsListArgs),
    /// Get an error by id.
    Get(IdArgs),
    /// Show error summary stats.
    Stats,
    /// Retry an error by id.
    Retry(IdArgs),
    /// Remove old error records.
    Cleanup(ErrorsCleanupArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ErrorsListArgs {
    #[arg(long)]
    pub(crate) category: Option<String>,
    #[arg(long)]
    pub(crate) severity: Option<String>,
    #[arg(long)]
    pub(crate) task_id: Option<String>,
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct ErrorsCleanupArgs {
    #[arg(long, default_value_t = 30)]
    pub(crate) days: u32,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TaskControlCommand {
    /// Pause a task.
    Pause(TaskIdArgs),
    /// Resume a paused task.
    Resume(TaskIdArgs),
    /// Cancel a task (confirmation required).
    Cancel(TaskControlCancelArgs),
    /// Set task priority.
    SetPriority(TaskControlPriorityArgs),
    /// Set or clear task deadline.
    SetDeadline(TaskControlDeadlineArgs),
}

#[derive(Debug, Args)]
pub(crate) struct TaskIdArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) task_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskControlCancelArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) task_id: String,
    #[arg(
        long,
        value_name = "TASK_ID",
        help = "Confirmation token; must match --task-id."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview cancellation payload without mutating task state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TaskControlPriorityArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) task_id: String,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_HELP)]
    pub(crate) priority: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskControlDeadlineArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) task_id: String,
    #[arg(
        long,
        value_name = "RFC3339",
        help = "Deadline timestamp (RFC 3339), for example 2026-03-01T09:30:00Z."
    )]
    pub(crate) deadline: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitCommand {
    /// Manage repo registry entries.
    Repo {
        #[command(subcommand)]
        command: GitRepoCommand,
    },
    /// List repository branches.
    Branches(GitRepoArgs),
    /// Show repository status.
    Status(GitRepoArgs),
    /// Commit staged/untracked changes.
    Commit(GitCommitArgs),
    /// Push branch updates.
    Push(GitPushArgs),
    /// Pull branch updates.
    Pull(GitPullArgs),
    /// Manage git worktrees.
    Worktree {
        #[command(subcommand)]
        command: GitWorktreeCommand,
    },
    /// Manage confirmation requests/outcomes for destructive git operations.
    Confirm {
        #[command(subcommand)]
        command: GitConfirmCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitRepoCommand {
    /// List registered repositories.
    List,
    /// Get details for one repository.
    Get(GitRepoArgs),
    /// Initialize and register a local repository.
    Init(GitRepoInitArgs),
    /// Clone and register a repository.
    Clone(GitRepoCloneArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GitRepoArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitRepoInitArgs {
    #[arg(long, value_name = "NAME", help = "Repository registration name.")]
    pub(crate) name: String,
    #[arg(long, value_name = "PATH", help = "Optional filesystem path.")]
    pub(crate) path: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitRepoCloneArgs {
    #[arg(long, value_name = "URL", help = "Git clone URL.")]
    pub(crate) url: String,
    #[arg(long, value_name = "NAME", help = "Repository registration name.")]
    pub(crate) name: String,
    #[arg(long, value_name = "PATH", help = "Optional destination directory.")]
    pub(crate) path: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitCommitArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "TEXT", help = "Commit message.")]
    pub(crate) message: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitPushArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(
        long,
        value_name = "REMOTE",
        default_value = "origin",
        help = "Git remote name."
    )]
    pub(crate) remote: String,
    #[arg(
        long,
        value_name = "BRANCH",
        default_value = "main",
        help = "Branch to push."
    )]
    pub(crate) branch: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Force push (destructive and requires --confirmation-id)."
    )]
    pub(crate) force: bool,
    #[arg(
        long,
        value_name = "ID",
        help = "Approved confirmation id required for destructive git operations."
    )]
    pub(crate) confirmation_id: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview command payload without changing repository state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitPullArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(
        long,
        value_name = "REMOTE",
        default_value = "origin",
        help = "Git remote name."
    )]
    pub(crate) remote: String,
    #[arg(
        long,
        value_name = "BRANCH",
        default_value = "main",
        help = "Branch to pull."
    )]
    pub(crate) branch: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitWorktreeCommand {
    /// Create a repository worktree.
    Create(GitWorktreeCreateArgs),
    /// List repository worktrees.
    List(GitRepoArgs),
    /// Get one worktree by name.
    Get(GitWorktreeGetArgs),
    /// Remove a worktree (confirmation required).
    Remove(GitWorktreeRemoveArgs),
    /// Prune managed task worktrees for done/cancelled tasks.
    Prune(GitWorktreePruneArgs),
    /// Pull updates in a worktree.
    Pull(GitWorktreePullArgs),
    /// Push updates from a worktree.
    Push(GitWorktreePushArgs),
    /// Pull then push a worktree.
    Sync(GitWorktreeSyncArgs),
    /// Show synchronization status for a worktree.
    SyncStatus(GitWorktreeGetArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeCreateArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree registration name.")]
    pub(crate) worktree_name: String,
    #[arg(long, value_name = "PATH", help = "Filesystem path for the worktree.")]
    pub(crate) worktree_path: String,
    #[arg(
        long,
        value_name = "BRANCH",
        help = "Branch to check out in the worktree."
    )]
    pub(crate) branch: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Create the branch when it does not already exist."
    )]
    pub(crate) create_branch: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeGetArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree name.")]
    pub(crate) worktree_name: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeRemoveArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree name.")]
    pub(crate) worktree_name: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Force removal if the worktree is dirty."
    )]
    pub(crate) force: bool,
    #[arg(
        long,
        value_name = "ID",
        help = "Approved confirmation id required before removing a worktree."
    )]
    pub(crate) confirmation_id: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview command payload without changing repository state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreePruneArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Delete remote branches for pruned worktrees when branch metadata is available."
    )]
    pub(crate) delete_remote_branch: bool,
    #[arg(
        long,
        value_name = "REMOTE",
        default_value = "origin",
        help = "Git remote name used with --delete-remote-branch."
    )]
    pub(crate) remote: String,
    #[arg(
        long,
        value_name = "ID",
        help = "Approved confirmation id required before pruning worktrees."
    )]
    pub(crate) confirmation_id: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview prune actions without changing repository state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreePullArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree name.")]
    pub(crate) worktree_name: String,
    #[arg(
        long,
        value_name = "REMOTE",
        default_value = "origin",
        help = "Git remote name."
    )]
    pub(crate) remote: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreePushArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree name.")]
    pub(crate) worktree_name: String,
    #[arg(
        long,
        value_name = "REMOTE",
        default_value = "origin",
        help = "Git remote name."
    )]
    pub(crate) remote: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Force push (destructive and requires --confirmation-id)."
    )]
    pub(crate) force: bool,
    #[arg(
        long,
        value_name = "ID",
        help = "Approved confirmation id required for destructive git operations."
    )]
    pub(crate) confirmation_id: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview command payload without changing repository state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeSyncArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree name.")]
    pub(crate) worktree_name: String,
    #[arg(
        long,
        value_name = "REMOTE",
        default_value = "origin",
        help = "Git remote name."
    )]
    pub(crate) remote: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitConfirmCommand {
    /// Request a confirmation record for a destructive git operation.
    Request(GitConfirmRequestArgs),
    /// Approve or reject a confirmation request.
    Respond(GitConfirmRespondArgs),
    /// Record operation outcome for a confirmation request.
    Outcome(GitConfirmOutcomeArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GitConfirmRequestArgs {
    #[arg(
        long,
        value_name = "TYPE",
        help = "Operation type, for example force_push or remove_worktree."
    )]
    pub(crate) operation_type: String,
    #[arg(long, value_name = "REPO", help = "Repository name.")]
    pub(crate) repo_name: String,
    #[arg(long, value_name = "JSON", help = "Optional JSON context payload.")]
    pub(crate) context_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitConfirmRespondArgs {
    #[arg(long, value_name = "ID", help = "Confirmation request identifier.")]
    pub(crate) request_id: String,
    #[arg(long, help = "Set to true to approve, false to reject.")]
    pub(crate) approved: bool,
    #[arg(long, value_name = "TEXT", help = "Optional reviewer comment.")]
    pub(crate) comment: Option<String>,
    #[arg(long, value_name = "USER", help = "Reviewer user id.")]
    pub(crate) user_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitConfirmOutcomeArgs {
    #[arg(long, value_name = "ID", help = "Confirmation request identifier.")]
    pub(crate) request_id: String,
    #[arg(long, help = "Whether the operation succeeded.")]
    pub(crate) success: bool,
    #[arg(long, value_name = "TEXT", help = "Outcome message.")]
    pub(crate) message: String,
    #[arg(long, value_name = "JSON", help = "Optional JSON metadata payload.")]
    pub(crate) metadata_json: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillCommand {
    /// Search skill catalog entries.
    Search(SkillSearchArgs),
    /// Install a skill with deterministic resolution.
    Install(SkillInstallArgs),
    /// List installed skills and lock alignment.
    List,
    /// Re-resolve one or all installed skills.
    Update(SkillUpdateArgs),
    /// Publish a new skill version into the registry catalog.
    Publish(SkillPublishArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SkillSearchArgs {
    #[arg(long, help = "Match skill name text (case-insensitive).")]
    pub(crate) query: Option<String>,
    #[arg(long, help = "Filter by source identifier.")]
    pub(crate) source: Option<String>,
    #[arg(long, help = "Filter by registry identifier.")]
    pub(crate) registry: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SkillInstallArgs {
    #[arg(long, help = "Skill name to resolve and install.")]
    pub(crate) name: String,
    #[arg(long, help = "Optional version constraint (semver req).")]
    pub(crate) version: Option<String>,
    #[arg(long, help = "Optional source constraint.")]
    pub(crate) source: Option<String>,
    #[arg(long, help = "Optional registry constraint.")]
    pub(crate) registry: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Allow pre-release versions during resolution."
    )]
    pub(crate) allow_prerelease: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillUpdateArgs {
    #[arg(
        long,
        help = "Optional skill name target; omit to update all installed skills."
    )]
    pub(crate) name: Option<String>,
    #[arg(long, help = "Optional version constraint override (semver req).")]
    pub(crate) version: Option<String>,
    #[arg(long, help = "Optional source constraint override.")]
    pub(crate) source: Option<String>,
    #[arg(long, help = "Optional registry constraint override.")]
    pub(crate) registry: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Allow pre-release versions during resolution."
    )]
    pub(crate) allow_prerelease: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillPublishArgs {
    #[arg(long, help = "Skill package name.")]
    pub(crate) name: String,
    #[arg(long, help = "Skill semver version.")]
    pub(crate) version: String,
    #[arg(
        long,
        help = "Source identifier (for example local, github, internal)."
    )]
    pub(crate) source: String,
    #[arg(long, default_value = "project", help = "Target registry identifier.")]
    pub(crate) registry: String,
    #[arg(long, help = "Artifact reference (path, URI, or package id).")]
    pub(crate) artifact: Option<String>,
    #[arg(long, help = "Integrity hash; auto-derived when omitted.")]
    pub(crate) integrity: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelCommand {
    /// Check model availability for one or more model ids.
    Availability(ModelAvailabilityArgs),
    /// Show configured model and API-key status.
    Status(ModelStatusArgs),
    /// Validate model selection for a task or explicit list.
    Validate(ModelValidateArgs),
    /// Manage cached model roster metadata.
    Roster {
        #[command(subcommand)]
        command: ModelRosterCommand,
    },
    /// Run and inspect model evaluations.
    Eval {
        #[command(subcommand)]
        command: ModelEvalCommand,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ModelAvailabilityArgs {
    #[arg(long = "model")]
    pub(crate) model: Vec<String>,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ModelStatusArgs {
    #[arg(long)]
    pub(crate) model_id: String,
    #[arg(long)]
    pub(crate) cli_tool: String,
}

#[derive(Debug, Args)]
pub(crate) struct ModelValidateArgs {
    #[arg(long)]
    pub(crate) task_id: Option<String>,
    #[arg(long = "model")]
    pub(crate) model: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelRosterCommand {
    /// Refresh model roster from providers.
    Refresh,
    /// Get current model roster snapshot.
    Get,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelEvalCommand {
    /// Run model evaluation.
    Run(ModelEvalRunArgs),
    /// Show latest model evaluation report.
    Report,
}

#[derive(Debug, Args)]
pub(crate) struct ModelEvalRunArgs {
    #[arg(long = "model")]
    pub(crate) model: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RunnerCommand {
    /// Show runner process health.
    Health,
    /// Detect and clean orphaned runner processes.
    Orphans {
        #[command(subcommand)]
        command: RunnerOrphanCommand,
    },
    /// Show runner restart statistics.
    RestartStats,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RunnerOrphanCommand {
    /// Detect orphaned runner processes.
    Detect,
    /// Clean orphaned runner processes.
    Cleanup(RunnerOrphanCleanupArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RunnerOrphanCleanupArgs {
    #[arg(long = "run-id")]
    pub(crate) run_id: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum OutputCommand {
    /// Read run event payloads.
    Run(OutputRunArgs),
    /// List artifacts for an execution id.
    Artifacts(OutputArtifactsArgs),
    /// Download an artifact payload.
    Download(OutputDownloadArgs),
    /// List artifact file ids for an execution.
    Files(OutputFilesArgs),
    /// Read aggregated JSONL output streams for a run.
    Jsonl(OutputJsonlArgs),
    /// Inspect run output with optional task/phase filtering.
    Monitor(OutputMonitorArgs),
    /// Infer CLI provider details from run output.
    Cli(OutputCliArgs),
}

#[derive(Debug, Args)]
pub(crate) struct OutputRunArgs {
    #[arg(long)]
    pub(crate) run_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OutputArtifactsArgs {
    #[arg(long)]
    pub(crate) execution_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OutputDownloadArgs {
    #[arg(long)]
    pub(crate) execution_id: String,
    #[arg(long)]
    pub(crate) artifact_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OutputFilesArgs {
    #[arg(long)]
    pub(crate) execution_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OutputJsonlArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long, default_value_t = false)]
    pub(crate) entries: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OutputMonitorArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) task_id: Option<String>,
    #[arg(long)]
    pub(crate) phase_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct OutputCliArgs {
    #[arg(long)]
    pub(crate) run_id: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorkflowCheckpointCommand {
    /// List checkpoints for a workflow.
    List(IdArgs),
    /// Get a specific checkpoint for a workflow.
    Get(WorkflowCheckpointGetArgs),
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowCheckpointGetArgs {
    #[arg(long, value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "INDEX", help = "Checkpoint index (zero-based).")]
    pub(crate) checkpoint: usize,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowRunArgs {
    #[arg(
        long,
        value_name = "TASK_ID",
        help = "Task id to run the workflow for."
    )]
    pub(crate) task_id: String,
    #[arg(
        long,
        value_name = "PIPELINE_ID",
        help = "Optional pipeline id override."
    )]
    pub(crate) pipeline_id: Option<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPauseArgs {
    #[arg(long, value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "WORKFLOW_ID",
        help = "Confirmation token; must match --id."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview pause payload without mutating workflow state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowCancelArgs {
    #[arg(long, value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "WORKFLOW_ID",
        help = "Confirmation token; must match --id."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview cancellation payload without mutating workflow state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseApproveArgs {
    #[arg(long, value_name = "WORKFLOW_ID", help = "Workflow identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
    #[arg(long, value_name = "TEXT", help = "Approval note for the phase gate.")]
    pub(crate) note: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseGetArgs {
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseUpsertArgs {
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
    #[arg(
        long,
        value_name = "JSON",
        help = "Phase runtime definition JSON payload."
    )]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPhaseRemoveArgs {
    #[arg(long, value_name = "PHASE_ID", help = "Phase identifier.")]
    pub(crate) phase: String,
    #[arg(
        long,
        value_name = "PHASE_ID",
        help = "Confirmation token; must match --phase."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview phase removal impact without mutating workflow config."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPipelineUpsertArgs {
    #[arg(
        long,
        value_name = "JSON",
        help = "Workflow pipeline definition JSON payload."
    )]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowPipelineUpdateArgs {
    #[arg(long, value_name = "PIPELINE_ID", help = "Pipeline identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "NAME", help = "Pipeline display name.")]
    pub(crate) name: String,
    #[arg(long, value_name = "TEXT", help = "Optional pipeline description.")]
    pub(crate) description: Option<String>,
    #[arg(
        long = "phase",
        value_name = "PHASE_ID",
        help = "Ordered phase ids for the pipeline. Repeat to add multiple phases."
    )]
    pub(crate) phases: Vec<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowStateMachineSetArgs {
    #[arg(
        long,
        value_name = "JSON",
        help = "Workflow state-machine configuration JSON payload."
    )]
    pub(crate) input_json: String,
}

#[derive(Debug, Args)]
pub(crate) struct WorkflowAgentRuntimeSetArgs {
    #[arg(
        long,
        value_name = "JSON",
        help = "Workflow agent-runtime configuration JSON payload."
    )]
    pub(crate) input_json: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn agent_run_help_includes_actionable_field_descriptions() {
        let error = Cli::try_parse_from(["ao", "agent", "run", "--help"])
            .expect_err("help output should short-circuit parsing");
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();
        assert!(help.contains("Run identifier. Omit to auto-generate a UUID."));
        assert!(help.contains("CLI provider to execute, for example codex or claude."));
        assert!(help.contains("Runner config scope: project or global."));
    }

    #[test]
    fn daemon_run_rejects_zero_interval_with_clear_validation_error() {
        let error = Cli::try_parse_from(["ao", "daemon", "run", "--interval-secs", "0"])
            .expect_err("zero interval should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--interval-secs"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn daemon_run_rejects_zero_max_tasks_per_tick_with_clear_validation_error() {
        let error = Cli::try_parse_from(["ao", "daemon", "run", "--max-tasks-per-tick", "0"])
            .expect_err("zero max-tasks-per-tick should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--max-tasks-per-tick"));
        assert!(message.contains("greater than 0"));
    }

    #[test]
    fn daemon_events_rejects_zero_limit() {
        let error = Cli::try_parse_from(["ao", "daemon", "events", "--limit", "0"])
            .expect_err("zero limit should fail validation");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        let message = error.to_string();
        assert!(message.contains("--limit"));
        assert!(message.contains("greater than 0"));
    }
}
