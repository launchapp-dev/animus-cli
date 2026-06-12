use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum FlavorCommand {
    /// List available flavor manifests on disk.
    List,
    /// Show the currently active flavor + drift report against the manifest.
    Current(FlavorCurrentArgs),
    /// Print a parsed flavor manifest (TOML or JSON via --json).
    Info(FlavorDescribeArgs),
    /// Install the named flavor: every plugin its manifest marks `required`
    /// (delegates to `animus plugin install-defaults --flavor <name>`).
    Install(FlavorInstallArgs),
}

#[derive(Debug, Args)]
pub(crate) struct FlavorCurrentArgs {
    /// Flavor id to probe. Defaults to the project's persisted active
    /// flavor (`.animus/plugin-scope.yaml` `active_flavor:`), falling back
    /// to `default` when none is recorded. Pass `--name` to probe a
    /// specific flavor regardless of the persisted selection.
    #[arg(long)]
    pub(crate) name: Option<String>,
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
    /// Also install every plugin the flavor manifest marks `recommended`.
    #[arg(long, default_value_t = false)]
    pub(crate) include_recommended: bool,
}
