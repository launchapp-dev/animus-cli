use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum StateCommand {
    /// Export scoped runtime state to a versioned tar.zst archive.
    Export(StateExportArgs),
    /// Import a versioned tar.zst archive into scoped runtime state.
    Import(StateImportArgs),
}

#[derive(Debug, Args)]
pub(crate) struct StateExportArgs {
    /// Output archive path. Defaults to
    /// `animus-state-<repo-scope>-<UTC-timestamp>.tar.zst` in the cwd.
    #[arg(long, value_name = "PATH")]
    pub(crate) out: Option<String>,
    /// Include the `runs/` directory (workflow run history) in the archive.
    #[arg(long, default_value_t = false)]
    pub(crate) include_runs: bool,
    /// Include the `artifacts/` directory (potentially large) in the archive.
    #[arg(long, default_value_t = false)]
    pub(crate) include_artifacts: bool,
}

#[derive(Debug, Args)]
pub(crate) struct StateImportArgs {
    /// Path to a `*.tar.zst` archive produced by `animus state export`.
    #[arg(value_name = "PATH")]
    pub(crate) archive: String,
    /// Re-scope the archive into a different project root. The new scope id
    /// is computed from this path with `repository_scope_for_path`.
    #[arg(long, value_name = "PATH")]
    pub(crate) into_project: Option<String>,
    /// Allow overwriting an existing non-empty scope directory. A safety
    /// snapshot is taken to `~/.animus/<scope>/.backup-pre-import-<ts>/`
    /// before extraction.
    #[arg(long, default_value_t = false)]
    pub(crate) yes: bool,
    /// Explicit opt-in to overwrite an existing `~/.animus/principals.yaml`
    /// when the archived copy differs. `--yes` alone never touches RBAC
    /// config.
    #[arg(long, default_value_t = false)]
    pub(crate) yes_overwrite_principals: bool,
}
