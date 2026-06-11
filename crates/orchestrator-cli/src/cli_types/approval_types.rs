use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum ApprovalCommand {
    /// Request an approval record for a destructive operation.
    Request(ApprovalRequestArgs),
    /// Approve or reject an approval request.
    Respond(ApprovalRespondArgs),
    /// Record operation outcome for an approval request.
    Outcome(ApprovalOutcomeArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ApprovalRequestArgs {
    #[arg(long, value_name = "TYPE", help = "Operation type, for example force_push or remove_worktree.")]
    pub(crate) operation_type: String,
    #[arg(long, value_name = "REPO", help = "Repository name.")]
    pub(crate) repo_name: String,
    #[arg(long, value_name = "JSON", help = "Optional JSON context payload.")]
    pub(crate) context_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ApprovalRespondArgs {
    #[arg(long, value_name = "ID", help = "Approval request identifier.")]
    pub(crate) request_id: String,
    #[arg(long, help = "Set to true to approve, false to reject.")]
    pub(crate) approved: bool,
    #[arg(long, value_name = "TEXT", help = "Optional reviewer comment.")]
    pub(crate) comment: Option<String>,
    #[arg(long, value_name = "USER", help = "Reviewer user id.")]
    pub(crate) user_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ApprovalOutcomeArgs {
    #[arg(long, value_name = "ID", help = "Approval request identifier.")]
    pub(crate) request_id: String,
    #[arg(long, help = "Whether the operation succeeded.")]
    pub(crate) success: bool,
    #[arg(long, value_name = "TEXT", help = "Outcome message.")]
    pub(crate) message: String,
    #[arg(long, value_name = "JSON", help = "Optional JSON metadata payload.")]
    pub(crate) metadata_json: Option<String>,
}
