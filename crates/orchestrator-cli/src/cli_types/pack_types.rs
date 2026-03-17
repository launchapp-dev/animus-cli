use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum PackCommand {
    /// Install a pack from a local path or marketplace registry.
    Install(PackInstallArgs),
    /// List discovered packs and indicate which ones are active for this project.
    List(PackListArgs),
    /// Inspect a discovered pack or a local pack manifest.
    Inspect(PackInspectArgs),
    /// Pin a pack version/source or toggle enablement for this project.
    Pin(PackPinArgs),
    /// Search packs across marketplace registries.
    Search(PackSearchArgs),
    /// Manage marketplace registries for remote pack discovery and installation.
    Registry {
        #[command(subcommand)]
        command: PackRegistryCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum PackRegistryCommand {
    /// Add a marketplace registry (git URL).
    Add(PackRegistryAddArgs),
    /// Remove a marketplace registry.
    Remove(PackRegistryRemoveArgs),
    /// List all registered marketplace registries.
    List,
    /// Sync (re-clone) a registry to get latest pack catalog.
    Sync(PackRegistrySyncArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PackRegistryAddArgs {
    #[arg(long, help = "Registry identifier (e.g. 'audiogenius').")]
    pub(crate) id: String,
    #[arg(long, help = "Git URL of the marketplace repository.")]
    pub(crate) url: String,
}

#[derive(Debug, Args)]
pub(crate) struct PackRegistryRemoveArgs {
    #[arg(long, help = "Registry identifier to remove.")]
    pub(crate) id: String,
}

#[derive(Debug, Args)]
pub(crate) struct PackRegistrySyncArgs {
    #[arg(long, help = "Registry identifier to sync. Omit to sync all.")]
    pub(crate) id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct PackSearchArgs {
    #[arg(long, help = "Search query to match against pack names and descriptions.")]
    pub(crate) query: Option<String>,
    #[arg(long, help = "Filter by category (e.g. 'database', 'productivity', 'devops').")]
    pub(crate) category: Option<String>,
    #[arg(long, help = "Filter by registry identifier.")]
    pub(crate) registry: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct PackInstallArgs {
    #[arg(long, value_name = "PATH", help = "Local pack directory containing pack.toml.")]
    pub(crate) path: Option<String>,
    #[arg(long, help = "Pack name to install from a marketplace registry.")]
    pub(crate) name: Option<String>,
    #[arg(long, help = "Marketplace registry identifier to install from.")]
    pub(crate) registry: Option<String>,
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
