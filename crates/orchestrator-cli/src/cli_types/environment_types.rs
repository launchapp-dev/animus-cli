use clap::{Args, Subcommand};

/// `animus environment` — manage environment nodes (ephemeral run instances)
/// through the installed environment plugin: list, inspect, tear down one, and
/// reap orphaned/dead nodes. Backed by the environment role's node-management
/// surface (`environment/list` · `/get` · `/teardown_node` · `/reap`).
#[derive(Debug, Subcommand)]
pub(crate) enum EnvironmentCommand {
    /// List managed environment nodes with their state + orphan flag.
    List(EnvironmentListArgs),
    /// Describe one managed node by substrate id or name.
    Get(EnvironmentGetArgs),
    /// Destroy one managed node by substrate id or name (idempotent).
    Teardown(EnvironmentTeardownArgs),
    /// Reap orphaned/dead nodes. Default reaps dead (FAILED/CRASHED) nodes AND,
    /// when the journal's live run set is readable, healthy (SUCCESS) nodes whose
    /// owning workflow is terminal/gone (owner-known mode, plugin age grace
    /// applies). `--all --force` keeps the legacy behavior: also reap healthy
    /// nodes with no live owning run.
    Reap(EnvironmentReapArgs),
}

#[derive(Debug, Args)]
pub(crate) struct EnvironmentListArgs {
    #[arg(long, value_name = "PLUGIN", help = ENVIRONMENT_ID_HELP)]
    pub(crate) environment: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct EnvironmentGetArgs {
    /// Substrate id or name of the node (e.g. a service id or `animus-run-*`).
    #[arg(value_name = "ID")]
    pub(crate) id: String,
    #[arg(long, value_name = "PLUGIN", help = ENVIRONMENT_ID_HELP)]
    pub(crate) environment: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct EnvironmentTeardownArgs {
    /// Substrate id or name of the node to destroy.
    #[arg(value_name = "ID")]
    pub(crate) id: String,
    #[arg(long, value_name = "PLUGIN", help = ENVIRONMENT_ID_HELP)]
    pub(crate) environment: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct EnvironmentReapArgs {
    /// Also reap non-dead nodes that have no live owning run (needs --force),
    /// WITHOUT consulting the journal's live run set (legacy healthy-orphan
    /// sweep). Without --all, the default reap runs owner-known: it passes the
    /// journal's live run ids so healthy nodes of terminal/gone workflows are
    /// reaped too (subject to the plugin's age grace).
    #[arg(long)]
    pub(crate) all: bool,
    /// Confirm reaping healthy orphans; required with --all (guards against a
    /// process with no liveness view treating every node as an orphan).
    #[arg(long)]
    pub(crate) force: bool,
    /// Report what WOULD be reaped without deleting anything.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Only reap nodes at least this many seconds old.
    #[arg(long, value_name = "SECS")]
    pub(crate) older_than_secs: Option<u64>,
    #[arg(long, value_name = "PLUGIN", help = ENVIRONMENT_ID_HELP)]
    pub(crate) environment: Option<String>,
}

const ENVIRONMENT_ID_HELP: &str = "Environment plugin id to target. Defaults to the sole installed environment plugin.";
