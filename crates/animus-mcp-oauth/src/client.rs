//! A small reusable MCP client session for deterministic, one-shot tool
//! invocation from the CLI (`animus mcp tools` / `animus mcp call`) and any
//! future in-kernel MCP caller.
//!
//! [`McpSession`] opens an rmcp MCP **client** over one of two transports —
//! chosen exactly the way the daemon builds `mcp_servers`:
//!
//! * **OAuth-protected server** (any flow: `authorization_code` /
//!   `manual_bearer` / `client_credentials` / `refresh_token`): spawn the
//!   local `animus-mcp-proxy` stdio child — the daemon's exact path — which
//!   reads + injects the cached bearer and forwards to the upstream. The
//!   resolved secret never appears on argv, in config, or on stdout.
//! * **Plain-HTTP server** (no `oauth:` block, e.g. a loopback github MCP):
//!   connect to its `url` directly over streamable-http.
//!
//! rmcp's `serve_client` performs the `initialize` -> `notifications/initialized`
//! handshake automatically; [`McpSession::list_tools`] issues `tools/list`
//! (paginated) and [`McpSession::call_tool`] issues `tools/call`. The whole
//! session is bounded by an overall timeout and the spawned proxy child is
//! always torn down on drop (rmcp's `TokioChildProcess` kills it) or via
//! [`McpSession::shutdown`].

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, Tool};
use rmcp::serve_client;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{IntoTransport, StreamableHttpClientTransport, TokioChildProcess};
use tokio::process::Command;

use crate::config::resolve_server_url;

/// Base name of the stdio OAuth bridge binary (no platform suffix).
const MCP_PROXY_BIN: &str = "animus-mcp-proxy";

/// An open MCP client session against one configured server.
///
/// The `()` client handler means server->client requests (sampling, roots,
/// elicitation) are answered by the unit handler rather than relayed — these
/// two verbs only issue client->server requests (`tools/list` / `tools/call`),
/// so that is sufficient and matches the proxy's own upstream client.
pub struct McpSession {
    service: RunningService<RoleClient, ()>,
}

impl McpSession {
    /// Resolve `server` from workflow/project config and open a session over
    /// the transport its config implies (proxy for OAuth, direct for
    /// plain-HTTP). `url_override` (the `--url` flag) wins over config.
    ///
    /// `timeout` bounds the connect handshake (and, for the OAuth path, the
    /// proxy spawn + upstream connect).
    pub async fn connect(
        project_root: &Path,
        server: &str,
        url_override: Option<&str>,
        timeout: Duration,
    ) -> Result<Self> {
        crate::ensure_crypto_provider();
        // The ONE `config_source` resolution per `mcp call`. The resolved URL +
        // flow are threaded to the spawned proxy (`--url` + `--auth-code`) so the
        // proxy TRUSTS them and does NOT re-resolve — collapsing the historical
        // 3× config_source spawn amplification (CLI + proxy-bin + proxy-connect)
        // down to this single touch.
        let resolution = resolve_server_url(project_root, server, url_override)?;
        // OAuth servers (every flow) are served through the local proxy, which
        // resolves + injects the live bearer itself; plain-HTTP servers are
        // connected directly.
        let uses_oauth = resolution.is_authorization_code || resolution.broker_oauth.is_some();
        if uses_oauth {
            Self::connect_via_proxy(project_root, server, &resolution, timeout).await
        } else {
            Self::connect_http(&resolution.url, timeout).await
        }
    }

    /// Spawn `animus-mcp-proxy --server <server> --url <resolved-url>
    /// [--auth-code] --project-root <root>` and connect an MCP client to its
    /// stdio. The proxy injects the cached bearer; no secret is passed on argv
    /// or read here. stderr is inherited so proxy diagnostics (e.g. "run `animus
    /// mcp auth`") reach the user; stdout carries only JSON-RPC frames.
    ///
    /// Passing the already-resolved `--url` (and, for keychain servers,
    /// `--auth-code`) lets the proxy skip its own `config_source` lookup — so a
    /// bulk `mcp call` burst does one source resolution per call, here, instead
    /// of one per proxy spawn.
    async fn connect_via_proxy(
        project_root: &Path,
        server: &str,
        resolution: &crate::config::ServerResolution,
        timeout: Duration,
    ) -> Result<Self> {
        let mut cmd = Command::new(mcp_proxy_command());
        cmd.arg("--server").arg(server).arg("--project-root").arg(project_root);
        // Pass the resolved `--url` for BOTH flows so the proxy binds the exact
        // upstream the parent selected (honoring a `--url` override) instead of
        // re-resolving the server name to a possibly-different same-named entry.
        if !resolution.url.trim().is_empty() {
            cmd.arg("--url").arg(&resolution.url);
        }
        // `--auth-code` is added ONLY for keychain (`authorization_code`) servers:
        // it tells the proxy to trust `--url` and skip its own config_source
        // lookup entirely (the amplification cut). Broker flows omit it because
        // they still need their full oauth block from config; the proxy re-resolves
        // those — and if the source is down it errors cleanly (ConfigSourceUnavailable)
        // rather than misrouting the broker server to the keychain path.
        if resolution.is_authorization_code {
            cmd.arg("--auth-code");
        }
        // `TokioChildProcess::new` pipes stdin/stdout and inherits stderr; the
        // child is killed on drop of the returned transport (held by the
        // service), so the proxy is always torn down.
        let transport = TokioChildProcess::new(cmd)
            .with_context(|| format!("failed to spawn `{MCP_PROXY_BIN}` for MCP server `{server}`"))?;
        Self::connect_transport(transport, timeout).await
    }

    /// Connect directly to a plain-HTTP (streamable-http) MCP `url` with no
    /// auth header — the transport for a server with no `oauth:` block.
    async fn connect_http(url: &str, timeout: Duration) -> Result<Self> {
        let transport = StreamableHttpClientTransport::from_uri(url.to_string());
        Self::connect_transport(transport, timeout).await
    }

    /// Drive the `initialize` -> `notifications/initialized` handshake over an
    /// arbitrary MCP client transport, bounded by `timeout`. Exposed so tests
    /// can connect against an in-process stub server over a duplex byte stream
    /// (the same stdio JSON-RPC framing the proxy uses).
    pub async fn connect_transport<T, E, A>(transport: T, timeout: Duration) -> Result<Self>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let service = tokio::time::timeout(timeout, serve_client((), transport))
            .await
            .map_err(|_| anyhow!("timed out during MCP initialize handshake after {}s", timeout.as_secs()))?
            .context("MCP initialize handshake failed")?;
        Ok(Self { service })
    }

    /// Enumerate the server's tools (`tools/list`, following pagination).
    pub async fn list_tools(&self) -> Result<Vec<Tool>> {
        self.service.list_all_tools().await.context("MCP `tools/list` request failed")
    }

    /// Call one tool (`tools/call`). `arguments` is the tool's JSON-object
    /// argument map, or `None` for a no-argument call.
    ///
    /// A tool that reports an in-band failure returns `Ok(result)` with
    /// `is_error == Some(true)` — that is a successful round-trip, not an
    /// `Err`; only transport/protocol failures are `Err`.
    pub async fn call_tool(&self, name: &str, arguments: Option<JsonObject>) -> Result<CallToolResult> {
        let mut params = CallToolRequestParams::new(name.to_string());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        self.service.call_tool(params).await.with_context(|| format!("MCP `tools/call` for `{name}` failed"))
    }

    /// Gracefully close the session (and tear down the proxy child, if any).
    pub async fn shutdown(self) {
        let _ = self.service.cancel().await;
    }
}

/// Resolve the `animus-mcp-proxy` binary path, mirroring the daemon-side
/// resolution in `animus-runtime-shared`: (1) sibling of the host CLI path
/// (`ANIMUS_HOST_CLI_PATH`), (2) sibling of the current executable, (3) bare
/// name (PATH lookup). The `.exe` suffix is appended on Windows.
fn mcp_proxy_command() -> String {
    let file_name = format!("{MCP_PROXY_BIN}{}", std::env::consts::EXE_SUFFIX);

    if let Some(host_cli) = std::env::var("ANIMUS_HOST_CLI_PATH").ok().filter(|value| !value.trim().is_empty()) {
        if let Some(dir) = std::path::Path::new(&host_cli).parent() {
            let candidate = dir.join(&file_name);
            if candidate.exists() {
                return candidate.display().to_string();
            }
        }
    }

    if let Some(candidate) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&file_name)))
        .filter(|candidate| candidate.exists())
    {
        return candidate.display().to_string();
    }

    MCP_PROXY_BIN.to_string()
}
