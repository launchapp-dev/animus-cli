use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    /// Start the MCP server in the current process.
    Serve(McpServeArgs),
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
pub(crate) struct McpServeArgs {
    /// Also expose the human-side interaction management tools
    /// (animus.interactions.list / animus.interactions.answer). Off by
    /// default so agent-injected servers cannot answer their own
    /// questions or approve their own approval requests.
    #[arg(long, default_value_t = false)]
    pub management: bool,
    /// Pin the agent identity used by the blocking animus.agent.ask /
    /// animus.agent.request_approval tools. When set, the payload
    /// `agent_id` is ignored so a spawned agent cannot claim a sibling
    /// profile whose approval_policy is more permissive. The host that
    /// injects this server should pass the agent profile id here.
    #[arg(long, value_name = "AGENT_ID")]
    pub agent_id: Option<String>,
    /// Pin the workflow context for the blocking animus.agent.ask /
    /// animus.agent.request_approval tools. When set, escalations default to
    /// wait="suspend" (return immediately, pause the workflow, resume on
    /// answer) and pending records carry this workflow id; the payload
    /// `workflow_id` is ignored. The workflow runner that injects this
    /// server should pass the running workflow id here.
    #[arg(long, value_name = "WORKFLOW_ID")]
    pub workflow_id: Option<String>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct McpAuthArgs {
    /// Logical MCP server name (matches the workflow/project config key).
    pub server: String,
    /// Override the upstream MCP URL (for servers not yet in config).
    #[arg(long)]
    pub url: Option<String>,
    /// Comma-separated OAuth scopes to request (overrides config scopes and
    /// the server's advertised scopes). When omitted, scopes resolve in order:
    /// config `scopes:`, else the server's advertised `scopes_supported`
    /// (auto-detected from discovery metadata), else none (server default).
    /// Pass this to narrow an over-broad auto-detected set, or `--scopes none`
    /// to force NO scopes (opt out of auto-detection; server default applies).
    #[arg(long, value_delimiter = ',')]
    pub scopes: Option<Vec<String>>,
    /// Skip the consent prompt and open the browser immediately.
    #[arg(long)]
    pub yes: bool,
    /// Resolve discovery + scopes and report them without opening a browser
    /// or obtaining any token.
    #[arg(long)]
    pub dry_run: bool,

    /// Delegated (headless/web) BEGIN: resolve + register/configure the client
    /// and PRINT the authorization URL + `state` instead of opening a browser
    /// or binding a localhost listener. Requires `--redirect-uri`. The PKCE
    /// state is persisted so a later `--complete` finishes the exchange. Used by
    /// a remote host (e.g. the portal) that drives the redirect itself.
    #[arg(long, conflicts_with = "dry_run")]
    pub print_url: bool,

    /// The caller's public OAuth callback URL (e.g.
    /// `https://portal/api/mcp-oauth/callback`). Required with `--print-url`.
    #[arg(long, requires = "print_url")]
    pub redirect_uri: Option<String>,

    /// Delegated (headless/web) COMPLETE: exchange the `--code`/`--state`
    /// returned to the caller's callback for a token, finishing a flow started
    /// with `--print-url`. Mutually exclusive with `--print-url`/`--dry-run`.
    #[arg(long, conflicts_with_all = ["print_url", "dry_run"])]
    pub complete: bool,

    /// Authorization code from the callback. Required with `--complete`.
    #[arg(long, requires = "complete")]
    pub code: Option<String>,

    /// CSRF `state` from the callback (and from the `--print-url` output).
    /// Required with `--complete`.
    #[arg(long, requires = "complete")]
    pub state: Option<String>,
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
