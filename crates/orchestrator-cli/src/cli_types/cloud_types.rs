use clap::{Parser, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum CloudCommand {
    /// Configure the sync server connection for this project.
    Setup(CloudSetupArgs),
    /// Push local tasks and requirements to the sync server.
    Push,
    /// Pull tasks and requirements from the sync server into local state.
    Pull,
    /// Show sync configuration and last sync status.
    Status,
    /// Link this project to a specific remote project by ID.
    Link(CloudLinkArgs),
}

#[derive(Debug, Parser)]
pub(crate) struct CloudSetupArgs {
    #[arg(long, help = "Sync server URL, e.g. https://ao-sync-production.up.railway.app")]
    pub(crate) server: String,
    #[arg(long, help = "Bearer token for authentication")]
    pub(crate) token: String,
}

#[derive(Debug, Parser)]
pub(crate) struct CloudLinkArgs {
    #[arg(long, help = "Remote project ID to link to")]
    pub(crate) project_id: String,
}
