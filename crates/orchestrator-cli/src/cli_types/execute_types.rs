use clap::{ArgAction, Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum ExecuteCommand {
    /// Generate task execution plan from requirements.
    Plan(ExecuteArgs),
    /// Generate and immediately run workflows from requirements.
    Run(ExecuteArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ExecuteArgs {
    #[arg(long = "id")]
    pub(crate) requirement_ids: Vec<String>,
    #[arg(long)]
    pub(crate) workflow_ref: Option<String>,
    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = false,
        help = "Deprecated no-op flag retained for CLI compatibility."
    )]
    pub(crate) ai_task_generation: bool,
    #[arg(long, action = ArgAction::Set, default_value_t = false)]
    pub(crate) include_wont: bool,
    #[arg(long)]
    pub(crate) input_json: Option<String>,
}
