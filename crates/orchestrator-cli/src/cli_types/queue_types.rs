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
    #[arg(long, value_name = "TASK_ID", help = "Task subject to enqueue.")]
    pub(crate) task_id: String,
    #[arg(
        long = "pipeline",
        alias = "pipeline-id",
        value_name = "PIPELINE",
        help = "Optional pipeline override. Defaults to the task pipeline."
    )]
    pub(crate) pipeline_id: Option<String>,
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
