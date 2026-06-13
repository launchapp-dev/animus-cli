use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum EventsCommand {
    /// Stream workflow lifecycle events (phase_started, phase_completed,
    /// workflow_completed, workflow_failed) from the daemon.
    ///
    /// Not sure which surface you need? Run `animus daemon observe` for the
    /// routing matrix.
    Tail(EventsTailArgs),
}

#[derive(Debug, Args)]
pub(crate) struct EventsTailArgs {
    /// Restrict the stream to a single workflow run id.
    #[arg(long, value_name = "ID")]
    pub(crate) workflow_id: Option<String>,
    /// Rewind window applied client-side (e.g. `5m`, `2h`). The daemon does
    /// not buffer historical events, so this filters incoming events whose
    /// `occurred_at` falls inside the window once the subscription is live.
    #[arg(long, value_name = "DURATION")]
    pub(crate) since: Option<String>,
    /// Emit one JSON object per line using the `animus.cli.v1` envelope.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
}
