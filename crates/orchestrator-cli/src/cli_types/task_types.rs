use clap::{Args, Subcommand};

use super::{
    IdArgs, DEPENDENCY_TYPE_HELP, INPUT_JSON_PRECEDENCE_HELP, TASK_COMPLEXITY_HELP,
    TASK_IMPACT_AREA_HELP, TASK_PRIORITY_FILTER_HELP, TASK_PRIORITY_HELP, TASK_RISK_FILTER_HELP,
    TASK_RISK_HELP, TASK_SCOPE_HELP, TASK_STATUS_FILTER_HELP, TASK_STATUS_HELP,
    TASK_TYPE_FILTER_HELP, TASK_TYPE_HELP,
};

#[derive(Debug, Subcommand)]
pub(crate) enum TaskCommand {
    /// List tasks with optional filters.
    List(TaskListArgs),
    /// List tasks sorted by priority/urgency.
    Prioritized,
    /// Get the next ready task.
    Next,
    /// Show task statistics.
    Stats,
    /// Get a task by id.
    Get(IdArgs),
    /// Create a task.
    Create(TaskCreateArgs),
    /// Update a task.
    Update(TaskUpdateArgs),
    /// Delete a task (confirmation required).
    Delete(TaskDeleteArgs),
    /// Assign a generic assignee string to a task.
    Assign(TaskAssignArgs),
    /// Assign an agent role to a task.
    AssignAgent(TaskAssignAgentArgs),
    /// Assign a human user to a task.
    AssignHuman(TaskAssignHumanArgs),
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
    #[arg(
        long,
        value_name = "ASSIGNEE_TYPE",
        help = "Assignee type filter: agent|human|unassigned."
    )]
    pub(crate) assignee_type: Option<String>,
    #[arg(
        long,
        value_name = "TAG",
        help = "Match tasks that include all provided tags. Repeat to require multiple tags."
    )]
    pub(crate) tag: Vec<String>,
    #[arg(
        long,
        value_name = "REQ_ID",
        help = "Filter tasks linked to a requirement id."
    )]
    pub(crate) linked_requirement: Option<String>,
    #[arg(
        long,
        value_name = "ENTITY_ID",
        help = "Filter tasks linked to an architecture entity id."
    )]
    pub(crate) linked_architecture_entity: Option<String>,
    #[arg(
        long,
        value_name = "TEXT",
        help = "Case-insensitive text search over task title and description."
    )]
    pub(crate) search: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TaskCreateArgs {
    #[arg(long, value_name = "TITLE", help = "Task title.")]
    pub(crate) title: String,
    #[arg(
        long,
        value_name = "TEXT",
        default_value = "",
        help = "Task description."
    )]
    pub(crate) description: String,
    #[arg(long, value_name = "TYPE", help = TASK_TYPE_HELP)]
    pub(crate) task_type: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(long, value_name = "RISK", help = TASK_RISK_HELP)]
    pub(crate) risk: Option<String>,
    #[arg(long, value_name = "SCOPE", help = TASK_SCOPE_HELP)]
    pub(crate) scope: Option<String>,
    #[arg(long, value_name = "COMPLEXITY", help = TASK_COMPLEXITY_HELP)]
    pub(crate) complexity: Option<String>,
    #[arg(
        long = "impact-area",
        value_name = "AREA",
        help = "Impact areas for the task. Repeat to add multiple values."
    )]
    pub(crate) impact_area: Vec<String>,
    #[arg(
        long,
        value_name = "TEXT",
        help = "Estimated effort label (for example: 2d, 1 sprint, half-day)."
    )]
    pub(crate) estimated_effort: Option<String>,
    #[arg(
        long,
        value_name = "PERCENT",
        help = "Maximum CPU budget percentage (greater than 0 and up to 100)."
    )]
    pub(crate) max_cpu_percent: Option<f32>,
    #[arg(long, value_name = "MB", help = "Maximum memory budget in MB.")]
    pub(crate) max_memory_mb: Option<u64>,
    #[arg(
        long,
        value_name = "BOOL",
        help = "Whether the task requires network access."
    )]
    pub(crate) requires_network: Option<bool>,
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
    #[arg(long, value_name = "RISK", help = TASK_RISK_HELP)]
    pub(crate) risk: Option<String>,
    #[arg(long, value_name = "SCOPE", help = TASK_SCOPE_HELP)]
    pub(crate) scope: Option<String>,
    #[arg(long, value_name = "COMPLEXITY", help = TASK_COMPLEXITY_HELP)]
    pub(crate) complexity: Option<String>,
    #[arg(long = "impact-area", value_name = "AREA", help = TASK_IMPACT_AREA_HELP)]
    pub(crate) impact_area: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace all impact areas with the provided --impact-area values."
    )]
    pub(crate) replace_impact_area: bool,
    #[arg(
        long,
        value_name = "TEXT",
        help = "Updated estimated effort label (empty string clears value)."
    )]
    pub(crate) estimated_effort: Option<String>,
    #[arg(
        long,
        value_name = "PERCENT",
        help = "Updated maximum CPU budget percentage (greater than 0 and up to 100)."
    )]
    pub(crate) max_cpu_percent: Option<f32>,
    #[arg(long, value_name = "MB", help = "Updated maximum memory budget in MB.")]
    pub(crate) max_memory_mb: Option<u64>,
    #[arg(long, default_value_t = false, help = "Clear max CPU budget.")]
    pub(crate) clear_max_cpu_percent: bool,
    #[arg(long, default_value_t = false, help = "Clear max memory budget.")]
    pub(crate) clear_max_memory_mb: bool,
    #[arg(
        long,
        value_name = "BOOL",
        help = "Whether the task requires network access."
    )]
    pub(crate) requires_network: Option<bool>,
    #[arg(
        long,
        value_name = "ASSIGNEE",
        help = "Updated assignee value for the task."
    )]
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
    #[arg(
        long,
        value_name = "TASK_ID",
        help = "Confirmation token; must match --id."
    )]
    pub(crate) confirm: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Preview deletion payload without mutating task state."
    )]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TaskAssignArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "ASSIGNEE", help = "Assignee value.")]
    pub(crate) assignee: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskAssignAgentArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "ROLE", help = "Agent role identifier.")]
    pub(crate) role: String,
    #[arg(long, value_name = "MODEL", help = "Optional model override.")]
    pub(crate) model: Option<String>,
    #[arg(
        long,
        value_name = "USER",
        default_value = "ao-cli",
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskAssignHumanArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "USER_ID", help = "Human user id.")]
    pub(crate) user_id: String,
    #[arg(
        long,
        value_name = "USER",
        default_value = "ao-cli",
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
        default_value = "ao-cli",
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
        default_value = "ao-cli",
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDependencyAddArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "DEPENDENCY_ID",
        help = "Dependency task identifier."
    )]
    pub(crate) dependency_id: String,
    #[arg(long, value_name = "TYPE", help = DEPENDENCY_TYPE_HELP)]
    pub(crate) dependency_type: String,
    #[arg(
        long,
        value_name = "USER",
        default_value = "ao-cli",
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskDependencyRemoveArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(
        long,
        value_name = "DEPENDENCY_ID",
        help = "Dependency task identifier."
    )]
    pub(crate) dependency_id: String,
    #[arg(
        long,
        value_name = "USER",
        default_value = "ao-cli",
        help = "Audit user id recorded in task metadata."
    )]
    pub(crate) updated_by: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskStatusArgs {
    #[arg(long, value_name = "TASK_ID", help = "Task identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "STATUS", help = TASK_STATUS_HELP)]
    pub(crate) status: String,
}
