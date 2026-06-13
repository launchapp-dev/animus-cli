use clap::{Args, Subcommand};

use super::{parse_duration_secs_default_seconds, IdArgs};

#[derive(Debug, Subcommand)]
pub(crate) enum HistoryCommand {
    /// List history records for a task.
    Task(HistoryTaskArgs),
    /// Get a history record by id.
    Get(IdArgs),
    /// List recent history records.
    Recent(HistoryRecentArgs),
    /// Search history records.
    Search(HistorySearchArgs),
    /// Remove old history records.
    Cleanup(HistoryCleanupArgs),
}

#[derive(Debug, Args)]
pub(crate) struct HistoryTaskArgs {
    #[arg(long)]
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct HistoryRecentArgs {
    /// Maximum number of recent records to return (default: 100).
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct HistorySearchArgs {
    #[arg(long)]
    pub(crate) task_id: Option<String>,
    #[arg(long)]
    pub(crate) workflow_id: Option<String>,
    #[arg(long)]
    pub(crate) status: Option<String>,
    /// RFC3339 lower bound on started_at (e.g. 2026-06-01T00:00:00Z).
    #[arg(long)]
    pub(crate) started_after: Option<String>,
    /// RFC3339 upper bound on started_at.
    #[arg(long)]
    pub(crate) started_before: Option<String>,
    /// Relative window: only records started within the last DURATION
    /// (e.g. 7d, 12h, 30m, 90s; bare numbers are seconds). Mutually
    /// exclusive with --started-after.
    #[arg(long, value_name = "DURATION", value_parser = parse_duration_secs_default_seconds, conflicts_with = "started_after")]
    pub(crate) since: Option<u64>,
    #[arg(long)]
    pub(crate) limit: Option<usize>,
    #[arg(long)]
    pub(crate) offset: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct HistoryCleanupArgs {
    #[arg(long, default_value_t = 30)]
    pub(crate) days: i64,
}
