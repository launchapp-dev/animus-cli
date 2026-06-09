use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    /// Start the MCP server in the current process.
    Serve,
    /// Start the memory context MCP server for workflow phases.
    Memory,
    /// Authenticate an OAuth-protected MCP server interactively
    /// (discovery + DCR + auth_code/PKCE + browser login). Tokens are
    /// stored in the OS keychain.
    Auth(McpAuthArgs),
    /// Show which OAuth-protected MCP servers are authenticated, with
    /// token expiry per principal.
    AuthStatus(McpAuthStatusArgs),
    /// Delete stored OAuth tokens for an MCP server.
    AuthLogout(McpAuthLogoutArgs),
}

#[derive(Debug, clap::Args)]
pub(crate) struct McpAuthArgs {
    /// Logical MCP server name (matches the workflow/project config key).
    pub server: String,
    /// Override the upstream MCP URL (for servers not yet in config).
    #[arg(long)]
    pub url: Option<String>,
    /// Comma-separated OAuth scopes to request (overrides config scopes).
    #[arg(long, value_delimiter = ',')]
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct McpAuthStatusArgs {
    /// Limit the report to a single server.
    #[arg(long)]
    pub server: Option<String>,
    /// Upstream MCP URL for a `--server` not in config (tokens are bound to
    /// the URL). Required to read a token authenticated via `mcp auth --url`.
    #[arg(long, requires = "server")]
    pub url: Option<String>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct McpAuthLogoutArgs {
    /// Logical MCP server name to log out.
    pub server: String,
    /// Upstream MCP URL the token was authenticated against. Required to
    /// delete a token for a server not in config (tokens are bound to the
    /// URL the `mcp auth` ran against).
    #[arg(long)]
    pub url: Option<String>,
}
