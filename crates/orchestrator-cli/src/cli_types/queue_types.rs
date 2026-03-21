use clap::{Args, Subcommand};

use super::INPUT_JSON_PRECEDENCE_HELP;

#[derive(Debug, Subcommand)]
pub(crate) enum QueueCommand {
    /// List queued dispatches.
    List,
    /// Show queue statistics.
    Stats,
    /// Enqueue a subject dispatch for a task, requirement, or custom title.
    ///
    /// Examples:
    ///   ao queue enqueue --task-id TASK-001
    ///   ao queue enqueue --requirement-id REQ-042 --workflow-ref ops
    ///   ao queue enqueue --title "Investigate flaky test" --description "Suite fails intermittently on CI"
    Enqueue(QueueEnqueueArgs),
    /// Hold a queued subject.
    Hold(QueueSubjectArgs),
    /// Release a held queued subject.
    Release(QueueSubjectArgs),
    /// Drop (remove) a queued subject dispatch regardless of status.
    Drop(QueueSubjectArgs),
    /// Reorder queued subjects by subject id.
    Reorder(QueueReorderArgs),
}

#[derive(Debug, Args)]
pub(crate) struct QueueEnqueueArgs {
    #[arg(
        long,
        value_name = "TASK_ID",
        group = "subject",
        help = "Task subject to enqueue (e.g. TASK-001). Mutually exclusive with --requirement-id / --title."
    )]
    pub(crate) task_id: Option<String>,
    #[arg(
        long,
        value_name = "REQ_ID",
        group = "subject",
        help = "Requirement subject to enqueue (e.g. REQ-042). Mutually exclusive with --task-id / --title."
    )]
    pub(crate) requirement_id: Option<String>,
    #[arg(
        long,
        value_name = "TITLE",
        group = "subject",
        help = "Custom subject title for ad-hoc dispatches. Mutually exclusive with --task-id / --requirement-id."
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
