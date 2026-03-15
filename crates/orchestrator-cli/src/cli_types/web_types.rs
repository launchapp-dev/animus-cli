use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum WebCommand {
    /// Start the AO web server.
    Serve(WebServeArgs),
    /// Open the AO web UI URL in a browser.
    Open(WebOpenArgs),
}

#[derive(Debug, Args)]
pub(crate) struct WebServeArgs {
    #[arg(long, value_name = "HOST", default_value = "127.0.0.1", help = "Host interface to bind the web server.")]
    pub(crate) host: String,
    #[arg(long, value_name = "PORT", default_value_t = 4173, help = "Port to bind the web server.")]
    pub(crate) port: u16,
    #[arg(long, default_value_t = false, help = "Open the web UI in a browser after startup.")]
    pub(crate) open: bool,
    #[arg(long, value_name = "PATH", help = "Override static assets directory.")]
    pub(crate) assets_dir: Option<String>,
    #[arg(long, default_value_t = false, help = "Serve API endpoints only without static assets.")]
    pub(crate) api_only: bool,
    #[arg(
        long,
        value_name = "COUNT",
        default_value_t = 50,
        help = "Default page size for paginated list API endpoints."
    )]
    pub(crate) page_size_default: usize,
    #[arg(
        long,
        value_name = "COUNT",
        default_value_t = 200,
        help = "Maximum allowed page size for paginated list API endpoints."
    )]
    pub(crate) page_size_max: usize,
}

#[derive(Debug, Args)]
pub(crate) struct WebOpenArgs {
    #[arg(long, value_name = "HOST", default_value = "127.0.0.1", help = "Host name for the web URL.")]
    pub(crate) host: String,
    #[arg(long, value_name = "PORT", default_value_t = 4173, help = "Port for the web URL.")]
    pub(crate) port: u16,
    #[arg(long, value_name = "PATH", default_value = "/", help = "Path to open, such as / or /runs.")]
    pub(crate) path: String,
}
