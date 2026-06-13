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
    /// Approve the request. Mutually exclusive with `--reject`; exactly
    /// one of the two is required (omitting both errors rather than
    /// silently rejecting).
    #[arg(long, group = "decision", help = "Approve the request.")]
    pub(crate) approve: bool,
    /// Reject the request. Mutually exclusive with `--approve`.
    #[arg(long, group = "decision", help = "Reject the request.")]
    pub(crate) reject: bool,
    #[arg(long, value_name = "TEXT", help = "Optional reviewer comment.")]
    pub(crate) comment: Option<String>,
    #[arg(long, value_name = "USER", help = "Reviewer user id.")]
    pub(crate) user_id: Option<String>,
}

impl ApprovalRespondArgs {
    /// Resolve the approve/reject decision. Returns an error when neither
    /// flag is given so an omitted decision can never silently reject.
    pub(crate) fn approved(&self) -> anyhow::Result<bool> {
        match (self.approve, self.reject) {
            (true, false) => Ok(true),
            (false, true) => Ok(false),
            // clap's `group` already rejects (true, true); this guards
            // the neither-given case with an actionable message.
            _ => Err(crate::invalid_input_error("provide exactly one of --approve or --reject")),
        }
    }
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
