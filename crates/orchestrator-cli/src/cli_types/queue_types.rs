use clap::{Args, Subcommand};

use super::INPUT_JSON_PRECEDENCE_HELP;

#[derive(Debug, Subcommand)]
pub(crate) enum QueueCommand {
    /// List queued dispatches.
    List,
    /// Show queue statistics.
    Stats,
    /// Enqueue a task-backed subject dispatch.
    Enqueue(QueueEnqueueArgs),
    /// Hold a queued subject.
    Hold(QueueSubjectArgs),
    /// Release a held queued subject.
    Release(QueueSubjectArgs),
    /// Reorder queued subjects by subject id.
    Reorder(QueueReorderArgs),
}

#[derive(Debug, Args)]
pub(crate) struct QueueEnqueueArgs {
    #[arg(
        long,
        value_name = "TASK_ID",
        group = "subject",
        help = "Task subject to enqueue (mutually exclusive with --requirement-id / --title)."
    )]
    pub(crate) task_id: Option<String>,
    #[arg(
        long,
        value_name = "REQ_ID",
        group = "subject",
        help = "Requirement subject to enqueue (mutually exclusive with --task-id / --title)."
    )]
    pub(crate) requirement_id: Option<String>,
    #[arg(
        long,
        value_name = "TITLE",
        group = "subject",
        help = "Custom subject title (mutually exclusive with --task-id / --requirement-id)."
    )]
    pub(crate) title: Option<String>,
    #[arg(long, value_name = "TEXT", help = "Custom subject description (used with --title).")]
    pub(crate) description: Option<String>,
    #[arg(long = "workflow-ref", value_name = "WORKFLOW_REF", help = "Optional YAML workflow reference override.")]
    pub(crate) workflow_ref: Option<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct QueueSubjectArgs {
    #[arg(long, value_name = "SUBJECT_ID", help = "Queued subject identifier.")]
    pub(crate) subject_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct QueueReorderArgs {
    #[arg(
        long = "subject-id",
        value_name = "SUBJECT_ID",
        help = "Ordered queued subject ids. Repeat to provide the desired order."
    )]
    pub(crate) subject_ids: Vec<String>,
}
