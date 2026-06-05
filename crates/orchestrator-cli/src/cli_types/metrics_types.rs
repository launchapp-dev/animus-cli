use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum MetricsCommand {
    /// Show the current opt-in state, install id, and pending event count.
    Status,
    /// Opt in to anonymous usage metrics. No-op if already enabled.
    Enable,
    /// Opt out of anonymous usage metrics and drop any pending events.
    Disable,
    /// Send any pending events to the metrics endpoint immediately.
    Flush,
}
