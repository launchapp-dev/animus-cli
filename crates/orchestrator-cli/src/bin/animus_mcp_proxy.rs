//! `animus-mcp-proxy` — stdio MCP bridge to an OAuth-protected upstream.
//!
//! Spawned per agent by the runtime-contract assembler for any MCP server
//! whose oauth flow is `authorization_code`. Reads the live bearer token
//! from the OS keychain (written by `animus mcp auth <server>`), serves the
//! agent an auth-free stdio MCP endpoint, and forwards to the upstream with
//! the bearer injected + refreshed on expiry/401.
//!
//! Ships in the `orchestrator-cli` package (alongside the `animus` binary) so
//! the standard `cargo build -p orchestrator-cli` build + release path emits
//! it next to `animus`.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let project_root = args
        .project_root
        .map(|p| p.display().to_string())
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_else(|| ".".to_string());

    // stdout is the MCP stdio channel and must carry only JSON-RPC frames.
    // The proxy never logs tokens; diagnostics go to stderr.
    animus_mcp_oauth::proxy::run(std::path::Path::new(&project_root), &args.server, args.url.as_deref()).await
}
