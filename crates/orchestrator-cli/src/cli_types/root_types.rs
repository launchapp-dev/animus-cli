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
    /// v0.5.8 honor-system principal override. Sends `$/setPrincipal` to
    /// the daemon at connection open. Logged loudly; ignored under
    /// `policy.rbac=single-user`. Under `enforce` the daemon rejects
    /// impersonation when peer credentials don't match.
    #[arg(
        long = "as",
        global = true,
        value_name = "PRINCIPAL",
        help = "Impersonate a declared principal (honor-system; warned)."
    )]
    pub(crate) as_principal: Option<String>,
    /// v0.5.9: bypass all on-disk and in-memory hot-path caches for this
    /// invocation. Mirrors `ANIMUS_DISABLE_*_CACHE=1` env vars but lets
    /// scripts opt out per-call. Reads stay correct because caches are
    /// best-effort fall-throughs to the live source of truth.
    #[arg(long = "no-cache", global = true, help = "Bypass hot-path read caches for this invocation.")]
    pub(crate) no_cache: bool,

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
    /// Hold multi-turn conversations with a provider tool (v0.5.10).
    Chat {
        #[command(subcommand)]
        command: ChatCommand,
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
    /// Manage the `animus` binary itself — check for and install updates.
    #[command(name = "self")]
    SelfCmd {
        #[command(subcommand)]
        command: SelfCommand,
    },
    /// Check for and install a newer `animus` release in one step.
    /// Thin top-level alias over `animus self update` with the simplified
    /// `--check / --yes / --channel` surface.
    Update(UpdateArgs),
    /// Manage opt-in anonymous usage telemetry.
    Metrics {
        #[command(subcommand)]
        command: MetricsCommand,
    },
    /// Inspect token + USD spend across workflow runs (v0.5.5).
    Cost {
        #[command(subcommand)]
        command: CostCommand,
    },
    /// Inspect identity + permissions (v0.5.8 small-core RBAC).
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Stream workflow lifecycle events from the daemon (v0.5.8).
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
    /// Export and import scoped runtime state for backup or migration (v0.5.8).
    State {
        #[command(subcommand)]
        command: StateCommand,
    },
    /// Manage project-scoped secrets stored in the OS keychain (v0.5.8).
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
}
