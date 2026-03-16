use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum PackCommand {
    /// Install a pack from a local filesystem path into the machine pack registry.
    Install(PackInstallArgs),
    /// List discovered packs and indicate which ones are active for this project.
    List(PackListArgs),
    /// Inspect a discovered pack or a local pack manifest.
    Inspect(PackInspectArgs),
    /// Pin a pack version/source or toggle enablement for this project.
    Pin(PackPinArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PackInstallArgs {
    #[arg(long, value_name = "PATH", help = "Local pack directory containing pack.toml.")]
    pub(crate) path: String,
    #[arg(long, default_value_t = false, help = "Overwrite an existing installed pack with the same id and version.")]
    pub(crate) force: bool,
    #[arg(long, default_value_t = false, help = "Activate the installed pack for this project immediately.")]
    pub(crate) activate: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackListArgs {
    #[arg(long, default_value_t = false, help = "Show only packs currently active for this project.")]
    pub(crate) active_only: bool,
    #[arg(long, help = "Filter by source: bundled, installed, or project_override.")]
    pub(crate) source: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct PackInspectArgs {
    #[arg(long, help = "Pack identifier to inspect from discovered inventory.")]
    pub(crate) pack_id: Option<String>,
    #[arg(long, help = "Optional exact version to inspect.")]
    pub(crate) version: Option<String>,
    #[arg(long, help = "Optional source: bundled, installed, or project_override.")]
    pub(crate) source: Option<String>,
    #[arg(long, value_name = "PATH", help = "Inspect a local pack directory instead of discovered inventory.")]
    pub(crate) path: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct PackPinArgs {
    #[arg(long, help = "Pack identifier to pin.")]
    pub(crate) pack_id: String,
    #[arg(long, help = "Optional semver requirement to pin (for example '=0.2.0' or '^0.2').")]
    pub(crate) version: Option<String>,
    #[arg(long, help = "Optional preferred source: bundled, installed, or project_override.")]
    pub(crate) source: Option<String>,
    #[arg(long, default_value_t = false, help = "Disable this pack for the current project.")]
    pub(crate) disable: bool,
}
