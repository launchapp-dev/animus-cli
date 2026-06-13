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
    /// Show token + USD spend for a single chat conversation (v0.5.10).
    Conversation(CostConversationArgs),
    /// List recorded budget-cap breaches from the scoped breach log.
    Decisions(CostDecisionsArgs),
}

#[derive(Debug, Args)]
pub(crate) struct CostDecisionsArgs {
    /// Only show breaches observed inside this window. Accepts `30m`, `12h`, `7d`, `2w`.
    #[arg(long, value_name = "DURATION")]
    pub(crate) since: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct CostConversationArgs {
    /// Chat conversation id, e.g. `conv-abc123`.
    #[arg(value_name = "CONVERSATION_ID")]
    pub(crate) conversation_id: String,
}

/// Grouping dimension for `cost summary` breakdown views.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum CostSummaryBy {
    /// Group spend by provider / tool (claude, codex, gemini, ...).
    Provider,
    /// Group spend by model id.
    Model,
    /// Group spend by the subject / task each run was for.
    Task,
}

#[derive(Debug, Args)]
pub(crate) struct CostSummaryArgs {
    /// Lookback window. Accepts `30m`, `12h`, `7d`, `2w`. Defaults to `24h`.
    #[arg(long, value_name = "DURATION")]
    pub(crate) since: Option<String>,
    /// Cap on top-spender rows printed in the text view.
    #[arg(long, default_value_t = 5)]
    pub(crate) top: usize,
    /// Group totals by provider, model, or task instead of the workflow leaderboard.
    #[arg(long, value_enum)]
    pub(crate) by: Option<CostSummaryBy>,
    /// Report each run's full lifetime spend if it was touched in the window,
    /// instead of only the spend incurred inside the window. Restores the
    /// pre-v0.5.x summary semantics.
    #[arg(long)]
    pub(crate) lifetime: bool,
}

/// Grouping dimension for the `cost workflow` breakdown view.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum CostWorkflowBy {
    /// Group spend by provider / tool.
    Provider,
    /// Group spend by model id.
    Model,
    /// Group spend by phase id (the default rollup).
    Phase,
}

#[derive(Debug, Args)]
pub(crate) struct CostWorkflowArgs {
    /// Workflow run id, e.g. `wf-standard-workflow-impl-1-abc123`.
    #[arg(value_name = "WORKFLOW_RUN_ID")]
    pub(crate) workflow_run_id: String,
    /// Group the per-workflow rollup by provider, model, or phase.
    #[arg(long, value_enum)]
    pub(crate) by: Option<CostWorkflowBy>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum CostTopBy {
    Tokens,
    Cost,
    /// Rank model groups by total USD cost.
    Model,
    /// Rank provider / tool groups by total USD cost.
    Provider,
}

#[derive(Debug, Args)]
pub(crate) struct CostTopArgs {
    /// Rank by total tokens, total USD cost, or model/provider group.
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
