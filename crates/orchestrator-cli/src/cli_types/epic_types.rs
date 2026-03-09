use clap::{Args, Subcommand};

use super::{IdArgs, INPUT_JSON_PRECEDENCE_HELP, TASK_PRIORITY_HELP};

#[derive(Debug, Subcommand)]
pub(crate) enum EpicCommand {
    /// List epics.
    List,
    /// Get an epic by id.
    Get(IdArgs),
    /// Create an epic.
    Create(EpicCreateArgs),
    /// Update an epic.
    Update(EpicUpdateArgs),
    /// Delete an epic.
    Delete(IdArgs),
}

#[derive(Debug, Args)]
pub(crate) struct EpicCreateArgs {
    #[arg(long, value_name = "TITLE", help = "Epic title.")]
    pub(crate) title: String,
    #[arg(
        long,
        value_name = "TEXT",
        default_value = "",
        help = "Epic description."
    )]
    pub(crate) description: String,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(
        long,
        value_name = "STATUS",
        help = "Epic status: backlog|todo|in-progress|done|on-hold|cancelled."
    )]
    pub(crate) status: Option<String>,
    #[arg(
        long,
        value_name = "SOURCE",
        help = "Source describing where this epic originated."
    )]
    pub(crate) source: Option<String>,
    #[arg(
        long = "tag",
        value_name = "TAG",
        help = "Tags for the epic. Repeat to add multiple values."
    )]
    pub(crate) tag: Vec<String>,
    #[arg(
        long = "linked-requirement-id",
        value_name = "REQ_ID",
        help = "Requirement ids linked to the epic. Repeat to add multiple ids."
    )]
    pub(crate) linked_requirement_id: Vec<String>,
    #[arg(
        long = "linked-task-id",
        value_name = "TASK_ID",
        help = "Task ids linked to the epic. Repeat to add multiple ids."
    )]
    pub(crate) linked_task_id: Vec<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct EpicUpdateArgs {
    #[arg(long, value_name = "EPIC_ID", help = "Epic identifier.")]
    pub(crate) id: String,
    #[arg(long, value_name = "TITLE", help = "Updated epic title.")]
    pub(crate) title: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Updated epic description.")]
    pub(crate) description: Option<String>,
    #[arg(long, value_name = "PRIORITY", help = TASK_PRIORITY_HELP)]
    pub(crate) priority: Option<String>,
    #[arg(
        long,
        value_name = "STATUS",
        help = "Epic status: backlog|todo|in-progress|done|on-hold|cancelled."
    )]
    pub(crate) status: Option<String>,
    #[arg(
        long,
        value_name = "SOURCE",
        help = "Updated source describing where this epic originated."
    )]
    pub(crate) source: Option<String>,
    #[arg(
        long = "tag",
        value_name = "TAG",
        help = "Tags to assign to the epic. Repeat to add multiple values."
    )]
    pub(crate) tag: Vec<String>,
    #[arg(
        long = "linked-requirement-id",
        value_name = "REQ_ID",
        help = "Requirement ids linked to the epic. Repeat to add multiple ids."
    )]
    pub(crate) linked_requirement_id: Vec<String>,
    #[arg(
        long = "linked-task-id",
        value_name = "TASK_ID",
        help = "Task ids linked to the epic. Repeat to add multiple ids."
    )]
    pub(crate) linked_task_id: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace all tags with the provided --tag values."
    )]
    pub(crate) replace_tags: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace all linked requirement ids with the provided --linked-requirement-id values."
    )]
    pub(crate) replace_linked_requirement_ids: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Replace all linked task ids with the provided --linked-task-id values."
    )]
    pub(crate) replace_linked_task_ids: bool,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}
