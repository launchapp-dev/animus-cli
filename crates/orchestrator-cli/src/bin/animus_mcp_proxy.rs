//! `animus-mcp-proxy` — stdio MCP bridge to an OAuth-protected upstream.
//!
//! Spawned per agent by the runtime-contract assembler for any MCP server
//! with an `oauth:` block. For the interactive `authorization_code` flow the
//! live bearer token comes from the OS keychain (written by
//! `animus mcp auth <server>`); for the machine-to-machine flows
//! (`manual_bearer` / `client_credentials` / `refresh_token`) it is resolved
//! through `animus_runtime_shared::oauth_broker` at connect time. Either
//! way the proxy serves the agent an auth-free stdio MCP endpoint and
//! forwards to the upstream with the bearer injected + refreshed on
//! expiry/401 — the resolved secret never appears on argv, in `.mcp.json`,
//! or on the `mcp_servers` wire channel.
//!
//! Ships in the `orchestrator-cli` package (alongside the `animus` binary) so
//! the standard `cargo build -p orchestrator-cli` build + release path emits
//! it next to `animus`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use orchestrator_config::workflow_config::OauthConfig;

#[derive(Debug, Parser)]
#[command(name = "animus-mcp-proxy", about = "Stdio MCP proxy to an OAuth-protected upstream MCP server", version)]
struct Args {
    /// Logical MCP server name (matches the workflow/project config key and
    /// the keychain token entry).
    #[arg(long)]
    server: String,

    /// Override the upstream MCP URL. When omitted, resolved from
    /// workflow/project config.
    #[arg(long)]
    url: Option<String>,

    /// Project root. Defaults to the current working directory.
    #[arg(long)]
    project_root: Option<PathBuf>,
}

/// [`animus_mcp_oauth::proxy::BearerTokenSource`] over the OAuth broker for
/// the machine-to-machine flows. `force_refresh` (set after the upstream
/// rejected the cached token) bypasses only the broker's fresh-access-token
/// cache hit — the cached rotated refresh-token chain is still used and the
/// re-minted token (with any newly rotated refresh token) is written back.
struct BrokerBearerSource {
    server: String,
    project_root: String,
    oauth: OauthConfig,
}

impl animus_mcp_oauth::proxy::BearerTokenSource for BrokerBearerSource {
    fn access_token(&self, force_refresh: bool) -> Result<String> {
        let token = animus_runtime_shared::oauth_broker::resolve_token_for_project_with_options(
            &self.server,
            &self.oauth,
            &self.project_root,
            force_refresh,
        )?;
        Ok(token.access_token)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let project_root = args
        .project_root
        .map(|p| p.display().to_string())
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_else(|| ".".to_string());
    let root = std::path::Path::new(&project_root);

    // stdout is the MCP stdio channel and must carry only JSON-RPC frames.
    // The proxy never logs tokens; diagnostics go to stderr.
    let resolution = animus_mcp_oauth::resolve_server_url(root, &args.server, args.url.as_deref())?;
    match resolution.broker_oauth {
        Some(oauth) => {
            let source =
                Arc::new(BrokerBearerSource { server: args.server.clone(), project_root: project_root.clone(), oauth });
            animus_mcp_oauth::proxy::run_with_bearer_source(&args.server, &resolution.url, source).await
        }
        None => animus_mcp_oauth::proxy::run(root, &args.server, args.url.as_deref()).await,
    }
}
