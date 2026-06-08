use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum CostCommand {
    /// Show aggregate spend over a window (default: last 24h).
    Summary(CostSummaryArgs),
    /// Show the per-phase breakdown for a single workflow run id.
    Workflow(CostWorkflowArgs),
    /// Show the top N workflow runs by tokens or cost.
    Top(CostTopArgs),
    /// Aggregate tokens + cost over recent daily/weekly/monthly windows.
    Trends(CostTrendsArgs),
}

#[derive(Debug, Args)]
pub(crate) struct CostSummaryArgs {
    /// Lookback window. Accepts `30m`, `12h`, `7d`, `2w`. Defaults to `24h`.
    #[arg(long, value_name = "DURATION")]
    pub(crate) since: Option<String>,
    /// Cap on top-spender rows printed in the text view.
    #[arg(long, default_value_t = 5)]
    pub(crate) top: usize,
}

#[derive(Debug, Args)]
pub(crate) struct CostWorkflowArgs {
    /// Workflow run id, e.g. `wf-standard-workflow-impl-1-abc123`.
    #[arg(value_name = "WORKFLOW_RUN_ID")]
    pub(crate) workflow_run_id: String,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum CostTopBy {
    Tokens,
    Cost,
}

#[derive(Debug, Args)]
pub(crate) struct CostTopArgs {
    /// Rank by total tokens or total USD cost.
    #[arg(long, value_enum, default_value_t = CostTopBy::Cost)]
    pub(crate) by: CostTopBy,
    /// Number of workflows to list.
    #[arg(long, default_value_t = 10)]
    pub(crate) limit: usize,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum CostTrendWindow {
    Day,
    Week,
    Month,
}

#[derive(Debug, Args)]
pub(crate) struct CostTrendsArgs {
    /// Bucket size: day, week, or month.
    #[arg(long, value_enum, default_value_t = CostTrendWindow::Day)]
    pub(crate) window: CostTrendWindow,
    /// Number of windows to include (most recent N).
    #[arg(long, default_value_t = 30)]
    pub(crate) n: usize,
}
