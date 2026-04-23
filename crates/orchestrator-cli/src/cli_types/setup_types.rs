use clap::{ArgAction, Args};

#[derive(Debug, Args)]
pub(crate) struct SetupArgs {
    #[arg(
        long,
        help = "Run without prompts. Requires explicit --auto-merge, --auto-pr, and --auto-commit-before-merge values."
    )]
    pub(crate) non_interactive: bool,
    #[arg(
        long,
        help = "Preview setup changes without writing config. In non-interactive contexts, unspecified values default to the current daemon config."
    )]
    pub(crate) plan: bool,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_merge: Option<bool>,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_pr: Option<bool>,
    #[arg(long, action = ArgAction::Set)]
    pub(crate) auto_commit_before_merge: Option<bool>,
    #[arg(long, help = "Apply safe doctor remediations after setup changes.")]
    pub(crate) doctor_fix: bool,
}
