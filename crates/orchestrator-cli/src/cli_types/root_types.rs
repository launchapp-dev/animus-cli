use clap::{Parser, Subcommand};

use super::*;

#[derive(Debug, Parser)]
#[command(name = "ao", about = "Agent Orchestrator CLI", version)]
pub(crate) struct Cli {
    #[arg(long, global = true, help = "Emit machine-readable JSON output using the ao.cli.v1 envelope.")]
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
    /// Run and control workflow execution.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Draft and manage project requirements.
    Requirements {
        #[command(subcommand)]
        command: RequirementsCommand,
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
    /// Install, inspect, and pin workflow packs.
    Pack {
        #[command(subcommand)]
        command: PackCommand,
    },
    /// Inspect runner health and orphaned runs.
    Runner {
        #[command(subcommand)]
        command: RunnerCommand,
    },
    /// Show a unified project status dashboard.
    Status,
    /// Show unified work inbox and current focus (next task, active workflows, blocked/stale items).
    Now,
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
    /// Sync tasks and requirements with a remote ao-sync server.
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
    /// Run environment and configuration diagnostics.
    Doctor(DoctorArgs),
    /// Inspect and manage event triggers.
    Trigger {
        #[command(subcommand)]
        command: TriggerCommand,
    },
}
