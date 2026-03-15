use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum GitCommand {
    /// Manage repo registry entries.
    Repo {
        #[command(subcommand)]
        command: GitRepoCommand,
    },
    /// List repository branches.
    Branches(GitRepoArgs),
    /// Show repository status.
    Status(GitRepoArgs),
    /// Commit staged/untracked changes.
    Commit(GitCommitArgs),
    /// Push branch updates.
    Push(GitPushArgs),
    /// Pull branch updates.
    Pull(GitPullArgs),
    /// Manage git worktrees.
    Worktree {
        #[command(subcommand)]
        command: GitWorktreeCommand,
    },
    /// Manage confirmation requests/outcomes for destructive git operations.
    Confirm {
        #[command(subcommand)]
        command: GitConfirmCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitRepoCommand {
    /// List registered repositories.
    List,
    /// Get details for one repository.
    Get(GitRepoArgs),
    /// Initialize and register a local repository.
    Init(GitRepoInitArgs),
    /// Clone and register a repository.
    Clone(GitRepoCloneArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GitRepoArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitRepoInitArgs {
    #[arg(long, value_name = "NAME", help = "Repository registration name.")]
    pub(crate) name: String,
    #[arg(long, value_name = "PATH", help = "Optional filesystem path.")]
    pub(crate) path: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitRepoCloneArgs {
    #[arg(long, value_name = "URL", help = "Git clone URL.")]
    pub(crate) url: String,
    #[arg(long, value_name = "NAME", help = "Repository registration name.")]
    pub(crate) name: String,
    #[arg(long, value_name = "PATH", help = "Optional destination directory.")]
    pub(crate) path: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitCommitArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "TEXT", help = "Commit message.")]
    pub(crate) message: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitPushArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "REMOTE", default_value = "origin", help = "Git remote name.")]
    pub(crate) remote: String,
    #[arg(long, value_name = "BRANCH", default_value = "main", help = "Branch to push.")]
    pub(crate) branch: String,
    #[arg(long, default_value_t = false, help = "Force push (destructive and requires --confirmation-id).")]
    pub(crate) force: bool,
    #[arg(long, value_name = "ID", help = "Approved confirmation id required for destructive git operations.")]
    pub(crate) confirmation_id: Option<String>,
    #[arg(long, default_value_t = false, help = "Preview command payload without changing repository state.")]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitPullArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "REMOTE", default_value = "origin", help = "Git remote name.")]
    pub(crate) remote: String,
    #[arg(long, value_name = "BRANCH", default_value = "main", help = "Branch to pull.")]
    pub(crate) branch: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitWorktreeCommand {
    /// Create a repository worktree.
    Create(GitWorktreeCreateArgs),
    /// List repository worktrees.
    List(GitRepoArgs),
    /// Get one worktree by name.
    Get(GitWorktreeGetArgs),
    /// Remove a worktree (confirmation required).
    Remove(GitWorktreeRemoveArgs),
    /// Prune managed task worktrees for done/cancelled tasks.
    Prune(GitWorktreePruneArgs),
    /// Pull updates in a worktree.
    Pull(GitWorktreePullArgs),
    /// Push updates from a worktree.
    Push(GitWorktreePushArgs),
    /// Pull then push a worktree.
    Sync(GitWorktreeSyncArgs),
    /// Show synchronization status for a worktree.
    SyncStatus(GitWorktreeGetArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeCreateArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree registration name.")]
    pub(crate) worktree_name: String,
    #[arg(long, value_name = "PATH", help = "Filesystem path for the worktree.")]
    pub(crate) worktree_path: String,
    #[arg(long, value_name = "BRANCH", help = "Branch to check out in the worktree.")]
    pub(crate) branch: String,
    #[arg(long, default_value_t = false, help = "Create the branch when it does not already exist.")]
    pub(crate) create_branch: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeGetArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree name.")]
    pub(crate) worktree_name: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeRemoveArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree name.")]
    pub(crate) worktree_name: String,
    #[arg(long, default_value_t = false, help = "Force removal if the worktree is dirty.")]
    pub(crate) force: bool,
    #[arg(long, value_name = "ID", help = "Approved confirmation id required before removing a worktree.")]
    pub(crate) confirmation_id: Option<String>,
    #[arg(long, default_value_t = false, help = "Preview command payload without changing repository state.")]
    pub(crate) dry_run: bool,
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

#[derive(Debug, Args)]
pub(crate) struct GitWorktreePullArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree name.")]
    pub(crate) worktree_name: String,
    #[arg(long, value_name = "REMOTE", default_value = "origin", help = "Git remote name.")]
    pub(crate) remote: String,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreePushArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree name.")]
    pub(crate) worktree_name: String,
    #[arg(long, value_name = "REMOTE", default_value = "origin", help = "Git remote name.")]
    pub(crate) remote: String,
    #[arg(long, default_value_t = false, help = "Force push (destructive and requires --confirmation-id).")]
    pub(crate) force: bool,
    #[arg(long, value_name = "ID", help = "Approved confirmation id required for destructive git operations.")]
    pub(crate) confirmation_id: Option<String>,
    #[arg(long, default_value_t = false, help = "Preview command payload without changing repository state.")]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GitWorktreeSyncArgs {
    #[arg(long, value_name = "REPO", help = "Repository name or path.")]
    pub(crate) repo: String,
    #[arg(long, value_name = "NAME", help = "Worktree name.")]
    pub(crate) worktree_name: String,
    #[arg(long, value_name = "REMOTE", default_value = "origin", help = "Git remote name.")]
    pub(crate) remote: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GitConfirmCommand {
    /// Request a confirmation record for a destructive git operation.
    Request(GitConfirmRequestArgs),
    /// Approve or reject a confirmation request.
    Respond(GitConfirmRespondArgs),
    /// Record operation outcome for a confirmation request.
    Outcome(GitConfirmOutcomeArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GitConfirmRequestArgs {
    #[arg(long, value_name = "TYPE", help = "Operation type, for example force_push or remove_worktree.")]
    pub(crate) operation_type: String,
    #[arg(long, value_name = "REPO", help = "Repository name.")]
    pub(crate) repo_name: String,
    #[arg(long, value_name = "JSON", help = "Optional JSON context payload.")]
    pub(crate) context_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitConfirmRespondArgs {
    #[arg(long, value_name = "ID", help = "Confirmation request identifier.")]
    pub(crate) request_id: String,
    #[arg(long, help = "Set to true to approve, false to reject.")]
    pub(crate) approved: bool,
    #[arg(long, value_name = "TEXT", help = "Optional reviewer comment.")]
    pub(crate) comment: Option<String>,
    #[arg(long, value_name = "USER", help = "Reviewer user id.")]
    pub(crate) user_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GitConfirmOutcomeArgs {
    #[arg(long, value_name = "ID", help = "Confirmation request identifier.")]
    pub(crate) request_id: String,
    #[arg(long, help = "Whether the operation succeeded.")]
    pub(crate) success: bool,
    #[arg(long, value_name = "TEXT", help = "Outcome message.")]
    pub(crate) message: String,
    #[arg(long, value_name = "JSON", help = "Optional JSON metadata payload.")]
    pub(crate) metadata_json: Option<String>,
}
