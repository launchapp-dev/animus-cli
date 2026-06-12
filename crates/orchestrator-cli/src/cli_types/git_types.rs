use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum GitCommand {
    /// Manage repo registry entries.
    Repo {
        #[command(subcommand)]
        command: GitRepoCommand,
    },
    /// Manage git worktrees.
    Worktree {
        #[command(subcommand)]
        command: GitWorktreeCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitRepoCommand {
    /// List registered repositories.
    List,
}

#[derive(Debug, Args)]
pub(crate) struct GitRepoArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitWorktreeCommand {
    /// List repository worktrees.
    List(GitRepoArgs),
    /// Prune managed task worktrees for done/cancelled tasks.
    Prune(GitWorktreePruneArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreePruneArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Delete remote branches for pruned worktrees when branch metadata is available."
    )]
    pub(crate) delete_remote_branch: bool,
    #[arg(
        long,
        value_name = "REMOTE",
        default_value = "origin",
        help = "Git remote name used with --delete-remote-branch."
    )]
    pub(crate) remote: String,
    #[arg(long, value_name = "ID", help = "Approved confirmation id required before pruning worktrees.")]
    pub(crate) confirmation_id: Option<String>,
    #[arg(long, default_value_t = false, help = "Preview prune actions without changing repository state.")]
    pub(crate) dry_run: bool,
}
