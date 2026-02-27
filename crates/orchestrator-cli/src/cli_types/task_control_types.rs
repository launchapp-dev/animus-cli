use clap::{Args, Subcommand};

use super::{parse_percentage_u8, TaskIdArgs, TASK_PRIORITY_HELP};

#[derive(Debug, Subcommand)]
pub(crate) enum TaskControlCommand {
    /// Pause a task.
    Pause(TaskIdArgs),
    /// Resume a paused task.
    Resume(TaskIdArgs),
    /// Cancel a task (confirmation required).
    Cancel(TaskControlCancelArgs),
    /// Set task priority.
    SetPriority(TaskControlPriorityArgs),
    /// Set or clear task deadline.
    SetDeadline(TaskControlDeadlineArgs),
    /// Rebalance task priorities using a high-priority budget policy.
    RebalancePriority(TaskControlRebalancePriorityArgs),
}

#[derive(Debug, Args)]

pub(crate) struct TaskControlCancelArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) task_id: String,
    #[arg(
        long,
        value_name = "TASK_ID",
        help = "Confirmation token; must match --task-id."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview cancellation payload without mutating task state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TaskControlPriorityArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) task_id: String,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_HELP)]
    pub(crate) priority: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskControlDeadlineArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) task_id: String,
    #[arg(
        long,
        value_name = "RFC3339",
        help = "Deadline timestamp (RFC 3339), for example 2026-03-01T09:30:00Z."
    )]
    pub(crate) deadline: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskControlRebalancePriorityArgs {
    #[arg(
        long,
        value_name = "PERCENT",
        default_value_t = 20,
        value_parser = parse_percentage_u8,
        help = "Maximum percentage of active tasks allowed at high priority (0-100)."
    )]
    pub(crate) high_budget_percent: u8,
    #[arg(
        long = "essential-task-id",
        value_name = "TASK_ID",
        help = "Task ids to prioritize first when selecting high-priority tasks. Repeat to add multiple ids."
    )]
    pub(crate) essential_task_id: Vec<String>,
    #[arg(
        long = "nice-to-have-task-id",
        value_name = "TASK_ID",
        help = "Task ids to force low priority unless promoted to critical by blocked status. Repeat to add multiple ids."
    )]
    pub(crate) nice_to_have_task_id: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Apply planned priority changes. Without this flag, command runs in dry-run mode."
    )]
    pub(crate) apply: bool,
    #[arg(
        long,
        value_name = "TOKEN",
        help = "Confirmation token required with --apply. Use 'apply'."
    )]
    pub(crate) confirm: Option<String>,
}
