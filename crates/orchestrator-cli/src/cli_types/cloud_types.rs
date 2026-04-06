use clap::{Parser, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum CloudCommand {
    /// Authenticate with Animus Cloud via browser OAuth.
    Login(CloudLoginArgs),
    /// Configure the sync server connection for this project.
    Setup(CloudSetupArgs),
    /// Push local tasks, requirements, and workflow config to the sync server.
    Push,
    /// Pull tasks and requirements from the sync server into local state.
    Pull,
    /// Show sync configuration, cloud projects, daemon states, and active workflows.
    Status,
    /// Link this project to a specific remote project by ID.
    Link(CloudLinkArgs),
    /// Manage deployments on ao-cloud.
    Deploy {
        #[command(subcommand)]
        command: DeployCommand,
    },
}

#[derive(Debug, Parser)]
pub(crate) struct CloudLoginArgs {
    #[arg(long, env = "ANIMUS_CLOUD_URL", help = "Animus Cloud URL [default: https://animus.launchapp.dev] [env: ANIMUS_CLOUD_URL]")]
    pub(crate) server: Option<String>,
    #[arg(long, help = "Skip opening browser (print URL instead)")]
    pub(crate) no_browser: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum DeployCommand {
    /// Create a new deployment
    Create(DeployCreateArgs),
    /// Destroy an existing deployment
    Destroy(DeployDestroyArgs),
    /// Start a created deployment
    Start(DeployStartArgs),
    /// Stop a running deployment
    Stop(DeployStopArgs),
    /// Show deployment state
    Status(DeployStatusArgs),
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
    #[arg(long, help = "Remote project ID to link to (auto-detects from git remote if not provided)")]
    pub(crate) project_id: Option<String>,
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

#[derive(Debug, Parser)]
pub(crate) struct DeployStartArgs {
    #[arg(long, help = "Application name of the deployment to start")]
    pub(crate) app_name: String,
}

#[derive(Debug, Parser)]
pub(crate) struct DeployStopArgs {
    #[arg(long, help = "Application name of the deployment to stop")]
    pub(crate) app_name: String,
}

#[derive(Debug, Parser)]
pub(crate) struct DeployStatusArgs {
    #[arg(long, help = "Application name to check status for")]
    pub(crate) app_name: String,
}
