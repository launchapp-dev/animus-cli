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

use anyhow::{anyhow, Context, Result};
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

    /// The caller (CLI `mcp call`) already resolved this as an
    /// `authorization_code` (keychain) server and passed the upstream `--url`.
    /// With both set, the proxy TRUSTS the URL and skips the `config_source`
    /// round-trip entirely — the reduction that keeps bulk `mcp call` from
    /// re-saturating the source on every spawn. Ignored without `--url`.
    #[arg(long = "auth-code")]
    auth_code: bool,

    #[arg(long)]
    oauth_config_json: Option<String>,

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

fn explicit_oauth_config(args: &Args) -> Result<Option<OauthConfig>> {
    args.oauth_config_json
        .as_deref()
        .map(|raw| serde_json::from_str(raw).context("invalid --oauth-config-json value"))
        .transpose()
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

fn main() -> Result<()> {
    let worker_threads = animus_runtime_shared::cgroup_threads::tokio_worker_threads();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(async_main())
}

async fn async_main() -> Result<()> {
    let args = Args::parse();
    let project_root = args
        .project_root
        .as_ref()
        .map(|p| p.display().to_string())
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_else(|| ".".to_string());
    let root = std::path::Path::new(&project_root);

    if let Some(oauth) = explicit_oauth_config(&args)? {
        let url = args
            .url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("--url is required when --oauth-config-json is provided"))?;
        if oauth.flow == orchestrator_config::workflow_config::OauthFlow::AuthorizationCode {
            return animus_mcp_oauth::proxy::run_authorization_code(&args.server, url, root).await;
        }
        let source =
            Arc::new(BrokerBearerSource { server: args.server.clone(), project_root: project_root.clone(), oauth });
        return animus_mcp_oauth::proxy::run_with_bearer_source(&args.server, url, source).await;
    }

    // Fast path: the caller already resolved an `authorization_code` (keychain)
    // server and handed us the upstream `--url`. Trust it and skip the
    // `config_source` lookup entirely — this is the amplification cut that keeps
    // bulk `mcp call` from re-loading the source on every proxy spawn. Keychain
    // principal + secret store come from the OS keychain / on-disk scoped state,
    // never the config source.
    if args.auth_code {
        if let Some(url) = args.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            return animus_mcp_oauth::proxy::run_authorization_code(&args.server, url, root).await;
        }
    }

    // stdout is the MCP stdio channel and must carry only JSON-RPC frames.
    // The proxy never logs tokens; diagnostics go to stderr.
    //
    // The single `config_source` resolution needed to split broker vs keychain
    // flows. The resolved URL is threaded onward (broker path passes it to
    // `run_with_bearer_source`; keychain path to `run_authorization_code`) so
    // NEITHER downstream re-resolves — one source touch per spawn, not two.
    let resolution = animus_mcp_oauth::resolve_server_url(root, &args.server, args.url.as_deref())?;
    match resolution.broker_oauth {
        Some(oauth) => {
            let source =
                Arc::new(BrokerBearerSource { server: args.server.clone(), project_root: project_root.clone(), oauth });
            animus_mcp_oauth::proxy::run_with_bearer_source(&args.server, &resolution.url, source).await
        }
        None => animus_mcp_oauth::proxy::run_authorization_code(&args.server, &resolution.url, root).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use orchestrator_config::workflow_config::OauthFlow;

    #[test]
    fn explicit_oauth_config_parses_without_loading_project_config() {
        let args = Args::try_parse_from([
            "animus-mcp-proxy",
            "--server",
            "rental-v1",
            "--url",
            "https://example.test/mcp",
            "--oauth-config-json",
            r#"{"flow":"manual_bearer","bearer_env":"RENTAL_MCP_BEARER"}"#,
        ])
        .expect("args");

        let config = explicit_oauth_config(&args).expect("config").expect("explicit config");
        assert_eq!(config.flow, OauthFlow::ManualBearer);
        assert_eq!(config.bearer_env.as_deref(), Some("RENTAL_MCP_BEARER"));
    }

    #[test]
    fn explicit_oauth_config_rejects_invalid_json() {
        let args =
            Args::try_parse_from(["animus-mcp-proxy", "--server", "rental-v1", "--oauth-config-json", "not-json"])
                .expect("args");

        assert!(explicit_oauth_config(&args).is_err());
    }
}
