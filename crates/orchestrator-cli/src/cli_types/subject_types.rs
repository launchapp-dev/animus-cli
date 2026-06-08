use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum SubjectCommand {
    /// List subjects for a given kind via the active subject_backend plugin.
    ///
    /// Routes `<kind>/list` through the daemon's [`SubjectRouter`]. When no
    /// subject_backend plugin is installed for the requested kind the call
    /// fails with `NotFound`. Set
    /// `ANIMUS_DAEMON_DISABLE_SUBJECT_PLUGINS=1` to force every call to
    /// `NotFound` even when plugins are installed.
    List(SubjectListArgs),
    /// Fetch a single subject by id from the active subject_backend plugin.
    Get(SubjectGetArgs),
    /// Create a subject through the active subject_backend plugin.
    Create(SubjectCreateArgs),
    /// Update a subject through the active subject_backend plugin.
    Update(SubjectUpdateArgs),
    /// Return the highest-priority Ready subject for the given kind.
    ///
    /// Backed by the active subject_backend plugin for the resolved kind.
    /// Plugins opt in by implementing `<kind>/next`.
    /// Returns JSON `null` when no eligible subject exists.
    Next(SubjectNextArgs),
    /// Set the status of a subject by id through the active subject_backend.
    Status(SubjectStatusArgs),
    /// Delete a subject by id through the active subject_backend plugin.
    ///
    /// Routes `<kind>/delete` through the daemon's [`SubjectRouter`]. Backends
    /// that do not support delete return `BackendError::Unsupported` (the
    /// `SubjectBackend::delete` default impl restored in `animus-protocol`
    /// v0.5.7). Plugins that claim `supports_delete: true` honor it.
    Delete(SubjectDeleteArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SubjectListArgs {
    /// Subject kind to route through (e.g. `task`, `issue`, `linear`).
    /// Resolved against the kind→plugin map populated at daemon startup.
    /// When omitted, falls back to `default_subject_kind` in
    /// `.animus/config.json` (defaults to `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,

    /// Filter by normalized status (e.g. `ready`, `in_progress`,
    /// `blocked`, `done`). Backend-specific raw statuses can be queried
    /// via the structured filter once we expose `--native-status`; for
    /// v0.4.0 the CLI only forwards the normalized bucket.
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,

    /// Maximum number of subjects to return. Forwarded to the backend's
    /// list call via `SubjectFilter.limit`.
    #[arg(long, value_name = "N")]
    pub limit: Option<u32>,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectGetArgs {
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Backend-qualified subject id (e.g. `sqlite:01ABCD...`,
    /// `linear:ENG-123`).
    #[arg(long, value_name = "ID")]
    pub id: String,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectCreateArgs {
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
}

#[derive(Debug, Args)]
pub(crate) struct SubjectUpdateArgs {
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Backend-qualified subject id.
    #[arg(long, value_name = "ID")]
    pub id: String,
    /// New normalized status.
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,
    /// New priority bucket.
    #[arg(long, value_name = "PRIORITY")]
    pub priority: Option<String>,
    /// Replace labels with this comma-separated list.
    #[arg(long, value_name = "L1,L2", value_delimiter = ',')]
    pub labels: Vec<String>,
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
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Backend-qualified subject id.
    #[arg(long, value_name = "ID")]
    pub id: String,
    /// New normalized status to set.
    #[arg(long, value_name = "STATUS")]
    pub status: String,
}

#[derive(Debug, Args)]
pub(crate) struct SubjectDeleteArgs {
    /// Subject kind to route through. When omitted, falls back to
    /// `default_subject_kind` in `.animus/config.json` (defaults to
    /// `task`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Backend-qualified subject id to delete.
    #[arg(long, value_name = "ID")]
    pub id: String,
    /// Confirm the destructive operation. Required to actually delete;
    /// without it the command prints what would be deleted and exits 0.
    #[arg(long)]
    pub yes: bool,
}
