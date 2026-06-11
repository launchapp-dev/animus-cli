use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub(crate) enum SkillCommand {
    /// Search skills across built-in, user, project, and registry sources.
    Search(SkillSearchArgs),
    /// Install a skill with deterministic resolution.
    Install(SkillInstallArgs),
    /// List all available skills (built-in, user, project, and installed).
    List(SkillListArgs),
    /// Show details of a resolved skill definition.
    #[command(alias = "show")]
    Info(SkillShowArgs),
    /// Re-resolve one or all installed skills.
    Update(SkillUpdateArgs),
    /// Uninstall an installed skill: remove its materialized files and registry/lock entries.
    Uninstall(SkillUninstallArgs),
    /// Publish a new skill version into the registry catalog.
    Publish(SkillPublishArgs),
    /// Manage registered skill registry sources.
    Registry {
        #[command(subcommand)]
        command: SkillRegistryCommand,
    },
    /// Migrate legacy `.ao/skills/` (v0.3) into `.animus/skills/` (v0.4).
    ///
    /// Moves every entry under `<project>/.ao/skills/` to
    /// `<project>/.animus/skills/`, drops a `.migrated-from-ao` marker so the
    /// resolver stops warning about the legacy path, and (by default) also
    /// migrates `~/.ao/skills/` to `~/.animus/skills/`. Refuses to clobber
    /// existing non-empty targets — operator must merge manually.
    MigrateFromAo(SkillMigrateFromAoArgs),
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
    #[arg(long, help = "Skill name to resolve and install; optional with --path.")]
    pub(crate) name: Option<String>,
    #[arg(long, help = "Install a local Markdown skill file, skill folder, or directory of skill folders.")]
    pub(crate) path: Option<PathBuf>,
    #[arg(long, help = "Optional version constraint (semver req).")]
    pub(crate) version: Option<String>,
    #[arg(long, help = "Optional source constraint.")]
    pub(crate) source: Option<String>,
    #[arg(long, help = "Optional registry constraint.")]
    pub(crate) registry: Option<String>,
    #[arg(long, default_value_t = false, help = "Allow pre-release versions during resolution.")]
    pub(crate) allow_prerelease: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillUpdateArgs {
    #[arg(long, help = "Optional skill name target; omit to update all installed skills.")]
    pub(crate) name: Option<String>,
    #[arg(long, help = "Optional version constraint override (semver req).")]
    pub(crate) version: Option<String>,
    #[arg(long, help = "Optional source constraint override.")]
    pub(crate) source: Option<String>,
    #[arg(long, help = "Optional registry constraint override.")]
    pub(crate) registry: Option<String>,
    #[arg(long, default_value_t = false, help = "Allow pre-release versions during resolution.")]
    pub(crate) allow_prerelease: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillUninstallArgs {
    #[arg(value_name = "SKILL_ID", help = "Skill name to uninstall.")]
    pub(crate) name: String,
    #[arg(long, help = "Optional source constraint; omit to remove installed entries from all sources.")]
    pub(crate) source: Option<String>,
    #[arg(long, default_value_t = false, help = "Print what would be removed without modifying anything.")]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillListArgs {
    #[arg(long, help = "Filter by source: built-in, user, project, or installed.")]
    pub(crate) source: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SkillShowArgs {
    #[arg(long, help = "Skill name to show.")]
    pub(crate) name: String,
}

#[derive(Debug, Args)]
pub(crate) struct SkillMigrateFromAoArgs {
    #[arg(long, default_value_t = false, help = "Print actions without modifying the filesystem.")]
    pub(crate) dry_run: bool,
    #[arg(long, default_value_t = false, help = "Skip migrating the user-scoped ~/.ao/skills/ directory.")]
    pub(crate) project_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillPublishArgs {
    #[arg(long, help = "Skill package name.")]
    pub(crate) name: String,
    #[arg(long, help = "Skill semver version.")]
    pub(crate) version: String,
    #[arg(long, help = "Source identifier (for example local, github, internal).")]
    pub(crate) source: String,
    #[arg(long, default_value = "project", help = "Target registry identifier.")]
    pub(crate) registry: String,
    #[arg(long, help = "Artifact reference (path, URI, or package id).")]
    pub(crate) artifact: Option<String>,
    #[arg(long, help = "Integrity hash; auto-derived when omitted.")]
    pub(crate) integrity: Option<String>,
}
