use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum SkillCommand {
    /// Search skill catalog entries.
    Search(SkillSearchArgs),
    /// Install a skill with deterministic resolution.
    Install(SkillInstallArgs),
    /// List installed skills and lock alignment.
    List,
    /// Re-resolve one or all installed skills.
    Update(SkillUpdateArgs),
    /// Publish a new skill version into the registry catalog.
    Publish(SkillPublishArgs),
    /// Manage registered skill registry sources.
    Registry {
        #[command(subcommand)]
        command: SkillRegistryCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillRegistryCommand {
    /// Register a new registry source or update an existing one.
    Add(SkillRegistryAddArgs),
    /// Remove a registered registry source.
    Remove(SkillRegistryRemoveArgs),
    /// List all registered registry sources.
    List,
}

#[derive(Debug, Args)]
pub(crate) struct SkillRegistryAddArgs {
    #[arg(long, help = "Registry identifier.")]
    pub(crate) id: String,
    #[arg(long, help = "Registry URL (for example https://registry.mcp.run).")]
    pub(crate) url: String,
    #[arg(long, help = "Search priority (lower value means higher priority).")]
    pub(crate) priority: Option<u32>,
}

#[derive(Debug, Args)]
pub(crate) struct SkillRegistryRemoveArgs {
    #[arg(long, help = "Registry identifier to remove.")]
    pub(crate) id: String,
}

#[derive(Debug, Args)]
pub(crate) struct SkillSearchArgs {
    #[arg(long, help = "Match skill name text (case-insensitive).")]
    pub(crate) query: Option<String>,
    #[arg(long, help = "Filter by source identifier.")]
    pub(crate) source: Option<String>,
    #[arg(long, help = "Filter by registry identifier.")]
    pub(crate) registry: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SkillInstallArgs {
    #[arg(long, help = "Skill name to resolve and install.")]
    pub(crate) name: String,
    #[arg(long, help = "Optional version constraint (semver req).")]
    pub(crate) version: Option<String>,
    #[arg(long, help = "Optional source constraint.")]
    pub(crate) source: Option<String>,
    #[arg(long, help = "Optional registry constraint.")]
    pub(crate) registry: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Allow pre-release versions during resolution."
    )]
    pub(crate) allow_prerelease: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillUpdateArgs {
    #[arg(
        long,
        help = "Optional skill name target; omit to update all installed skills."
    )]
    pub(crate) name: Option<String>,
    #[arg(long, help = "Optional version constraint override (semver req).")]
    pub(crate) version: Option<String>,
    #[arg(long, help = "Optional source constraint override.")]
    pub(crate) source: Option<String>,
    #[arg(long, help = "Optional registry constraint override.")]
    pub(crate) registry: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Allow pre-release versions during resolution."
    )]
    pub(crate) allow_prerelease: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillPublishArgs {
    #[arg(long, help = "Skill package name.")]
    pub(crate) name: String,
    #[arg(long, help = "Skill semver version.")]
    pub(crate) version: String,
    #[arg(
        long,
        help = "Source identifier (for example local, github, internal)."
    )]
    pub(crate) source: String,
    #[arg(long, default_value = "project", help = "Target registry identifier.")]
    pub(crate) registry: String,
    #[arg(long, help = "Artifact reference (path, URI, or package id).")]
    pub(crate) artifact: Option<String>,
    #[arg(long, help = "Integrity hash; auto-derived when omitted.")]
    pub(crate) integrity: Option<String>,
}
