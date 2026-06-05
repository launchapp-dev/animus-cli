use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum MetricsCommand {
    /// Show the current opt-in state, install id, and pending event count.
    Status,
    /// Opt in to anonymous usage telemetry (skips re-prompting on first run).
    Enable,
    /// Opt out of anonymous usage telemetry. Drops any buffered events.
    Disable,
    /// Force-send any buffered events to the configured endpoint. Debug helper.
    Flush,
}
