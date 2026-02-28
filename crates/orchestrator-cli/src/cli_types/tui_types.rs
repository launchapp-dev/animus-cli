use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct WorkflowMonitorArgs {
    #[arg(
        long,
        default_value = "2",
        value_parser = clap::value_parser!(u64).range(1..),
        help = "Workflow list refresh interval in seconds."
    )]
    pub(crate) refresh_interval: u64,
    #[arg(
        long,
        default_value = "500",
        help = "Maximum number of output lines to buffer."
    )]
    pub(crate) buffer_lines: usize,
    #[arg(
        long,
        value_name = "WORKFLOW_ID",
        help = "Pin monitor to a specific workflow ID."
    )]
    pub(crate) workflow_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TuiArgs {
    #[arg(
        long,
        value_name = "MODEL_ID",
        help = "Model id to use for interactive runs."
    )]
    pub(crate) model: Option<String>,
    #[arg(
        long,
        value_name = "TOOL",
        help = "CLI provider, such as codex or claude."
    )]
    pub(crate) tool: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Run without full-screen UI rendering."
    )]
    pub(crate) headless: bool,
    #[arg(
        long,
        value_name = "TEXT",
        help = "Optional initial prompt to pre-fill in the UI."
    )]
    pub(crate) prompt: Option<String>,
}
