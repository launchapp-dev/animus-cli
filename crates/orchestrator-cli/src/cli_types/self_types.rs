use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum SelfCommand {
    /// Check for, download, and install a newer `animus` release from GitHub.
    Update(SelfUpdateArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SelfUpdateArgs {
    /// Print the available version and exit. Non-zero exit when no update
    /// is available so this is scriptable in CI hooks.
    #[arg(long, default_value_t = false)]
    pub(crate) check_only: bool,
    /// Re-install the current version (useful to repair a broken install).
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    /// Consider prereleases regardless of the configured `auto_update.channel`.
    #[arg(long, default_value_t = false)]
    pub(crate) prerelease: bool,
    /// Skip the interactive `[y/N]` confirmation. Useful for CI.
    #[arg(long, default_value_t = false)]
    pub(crate) yes: bool,
}
