use clap::{Parser, Subcommand};

use super::*;

#[derive(Debug, Parser)]
#[command(name = "animus", about = "Animus — the spirit that drives your agents", version)]
pub(crate) struct Cli {
    #[arg(long, global = true, help = "Emit machine-readable JSON output using the animus.cli.v1 envelope.")]
    pub(crate) json: bool,
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Project root directory. Overrides default root resolution."
    )]
    pub(crate) project_root: Option<String>,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Show the installed `animus` version.
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
    /// Run and control workflow execution.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Inspect and search execution history.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
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
    /// Discover, inspect, install, and call Animus STDIO plugins.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
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
    /// Run the Animus MCP service endpoint.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Serve and open the Animus web UI.
    Web {
        #[command(subcommand)]
        command: WebCommand,
    },
    /// Initialize an Animus project from a template.
    Init(InitArgs),
    /// Run environment and configuration diagnostics.
    Doctor(DoctorArgs),
    /// Inspect and manage event triggers.
    Trigger {
        #[command(subcommand)]
        command: TriggerCommand,
    },
    /// Tail and inspect daemon log output (in-tree or via log_storage_backend plugin).
    Logs {
        #[command(subcommand)]
        command: LogsCommand,
    },
    /// List, get, create, and update subjects via installed subject_backend plugins.
    Subject {
        #[command(subcommand)]
        command: SubjectCommand,
    },
    /// Inspect or install Animus flavor manifests (`flavors/<name>.toml`).
    Flavor {
        #[command(subcommand)]
        command: FlavorCommand,
    },
}
