use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum FlavorCommand {
    /// List available flavor manifests on disk.
    List,
    /// Show the currently active flavor + drift report against the manifest.
    Current(FlavorCurrentArgs),
    /// Print a parsed flavor manifest (TOML or JSON via --json).
    Describe(FlavorDescribeArgs),
    /// Install the named flavor (equivalent to `animus plugin install-defaults
    /// --include-subjects --include-transports`).
    Install(FlavorInstallArgs),
}

#[derive(Debug, Args)]
pub(crate) struct FlavorCurrentArgs {
    /// Flavor id to probe (defaults to `default`).
    #[arg(long, default_value = "default")]
    pub(crate) name: String,
}

#[derive(Debug, Args)]
pub(crate) struct FlavorDescribeArgs {
    /// Flavor id to describe (defaults to `default`).
    #[arg(long, default_value = "default")]
    pub(crate) name: String,
}

#[derive(Debug, Args)]
pub(crate) struct FlavorInstallArgs {
    /// Flavor id to install (defaults to `default`).
    #[arg(default_value = "default")]
    pub(crate) name: String,
    /// Allow overwriting plugins that are already installed.
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    /// Suppress install confirmation prompts.
    #[arg(long, default_value_t = false)]
    pub(crate) yes: bool,
}
