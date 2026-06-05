use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum RunnerCommand {
    /// Show provider plugin health (one row per discovered provider).
    Health,
    /// Detect and clean orphaned CLI processes tracked under the cli-tracker.
    Orphans {
        #[command(subcommand)]
        command: RunnerOrphanCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum RunnerOrphanCommand {
    /// Detect orphaned CLI processes.
    Detect,
    /// Clean orphaned CLI processes.
    Cleanup(RunnerOrphanCleanupArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RunnerOrphanCleanupArgs {
    #[arg(long = "run-id")]
    pub(crate) run_id: Vec<String>,
}
