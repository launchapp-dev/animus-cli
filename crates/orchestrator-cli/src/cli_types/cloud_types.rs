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
    /// Manage deployments on ao-cloud.
    Deploy {
        #[command(subcommand)]
        command: DeployCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum DeployCommand {
    /// Create a new deployment
    Create(DeployCreateArgs),
    /// Destroy an existing deployment
    Destroy(DeployDestroyArgs),
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

#[derive(Debug, Parser)]
pub(crate) struct DeployCreateArgs {
    #[arg(long, help = "Application name for the deployment")]
    pub(crate) app_name: String,
    #[arg(long, help = "Deployment region (e.g., fra)")]
    pub(crate) region: String,
    #[arg(long, help = "Machine size (e.g., shared-cpu-1x, performance-1x)")]
    pub(crate) machine_size: String,
}

#[derive(Debug, Parser)]
pub(crate) struct DeployDestroyArgs {
    #[arg(long, help = "Application name of the deployment to destroy")]
    pub(crate) app_name: String,
}
