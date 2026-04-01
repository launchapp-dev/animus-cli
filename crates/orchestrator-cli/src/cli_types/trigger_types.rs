use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum TriggerCommand {
    /// List all configured event triggers for this project.
    List,
    /// Manually fire a webhook trigger (for testing and development).
    ///
    /// Directly queues an event into the trigger's pending queue, bypassing
    /// the HTTP server.  The daemon will dispatch it on the next tick.
    Fire {
        /// The trigger ID to fire (must be a webhook or github_webhook trigger).
        trigger_id: String,
        /// Optional JSON payload to pass as the webhook event body.
        /// Defaults to `{}`.
        #[arg(long, default_value = "{}")]
        payload: String,
    },
}
