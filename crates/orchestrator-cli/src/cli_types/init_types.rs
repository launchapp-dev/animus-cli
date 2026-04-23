use clap::{ArgAction, Args};

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    #[arg(long, value_name = "TEMPLATE_ID", help = "Bundled project template id to initialize from.", conflicts_with = "path")]
    pub(crate) template: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Local template directory containing template.toml.",
        conflicts_with = "template"
    )]
    pub(crate) path: Option<String>,
    #[arg(long, help = "Run without prompts. Requires --template or --path.")]
    pub(crate) non_interactive: bool,
    #[arg(long, help = "Preview init changes without writing project files.")]
    pub(crate) plan: bool,
    #[arg(long, help = "Overwrite existing project files targeted by the template.")]
    pub(crate) force: bool,
    #[arg(long, action = ArgAction::Set, help = "Override the template default for automatic merge.")]
    pub(crate) auto_merge: Option<bool>,
    #[arg(long, action = ArgAction::Set, help = "Override the template default for automatic pull request creation.")]
    pub(crate) auto_pr: Option<bool>,
    #[arg(long, action = ArgAction::Set, help = "Override the template default for automatic commit before merge.")]
    pub(crate) auto_commit_before_merge: Option<bool>,
}
