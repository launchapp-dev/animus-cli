use clap::{Args, Subcommand};

use super::{parse_duration_secs_default_seconds, INPUT_JSON_PRECEDENCE_HELP};

#[derive(Debug, Subcommand)]
pub(crate) enum QueueCommand {
    /// List queued dispatches.
    List,
    /// Show queue statistics.
    Stats,
    /// Enqueue a subject dispatch for any subject kind or a custom title.
    ///
    /// Examples:
    ///   animus queue enqueue --subject-id TASK-001
    ///   animus queue enqueue --subject-id requirement:REQ-042 --workflow-ref ops
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
        value_name = "TITLE",
        group = "subject",
        help = "Custom subject title for ad-hoc dispatches. Mutually exclusive with --subject-id / --adhoc."
    )]
    pub(crate) title: Option<String>,
    #[arg(
        long = "subject-id",
        value_name = "SUBJECT_ID",
        group = "subject",
        help = "Subject to enqueue for any kind. Accepts a qualified id (task:TASK-001 / requirement:REQ-042 / blog:BLOG-001 — kind trusted; the recommended form) or a bare id (TASK-001 — kind probed across backends that declare concrete kinds; pure catch-all dynamic backends require the qualified form). Mutually exclusive with --title / --adhoc."
    )]
    pub(crate) subject_id: Option<String>,
    #[arg(
        long,
        group = "subject",
        help = "Dispatch a subjectless (ad-hoc) run with NO bound subject. The workflow runs without subject-bound template vars — use for subject-less-by-design workflows (e.g. relate) fired without a target. Requires --workflow-ref. Mutually exclusive with --subject-id / --title. NOTE: not yet supported through the installed queue plugin (its RPC protocol still requires a subject); dispatch such runs directly for now."
    )]
    pub(crate) adhoc: bool,
    #[arg(long, value_name = "TEXT", help = "Custom subject description (used with --title).")]
    pub(crate) description: Option<String>,
    #[arg(long = "workflow-ref", value_name = "WORKFLOW_REF", help = "Optional YAML workflow reference override.")]
    pub(crate) workflow_ref: Option<String>,
    #[arg(long, value_name = "JSON", help = INPUT_JSON_PRECEDENCE_HELP)]
    pub(crate) input_json: Option<String>,
    #[arg(
        long = "idempotency-key",
        value_name = "KEY",
        help = "Stable producer key for durable enqueue. An identical retry returns the original queue receipt; changed content with the same key fails closed."
    )]
    pub(crate) idempotency_key: Option<String>,
    #[arg(
        long = "at",
        value_name = "WHEN",
        help = "Defer dispatch until this time. Accepts an RFC 3339 timestamp (2026-06-13T15:00:00Z) or a relative offset (90s, 30m, 2h, 3d). The entry stays queued but is not dispatched until then."
    )]
    pub(crate) run_at: Option<String>,
    #[arg(
        long = "expire-after",
        value_name = "DURATION",
        requires = "run_at",
        value_parser = parse_duration_secs_default_seconds,
        help = "Grace window after --at (e.g. 10m, 1h; bare number = seconds). If the entry is still pending past --at + this window, it is dropped instead of dispatched late. Omit to always fire late."
    )]
    pub(crate) expire_after_secs: Option<u64>,
}

#[derive(Debug, Args)]
pub(crate) struct QueueSubjectArgs {
    #[arg(
        value_name = "SUBJECT_ID",
        required_unless_present_any = ["subject_id", "all"],
        conflicts_with = "all",
        help = "Queued subject ids (one or more). Each accepts a bare id (TASK-001) or the qualified form (task:TASK-001)."
    )]
    pub(crate) subject_ids: Vec<String>,
    #[arg(
        long = "subject-id",
        value_name = "SUBJECT_ID",
        conflicts_with = "all",
        help = "Queued subject id (flag form; may be combined with positional ids). Accepts bare or qualified form."
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
