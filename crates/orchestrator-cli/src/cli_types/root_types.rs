use clap::{Parser, Subcommand};

use super::*;

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
    /// Inspect and mutate the daemon dispatch queue.
    Queue {
        #[command(subcommand)]
        command: QueueCommand,
    },
    /// Manage tasks, dependencies, status, and operational controls.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Deprecated alias for `task`; retained for backwards compatibility.
    #[command(name = "task-control", hide = true)]
    TaskControl {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Run and control workflow execution.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Manage workflow schedules.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    /// Draft and refine project vision artifacts.
    Vision {
        #[command(subcommand)]
        command: VisionCommand,
    },
    /// Planning facade spanning vision and requirements workflows.
    Planning {
        #[command(subcommand)]
        command: PlanningCommand,
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
    /// Deprecated alias for `workflow execute`.
    #[command(hide = true)]
    Execute {
        #[command(subcommand)]
        command: ExecuteCommand,
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
    /// Show a unified project status dashboard.
    Status,
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
    /// Live workflow phase monitor with agent output streaming.
    WorkflowMonitor(WorkflowMonitorArgs),
    /// Run environment and configuration diagnostics.
    Doctor(DoctorArgs),
}
