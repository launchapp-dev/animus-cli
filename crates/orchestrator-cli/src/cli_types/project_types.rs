use clap::{Args, Subcommand};

use super::{IdArgs, INPUT_JSON_PRECEDENCE_HELP};

#[derive(Debug, Subcommand)]
pub(crate) enum ProjectCommand {
    /// List registered projects.
    List,
    /// Show the active project.
    Active,
    /// Get a project by id.
    Get(IdArgs),
    /// Create a new project entry.
    Create(ProjectCreateArgs),
    /// Mark a project as active.
    Load(IdArgs),
    /// Rename a project.
    Rename(ProjectRenameArgs),
    /// Archive a project.
    Archive(IdArgs),
    /// Remove a project.
    Remove(IdArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ProjectCreateArgs {
    #[arg(long, value_name = "NAME", help = "Human-friendly project name.")]
    pub(crate) name: String,
    #[arg(long, value_name = "PATH", help = "Filesystem path to the project root.")]
    pub(crate) path: String,
    #[arg(
        long,
        value_name = "TYPE",
        help = "Project type: web-app|mobile-app|desktop-app|full-stack-platform|library|infrastructure|other (aliases accepted)."
    )]
    pub(crate) project_type: Option<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]

pub(crate) struct ProjectRenameArgs {
    #[arg(long, value_name = "ID", help = "Project identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "NAME", help = "Updated project name.")]
    pub(crate) name: String,
}
