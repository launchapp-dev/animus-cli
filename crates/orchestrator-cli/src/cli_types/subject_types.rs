use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

/// CLI mirror of the MCP batch `on_error` policy. `stop` (default) marks
/// every item after the first failure as skipped; `continue` processes
/// every item regardless of failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum BatchOnError {
    #[default]
    Stop,
    Continue,
}

impl BatchOnError {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            BatchOnError::Stop => "stop",
            BatchOnError::Continue => "continue",
        }
    }
    pub(crate) fn is_stop(self) -> bool {
        matches!(self, BatchOnError::Stop)
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum SubjectCommand {
    /// List subjects of a given kind.
    ///
    /// Filter by status with --status and cap results with --limit. Without
    /// --json the results print as a table; with --json they print the
    /// machine-readable envelope.
    List(SubjectListArgs),
    /// Fetch a single subject by id from the active subject_backend plugin.
    Get(SubjectGetArgs),
    /// Create a subject through the active subject_backend plugin.
    Create(SubjectCreateArgs),
    /// Create multiple subjects of one kind from a JSON items file.
    ///
    /// Mirrors the `animus.subject.batch-create` MCP tool: `--file` points
    /// at a JSON array of `{title, status?, priority?, labels?, body?}`
    /// items (max 100). Items run one at a time in order; `--on-error stop`
    /// (default) skips the remainder after the first failure,
    /// `--on-error continue` runs every item. Emits an
    /// `animus.cli.v1`-wrapped batch result with per-item outcomes.
    BatchCreate(SubjectBatchCreateArgs),
    /// Update a subject through the active subject_backend plugin.
    Update(SubjectUpdateArgs),
    /// Apply patches to multiple subjects of one kind from a JSON items file.
    ///
    /// Mirrors the `animus.subject.batch-update` MCP tool: `--file` points
    /// at a JSON array of `{id, status?, priority?, labels?}` items (max
    /// 100); each item needs at least one of status / priority / labels.
    /// `--on-error` semantics match `batch-create`.
    BatchUpdate(SubjectBatchUpdateArgs),
    /// Return the highest-priority ready subject of the given kind.
    ///
    /// Prints nothing actionable when no ready subject exists (JSON `null`
    /// under --json).
    Next(SubjectNextArgs),
    /// Set the status of a subject by id.
    Status(SubjectStatusArgs),
    /// Delete a subject by id.
    ///
    /// Not every kind supports deletion; kinds that do not will report the
    /// operation as unsupported.
    Delete(SubjectDeleteArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SubjectListArgs {
    /// Authenticated actor JSON. Selects the non-downgradable v2 subject wire.
    #[arg(long, value_name = "JSON")]
    pub actor_json: Option<String>,
    /// Subject kind to list (e.g. `task`, `issue`, `linear`). When omitted,
    /// falls back to `default_subject_kind` in `.animus/config.json`
    /// (defaults to `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,

    /// Filter by status (e.g. `ready`, `in_progress`, `blocked`, `done`).
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,

    /// Maximum subjects per page. Defaults to a bounded page so MCP/agent
    /// callers don't pull the whole set. Pass `--limit 0` to remove the per-page
    /// cap. The result carries `next_cursor` (and `total` when the backend
    /// reports it) — page with `--cursor` until `next_cursor` is null to read
    /// everything from a paginating backend.
    #[arg(long, value_name = "N")]
    pub limit: Option<u32>,

    /// Opaque cursor from a prior page's `next_cursor`, to fetch the next page.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,

    /// Case-insensitive substring filter on the subject TITLE. Looks a subject
    /// up by name without paging the whole set: the full set is fetched and
    /// filtered by title, then `--limit` is applied to the matches.
    #[arg(long, value_name = "TEXT")]
    pub query: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectGetArgs {
    /// Authenticated actor JSON. Selects the non-downgradable v2 subject wire.
    #[arg(long, value_name = "JSON")]
    pub actor_json: Option<String>,
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Subject id. Accepts either the bare native id (e.g. `TASK-001`) or
    /// the kind-qualified form (e.g. `task:TASK-001`, `linear:ENG-123`).
    #[arg(long, value_name = "ID")]
    pub id: String,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectCreateArgs {
    /// Authenticated actor JSON. The created subject is owned by this actor.
    #[arg(long, value_name = "JSON")]
    pub actor_json: Option<String>,
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Required title for the new subject.
    #[arg(long, value_name = "TITLE")]
    pub title: String,
    /// Optional normalized status to set on creation.
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,
    /// Optional priority bucket (e.g. `p0`, `p1`, `p2`, `p3`).
    #[arg(long, value_name = "PRIORITY")]
    pub priority: Option<String>,
    /// Comma-separated list of labels to attach.
    #[arg(long, value_name = "L1,L2", value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Optional free-form body / description.
    #[arg(long, value_name = "BODY")]
    pub body: Option<String>,
    /// Structured custom fields as a JSON object, merged into the subject's
    /// `data` (e.g. `--data '{"source":"krisp","occurred_at":"2026-07-09T21:00:00Z"}'`).
    /// Lets command-phase / scripted callers set declared kind fields that
    /// have no dedicated flag. Merges with (does not replace) other fields.
    #[arg(long, value_name = "JSON")]
    pub data: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectUpdateArgs {
    /// Authenticated actor JSON. Only an owned subject can be updated.
    #[arg(long, value_name = "JSON")]
    pub actor_json: Option<String>,
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Subject id. Accepts the bare native id (e.g. `TASK-001`) or the
    /// kind-qualified form (e.g. `task:TASK-001`).
    #[arg(long, value_name = "ID")]
    pub id: String,
    /// Rename the subject. Replaces the subject's title with this value.
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,
    /// New normalized status.
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,
    /// New priority bucket.
    #[arg(long, value_name = "PRIORITY")]
    pub priority: Option<String>,
    /// Replace labels with this comma-separated list.
    #[arg(long, value_name = "L1,L2", value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Replace the free-form body / description (markdown). Use this to write
    /// long-form content onto a subject (e.g. an agent's findings).
    #[arg(long, value_name = "BODY")]
    pub body: Option<String>,
    /// Structured custom fields as a JSON object, merged into the subject's
    /// `data` (e.g. `--data '{"source":"krisp","occurred_at":"2026-07-09T21:00:00Z"}'`).
    /// Lets command-phase / scripted callers set declared kind fields that
    /// have no dedicated flag. Merges with (does not replace) other fields.
    #[arg(long, value_name = "JSON")]
    pub data: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectBatchCreateArgs {
    /// Authenticated actor JSON applied to every item.
    #[arg(long, value_name = "JSON")]
    pub actor_json: Option<String>,
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Path to a JSON file containing the items array. Each item is
    /// `{"title": "...", "status"?, "priority"?, "labels"?: [..], "body"?}`.
    /// Maximum 100 items.
    #[arg(long, value_name = "JSON")]
    pub file: PathBuf,
    /// Error policy: `stop` (default) skips remaining items after the first
    /// failure; `continue` processes every item.
    #[arg(long, value_name = "POLICY", default_value = "stop")]
    pub on_error: BatchOnError,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectBatchUpdateArgs {
    /// Authenticated actor JSON applied to every item.
    #[arg(long, value_name = "JSON")]
    pub actor_json: Option<String>,
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Path to a JSON file containing the items array. Each item is
    /// `{"id": "...", "status"?, "priority"?, "labels"?: [..]}` and must
    /// carry at least one of status / priority / labels. Maximum 100 items.
    #[arg(long, value_name = "JSON")]
    pub file: PathBuf,
    /// Error policy: `stop` (default) skips remaining items after the first
    /// failure; `continue` processes every item.
    #[arg(long, value_name = "POLICY", default_value = "stop")]
    pub on_error: BatchOnError,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectNextArgs {
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectStatusArgs {
    /// Authenticated actor JSON. Only an owned subject can be changed.
    #[arg(long, value_name = "JSON")]
    pub actor_json: Option<String>,
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Subject id. Accepts the bare native id (e.g. `TASK-001`) or the
    /// kind-qualified form (e.g. `task:TASK-001`).
    #[arg(long, value_name = "ID")]
    pub id: String,
    /// New normalized status to set.
    #[arg(long, value_name = "STATUS")]
    pub status: String,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectDeleteArgs {
    /// Authenticated actor JSON. Only an owned subject can be deleted.
    #[arg(long, value_name = "JSON")]
    pub actor_json: Option<String>,
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Subject id to delete. Accepts the bare native id (e.g. `TASK-001`)
    /// or the kind-qualified form (e.g. `task:TASK-001`).
    #[arg(long, value_name = "ID")]
    pub id: String,
    /// Confirm the destructive operation. Required to actually delete;
    /// without it the command prints what would be deleted and exits 0.
    #[arg(long)]
    pub yes: bool,
}
