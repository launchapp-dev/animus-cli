use clap::{Args, Subcommand};

use super::{
    parse_percentage_u8, parse_positive_u64, parse_positive_usize, IdArgs, DEPENDENCY_TYPE_HELP,
    INPUT_JSON_PRECEDENCE_HELP, TASK_PRIORITY_FILTER_HELP, TASK_PRIORITY_HELP, TASK_RISK_FILTER_HELP, TASK_SORT_HELP,
    TASK_STATUS_FILTER_HELP, TASK_STATUS_HELP, TASK_TYPE_FILTER_HELP, TASK_TYPE_HELP,
};

#[derive(Debug, Subcommand)]
pub(crate) enum TaskCommand {
    /// List tasks with optional filters.
    List(TaskListArgs),
    /// Get the next ready task.
    Next,
    /// Show task statistics.
    Stats(TaskStatsArgs),
    /// Get a task by id.
    Get(IdArgs),
    /// Create a task.
    Create(TaskCreateArgs),
    /// Update a task.
    Update(TaskUpdateArgs),
    /// Delete a task (confirmation required).
    Delete(TaskDeleteArgs),
    /// Assign an assignee to a task.
    Assign(TaskAssignArgs),
    /// Add a checklist item.
    ChecklistAdd(TaskChecklistAddArgs),
    /// Mark a checklist item complete/incomplete.
    ChecklistUpdate(TaskChecklistUpdateArgs),
    /// Add a task dependency edge.
    DependencyAdd(TaskDependencyAddArgs),
    /// Remove a task dependency edge.
    DependencyRemove(TaskDependencyRemoveArgs),
    /// Set task status.
    Status(TaskStatusArgs),
    /// Show workflow dispatch history for a task.
    History(IdArgs),
    /// Pause a task.
    Pause(IdArgs),
    /// Resume a paused task.
    Resume(IdArgs),
    /// Cancel a task (confirmation required).
    Cancel(TaskCancelArgs),
    /// Reopen a task from terminal state (Done/Cancelled) back to Backlog.
    Reopen(TaskReopenArgs),
    /// Set task priority.
    SetPriority(TaskSetPriorityArgs),
    /// Set or clear task deadline.
    SetDeadline(TaskSetDeadlineArgs),
    /// Rebalance task priorities using a high-priority budget policy.
    RebalancePriority(TaskRebalancePriorityArgs),
}

#[derive(Debug, Args)]
pub(crate) struct TaskCancelArgs {
    #[arg(short, long, visible_alias = "task-id", value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TASK_ID", help = "Confirmation token; must match --id.")]
    pub(crate) confirm: Option<String>,
    #[arg(long, default_value_t = false, help = "Preview cancellation payload without mutating task state.")]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TaskReopenArgs {
    #[arg(short, long, visible_alias = "task-id", value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TASK_ID", help = "Confirmation token; must match --id.")]
    pub(crate) confirm: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskSetPriorityArgs {
    #[arg(short, long, visible_alias = "task-id", value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(short, long, value_name = "PRIORITY", help = TASK_PRIORITY_HELP)]
    pub(crate) priority: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskSetDeadlineArgs {
    #[arg(short, long, visible_alias = "task-id", value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "RFC3339", help = "Deadline timestamp (RFC 3339), for example 2026-03-01T09:30:00Z.")]
    pub(crate) deadline: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskRebalancePriorityArgs {
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
    #[arg(long, value_name = "TOKEN", help = "Confirmation token required with --apply. Use 'apply'.")]
    pub(crate) confirm: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskStatsArgs {
    #[arg(
        long,
        value_name = "HOURS",
        default_value_t = 24,
        value_parser = parse_positive_u64,
        help = "Flag in-progress tasks as stale when updated_at age is at least this many hours."
    )]
    pub(crate) stale_threshold_hours: u64,
}

#[derive(Debug, Args)]
pub(crate) struct TaskListArgs {
    #[arg(long, value_name = "TYPE", help = TASK_TYPE_FILTER_HELP)]
    pub(crate) task_type: Option<String>,
    #[arg(long, value_name = "STATUS", help = TASK_STATUS_FILTER_HELP)]
    pub(crate) status: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_FILTER_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(long, value_name = "RISK", help = TASK_RISK_FILTER_HELP)]
    pub(crate) risk: Option<String>,
    #[arg(long, value_name = "ASSIGNEE_TYPE", help = "Assignee type filter: agent|human|unassigned.")]
    pub(crate) assignee_type: Option<String>,
    #[arg(
        long,
        value_name = "TAG",
        help = "Match tasks that include all provided tags. Repeat to require multiple tags."
    )]
    pub(crate) tag: Vec<String>,
    #[arg(long, value_name = "REQ_ID", help = "Filter tasks linked to a requirement id.")]
    pub(crate) linked_requirement: Option<String>,
    #[arg(long, value_name = "ENTITY_ID", help = "Filter tasks linked to an architecture entity id.")]
    pub(crate) linked_architecture_entity: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Case-insensitive text search over task title and description.")]
    pub(crate) search: Option<String>,
    #[arg(long, value_name = "SORT", help = TASK_SORT_HELP)]
    pub(crate) sort: Option<String>,
    #[arg(
        long,
        value_name = "COUNT",
        value_parser = parse_positive_usize,
        help = "Maximum number of tasks to return."
    )]
    pub(crate) limit: Option<usize>,
    #[arg(long, value_name = "COUNT", default_value_t = 0, help = "Number of tasks to skip before returning results.")]
    pub(crate) offset: usize,
}

#[derive(Debug, Args)]
pub(crate) struct TaskCreateArgs {
    #[arg(short, long, value_name = "TITLE", help = "Task title.")]
    pub(crate) title: String,
    #[arg(long, value_name = "TEXT", help = "Task description.")]
    pub(crate) description: Option<String>,
    #[arg(long, value_name = "TYPE", help = TASK_TYPE_HELP)]
    pub(crate) task_type: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(
        long = "linked-requirement",
        value_name = "REQ_ID",
        help = "Link requirement ids to the new task. Repeat to add multiple ids."
    )]
    pub(crate) linked_requirement: Vec<String>,
    #[arg(
        long = "linked-architecture-entity",
        value_name = "ENTITY_ID",
        help = "Link architecture entity ids to the new task. Repeat to add multiple ids."
    )]
    pub(crate) linked_architecture_entity: Vec<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskUpdateArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TITLE", help = "Updated task title.")]
    pub(crate) title: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Updated task description.")]
    pub(crate) description: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(long, value_name = "STATUS", help = TASK_STATUS_HELP)]
    pub(crate) status: Option<String>,
    #[arg(long, value_name = "ASSIGNEE", help = "Updated assignee value for the task.")]
    pub(crate) assignee: Option<String>,
    #[arg(
        long = "linked-architecture-entity",
        value_name = "ENTITY_ID",
        help = "Architecture entity ids to link. Repeat to add multiple ids."
    )]
    pub(crate) linked_architecture_entity: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace all linked architecture entities with the provided --linked-architecture-entity values."
    )]
    pub(crate) replace_linked_architecture_entities: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDeleteArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TASK_ID", help = "Confirmation token; must match --id.")]
    pub(crate) confirm: Option<String>,
    #[arg(long, default_value_t = false, help = "Preview deletion payload without mutating task state.")]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TaskAssignArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "ASSIGNEE", help = "Assignee identifier (user id or agent role).")]
    pub(crate) assignee: String,
    #[arg(long = "type", value_name = "TYPE", help = "Assignee type: agent|human.")]
    pub(crate) assignee_type: Option<String>,
    #[arg(long = "agent-role", value_name = "ROLE", help = "Agent role identifier.")]
    pub(crate) agent_role: Option<String>,
    #[arg(long, value_name = "MODEL", help = "Optional model override (agent only).")]
    pub(crate) model: Option<String>,
    #[arg(
        long,
        value_name = "USER",
        default_value = protocol::ACTOR_CLI,
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskChecklistAddArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TEXT", help = "Checklist item text.")]
    pub(crate) description: String,
    #[arg(
        long,
        value_name = "USER",
        default_value = protocol::ACTOR_CLI,
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskChecklistUpdateArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "ITEM_ID", help = "Checklist item identifier.")]
    pub(crate) item_id: String,
    #[arg(long, help = "Set to true to mark complete, false to mark incomplete.")]
    pub(crate) completed: bool,
    #[arg(
        long,
        value_name = "USER",
        default_value = protocol::ACTOR_CLI,
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDependencyAddArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "DEPENDENCY_ID", help = "Dependency task identifier.")]
    pub(crate) dependency_id: String,
    #[arg(long, value_name = "TYPE", help = DEPENDENCY_TYPE_HELP)]
    pub(crate) dependency_type: String,
    #[arg(
        long,
        value_name = "USER",
        default_value = protocol::ACTOR_CLI,
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDependencyRemoveArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "DEPENDENCY_ID", help = "Dependency task identifier.")]
    pub(crate) dependency_id: String,
    #[arg(
        long,
        value_name = "USER",
        default_value = protocol::ACTOR_CLI,
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskStatusArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(short, long, value_name = "STATUS", help = TASK_STATUS_HELP)]
    pub(crate) status: String,
}
