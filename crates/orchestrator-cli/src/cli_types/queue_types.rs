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
    ///   animus queue enqueue --task-id TASK-001
    ///   animus queue enqueue --requirement-id REQ-042 --workflow-ref ops
    ///   animus queue enqueue --title "Investigate flaky test" --description "Suite fails intermittently on CI"
    Enqueue(QueueEnqueueArgs),
    /// Hold one or more queued subjects.
    Hold(QueueSubjectArgs),
    /// Release one or more held queued subjects.
    Release(QueueSubjectArgs),
    /// Drop (remove) one or more queued subject dispatches regardless of status.
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
    #[arg(
        value_name = "SUBJECT_ID",
        required_unless_present_any = ["subject_id", "all"],
        conflicts_with = "all",
        help = "Queued subject identifiers (one or more)."
    )]
    pub(crate) subject_ids: Vec<String>,
    #[arg(
        long = "subject-id",
        value_name = "SUBJECT_ID",
        conflicts_with = "all",
        help = "Queued subject identifier (flag form; may be combined with positional ids)."
    )]
    pub(crate) subject_id: Option<String>,
    #[arg(
        long,
        help = "Target every queue entry matching this verb's eligible statuses. Mutually exclusive with explicit subject ids."
    )]
    pub(crate) all: bool,
    #[arg(long, help = "Skip the confirmation prompt required by --all. Only valid together with --all.")]
    pub(crate) yes: bool,
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
