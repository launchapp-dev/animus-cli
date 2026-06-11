//! `animus-mcp-proxy`: a stdio MCP server that transparently bridges an
//! agent to an OAuth-protected upstream MCP server.
//!
//! ```text
//!   agent ── stdio (no auth) ──▶ animus-mcp-proxy ── streamable-http + Bearer ──▶ upstream
//! ```
//!
//! The proxy:
//! - serves the agent as an rmcp MCP **server** over **stdio** (no auth);
//! - connects to the upstream as an rmcp MCP **client** over
//!   streamable-http, injecting the live bearer token — read from the OS
//!   keychain (`authorization_code` flow) or from a caller-supplied
//!   [`BearerTokenSource`] (the broker-backed `manual_bearer` /
//!   `client_credentials` / `refresh_token` flows);
//! - forwards `initialize` (returns the upstream's cached server info) and
//!   every request/notification transparently;
//! - on an upstream auth/transport failure, refreshes the token and
//!   reconnects **once** before surfacing the error;
//! - when there is no stored token (or refresh is rejected), returns a clear
//!   MCP error instructing the user to run `animus mcp auth <server>`.
//!
//! No OAuth/PKCE/token-exchange is implemented here: keychain token
//! lifecycle is entirely rmcp's `AuthorizationManager`, and broker token
//! lifecycle is whatever the injected [`BearerTokenSource`] implements.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use orchestrator_core::SecretStore;
use rmcp::model::{ClientNotification, ClientRequest, ErrorCode, ServerInfo, ServerResult};
use rmcp::service::{
    NotificationContext, RequestContext, RoleClient, RoleServer, RunningService, Service, ServiceError,
};
use rmcp::transport::auth::{AuthError, AuthorizationManager};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{stdio, StreamableHttpClientTransport};
use rmcp::{serve_client, ErrorData as McpError, ServiceExt};
use tokio::sync::Mutex;

use crate::config::{build_secret_store, resolve_principal_id, resolve_server_url};
use crate::keychain_store::KeychainCredentialStore;

/// Live upstream connection plus the metadata needed to rebuild it after a
/// token refresh.
struct Upstream {
    client: RunningService<RoleClient, ()>,
    server_info: ServerInfo,
}

/// Bearer-token resolver for the machine-to-machine OAuth flows
/// (`manual_bearer` / `client_credentials` / `refresh_token`). Implemented by
/// the `animus-mcp-proxy` binary over `animus_runtime_shared::oauth_broker`
/// so this crate stays free of the broker dependency. `force_refresh` is set
/// after an upstream auth failure; implementations should bypass any token
/// cache then. May block on network/disk I/O — the proxy always invokes it
/// via `spawn_blocking`.
pub trait BearerTokenSource: Send + Sync {
    fn access_token(&self, force_refresh: bool) -> Result<String>;
}

/// Where the upstream bearer token comes from: the keychain-backed rmcp
/// `AuthorizationManager` (`authorization_code`) or an injected
/// [`BearerTokenSource`] (broker flows).
enum TokenAuthority {
    Keychain(Arc<Mutex<AuthorizationManager>>),
    Bearer(Arc<dyn BearerTokenSource>),
}

impl TokenAuthority {
    /// Resolve the current access token. With `force_refresh`, the keychain
    /// manager forces a refresh-token exchange and a bearer source is asked
    /// to bypass its cache.
    async fn access_token(&self, server_name: &str, force_refresh: bool) -> Result<String> {
        match self {
            Self::Keychain(manager) => {
                let guard = manager.lock().await;
                if force_refresh {
                    // Ignore an "already fresh" refresh error path:
                    // get_access_token below still surfaces a real failure.
                    let _ = guard.refresh_token().await;
                }
                guard.get_access_token().await.map_err(|err| auth_error_to_anyhow(err, server_name))
            }
            Self::Bearer(source) => {
                let source = Arc::clone(source);
                let server = server_name.to_string();
                tokio::task::spawn_blocking(move || source.access_token(force_refresh))
                    .await
                    .map_err(|err| anyhow!("bearer token resolution for `{server}` panicked: {err}"))?
            }
        }
    }
}

/// The proxy service. Holds the upstream connection behind a mutex so a
/// refresh+reconnect can swap it out atomically while serializing requests.
pub struct McpProxy {
    server_name: String,
    upstream_url: String,
    auth: TokenAuthority,
    upstream: Mutex<Upstream>,
}

impl McpProxy {
    /// Build the proxy: resolve the server URL, load the keychain-backed
    /// auth manager, fetch the first access token, and open the upstream
    /// connection.
    pub async fn connect(project_root: &Path, server_name: &str, url_override: Option<&str>) -> Result<Self> {
        let resolution = resolve_server_url(project_root, server_name, url_override)?;
        let principal = resolve_principal_id(project_root);
        let secrets = build_secret_store(project_root)?;
        Self::connect_with_store(server_name, &resolution.url, secrets, &principal).await
    }

    /// Connect with an explicit keychain store, principal, and upstream URL.
    /// The production [`connect`](Self::connect) resolves these from config;
    /// tests inject a `MockSecretStore` + a local mock upstream.
    ///
    /// `upstream_url` is the `AuthorizationManager` base URL, which is BOTH
    /// the OAuth `resource` indicator (RFC 8707) and the discovery seed in
    /// rmcp 1.7 — it must match the URL the auth flow logged in against so
    /// refresh issues a token for the same audience.
    pub async fn connect_with_store(
        server_name: &str,
        upstream_url: &str,
        secrets: Arc<dyn SecretStore>,
        principal: &str,
    ) -> Result<Self> {
        crate::ensure_crypto_provider();
        let cred_store = KeychainCredentialStore::new(secrets, server_name, principal, upstream_url);

        let mut manager = AuthorizationManager::new(upstream_url)
            .await
            .map_err(|err| anyhow!("failed to init OAuth manager for `{server_name}`: {err}"))?;
        manager.set_credential_store(cred_store);

        // Hydrate the manager from the stored token (discovers metadata +
        // configures the client id). `false` means no usable stored token.
        let hydrated = manager
            .initialize_from_store()
            .await
            .map_err(|err| anyhow!("failed to load stored token for `{server_name}`: {err}"))?;
        if !hydrated {
            return Err(anyhow!(
                "no stored OAuth token for `{server_name}`; run `animus mcp auth {server_name}` first"
            ));
        }

        let auth = TokenAuthority::Keychain(Arc::new(Mutex::new(manager)));
        let upstream = Self::open_upstream(upstream_url, &auth, server_name, false).await?;

        Ok(Self {
            server_name: server_name.to_string(),
            upstream_url: upstream_url.to_string(),
            auth,
            upstream: Mutex::new(upstream),
        })
    }

    /// Connect with an injected [`BearerTokenSource`] instead of the keychain
    /// `AuthorizationManager`. Used for the machine-to-machine flows
    /// (`manual_bearer` / `client_credentials` / `refresh_token`), whose
    /// tokens the `animus-mcp-proxy` binary resolves through the OAuth
    /// broker at connect time.
    pub async fn connect_with_bearer_source(
        server_name: &str,
        upstream_url: &str,
        source: Arc<dyn BearerTokenSource>,
    ) -> Result<Self> {
        crate::ensure_crypto_provider();
        let auth = TokenAuthority::Bearer(source);
        let upstream = Self::open_upstream(upstream_url, &auth, server_name, false).await?;
        Ok(Self {
            server_name: server_name.to_string(),
            upstream_url: upstream_url.to_string(),
            auth,
            upstream: Mutex::new(upstream),
        })
    }

    /// Forward a request to the upstream from outside the stdio loop. Exposed
    /// for integration tests that drive the proxy's client side directly.
    pub async fn forward_request_for_test(&self, request: ClientRequest) -> Result<ServerResult, McpError> {
        self.forward_request(request).await
    }

    /// Open an upstream MCP client connection with the current access token
    /// (refreshing it if near expiry / on `force_refresh`).
    ///
    /// The initial `serve_client`/`initialize` handshake can itself be
    /// rejected with a 401 when the stored access token is still within
    /// `expires_in` but has been revoked server-side. In that case we force a
    /// token refresh once and retry the open, so proxy startup can recover
    /// using an otherwise-valid refresh token instead of failing immediately.
    async fn open_upstream(
        url: &str,
        auth: &TokenAuthority,
        server_name: &str,
        force_refresh: bool,
    ) -> Result<Upstream> {
        match Self::try_open_upstream(url, auth, server_name, force_refresh).await {
            Ok(upstream) => Ok(upstream),
            Err(first_err) if !force_refresh => {
                tracing::warn!(server = server_name, error = %first_err, "initial upstream connect failed; forcing token refresh and retrying once");
                Self::try_open_upstream(url, auth, server_name, true).await.map_err(|retry_err| {
                    anyhow!(
                        "failed to connect to upstream MCP server for `{server_name}` (after token refresh): {retry_err}"
                    )
                })
            }
            Err(err) => Err(err),
        }
    }

    /// Single attempt to open the upstream with the current access token.
    async fn try_open_upstream(
        url: &str,
        auth: &TokenAuthority,
        server_name: &str,
        force_refresh: bool,
    ) -> Result<Upstream> {
        let access_token = auth.access_token(server_name, force_refresh).await?;

        // `auth_header` takes the bearer token WITHOUT the `Bearer ` prefix.
        let config = StreamableHttpClientTransportConfig::with_uri(url.to_string()).auth_header(access_token);
        let transport = StreamableHttpClientTransport::from_config(config);

        // TODO(codex-p2): the upstream client uses the empty `()` handler, so
        // server→client requests (sampling, `roots/list`, elicitation) and
        // server→client notifications (list-changed, etc.) are answered/no-op'd
        // by the unit client instead of being relayed back to the agent. Full
        // bidirectional transparency would require deferring this connect until
        // after the agent's `initialize` (to forward its protocol
        // version/capabilities) and a `ClientHandler` that forwards
        // server-initiated traffic to the agent's peer. Tools-only OAuth
        // servers (GitHub/Linear/Notion) work today; servers that drive
        // sampling/roots/elicitation through the client are not yet
        // transparent. Documented in docs/reference/mcp-oauth.md#limitations.
        let client = serve_client((), transport)
            .await
            .with_context(|| format!("failed to connect to upstream MCP server for `{server_name}`"))?;
        let server_info = client.peer_info().cloned().unwrap_or_default();
        Ok(Upstream { client, server_info })
    }

    /// Forward a request to the upstream, retrying once after a token
    /// refresh + reconnect when (and only when) the first attempt fails with
    /// an auth/transport-shaped error.
    ///
    /// Normal upstream tool failures come back as `Ok(ServerResult)` with an
    /// `is_error` payload, not `Err`, so they are never retried. Among the
    /// `Err` cases we only refresh+retry on transport/protocol failures
    /// (which is how a 401 / dropped session surfaces); `Cancelled` and
    /// `Timeout` are propagated unchanged because re-auth won't fix them and
    /// re-sending a side-effecting `tools/call` could double-execute it.
    async fn forward_request(&self, request: ClientRequest) -> Result<ServerResult, McpError> {
        // First attempt against the current connection.
        let first = {
            let guard = self.upstream.lock().await;
            guard.client.peer().send_request(request.clone()).await
        };
        match first {
            Ok(result) => Ok(result),
            Err(err) if is_retryable_auth_error(&err) => {
                tracing::warn!(server = %self.server_name, error = %err, "upstream request failed (auth/transport); refreshing token and reconnecting once");
                self.refresh_and_reconnect().await.map_err(|reconnect_err| {
                    McpError::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!(
                            "upstream MCP `{}` request failed and re-auth failed: {}. Run `animus mcp auth {}`.",
                            self.server_name, reconnect_err, self.server_name
                        ),
                        None,
                    )
                })?;
                let guard = self.upstream.lock().await;
                guard
                    .client
                    .peer()
                    .send_request(request)
                    .await
                    .map_err(|retry_err| self.service_error_to_mcp_error(retry_err, "after refresh+retry"))
            }
            Err(err) => Err(self.service_error_to_mcp_error(err, "")),
        }
    }

    /// Map a `ServiceError` into an MCP error for the agent. An upstream
    /// JSON-RPC error object (`ServiceError::McpError`) is passed through
    /// verbatim so the proxy stays transparent for protocol errors like
    /// `method not found` / `invalid params`; transport/other failures are
    /// wrapped as `INTERNAL_ERROR` with a contextual message.
    fn service_error_to_mcp_error(&self, err: ServiceError, context: &str) -> McpError {
        match err {
            ServiceError::McpError(e) => e,
            other => {
                let suffix = if context.is_empty() { String::new() } else { format!(" {context}") };
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("upstream MCP `{}` request failed{suffix}: {other}", self.server_name),
                    None,
                )
            }
        }
    }

    /// Refresh the token and replace the upstream connection. The forced
    /// open makes the token authority bypass freshness checks/caches, since
    /// the current token was just rejected upstream.
    async fn refresh_and_reconnect(&self) -> Result<()> {
        let new_upstream = Self::open_upstream(&self.upstream_url, &self.auth, &self.server_name, true).await?;
        let mut guard = self.upstream.lock().await;
        *guard = new_upstream;
        Ok(())
    }

    /// Cached upstream server info for the proxy's `initialize` response.
    async fn upstream_server_info(&self) -> ServerInfo {
        self.upstream.lock().await.server_info.clone()
    }
}

/// Drive the proxy: serve the agent over stdio until the connection closes.
pub async fn run(project_root: &Path, server_name: &str, url_override: Option<&str>) -> Result<()> {
    let proxy = McpProxy::connect(project_root, server_name, url_override).await?;
    serve_until_closed(proxy, server_name).await
}

/// Drive the proxy for a broker-flow server: tokens come from `source`
/// (resolved by the caller, typically the `animus-mcp-proxy` binary over the
/// OAuth broker) instead of the keychain.
pub async fn run_with_bearer_source(
    server_name: &str,
    upstream_url: &str,
    source: Arc<dyn BearerTokenSource>,
) -> Result<()> {
    let proxy = McpProxy::connect_with_bearer_source(server_name, upstream_url, source).await?;
    serve_until_closed(proxy, server_name).await
}

async fn serve_until_closed(proxy: McpProxy, server_name: &str) -> Result<()> {
    let running =
        proxy.serve(stdio()).await.with_context(|| format!("failed to serve stdio MCP proxy for `{server_name}`"))?;
    running.waiting().await.context("mcp proxy stdio service ended unexpectedly")?;
    Ok(())
}

impl Service<RoleServer> for McpProxy {
    async fn handle_request(
        &self,
        request: ClientRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<ServerResult, McpError> {
        match request {
            // The upstream client already completed its own initialize
            // handshake when the proxy connected. Return the upstream's
            // cached server info rather than re-initializing it.
            ClientRequest::InitializeRequest(_) => {
                // `ServerInfo` is a type alias for `InitializeResult`, so the
                // cached upstream info is returned verbatim.
                let info = self.upstream_server_info().await;
                Ok(ServerResult::InitializeResult(info))
            }
            // Everything else is forwarded transparently.
            other => self.forward_request(other).await,
        }
    }

    async fn handle_notification(
        &self,
        notification: ClientNotification,
        _context: NotificationContext<RoleServer>,
    ) -> Result<(), McpError> {
        // The agent's `initialized` notification is local to the
        // proxy<->agent handshake; the upstream was already initialized.
        if matches!(notification, ClientNotification::InitializedNotification(_)) {
            return Ok(());
        }
        let guard = self.upstream.lock().await;
        if let Err(err) = guard.client.peer().send_notification(notification).await {
            tracing::warn!(server = %self.server_name, error = %err, "failed to forward notification upstream");
        }
        Ok(())
    }

    fn get_info(&self) -> ServerInfo {
        // Synchronous accessor — return a minimal default; the real upstream
        // capabilities are surfaced through the `initialize` response above.
        ServerInfo::default()
    }
}

/// True when an upstream request error is a transport-layer failure that a
/// token refresh + reconnect could plausibly recover.
///
/// An upstream HTTP `401` (the auth case we care about) surfaces through the
/// streamable-http transport as a `TransportSend` / `TransportClosed` failure
/// — the POST itself fails before a JSON-RPC response is produced. A
/// `ServiceError::McpError` is the opposite: a well-formed JSON-RPC *error
/// object* returned by the server (invalid params, a tool error, etc.). We
/// deliberately do NOT retry `McpError` (or `Cancelled`/`Timeout`/
/// `UnexpectedResponse`): re-auth won't fix an application error, and
/// re-sending a side-effecting `tools/call` could double-execute it.
fn is_retryable_auth_error(err: &ServiceError) -> bool {
    matches!(err, ServiceError::TransportSend(_) | ServiceError::TransportClosed)
}

fn auth_error_to_anyhow(err: AuthError, server_name: &str) -> anyhow::Error {
    match err {
        AuthError::AuthorizationRequired | AuthError::TokenExpired => {
            anyhow!("stored OAuth token for `{server_name}` is missing or expired; run `animus mcp auth {server_name}`")
        }
        other => anyhow!("OAuth token resolution failed for `{server_name}`: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_error_messages_point_to_reauth() {
        let msg = auth_error_to_anyhow(AuthError::AuthorizationRequired, "github").to_string();
        assert!(msg.contains("animus mcp auth github"), "msg: {msg}");
        let msg2 = auth_error_to_anyhow(AuthError::TokenExpired, "linear").to_string();
        assert!(msg2.contains("animus mcp auth linear"), "msg2: {msg2}");
    }

    #[test]
    fn only_transport_errors_are_retried() {
        assert!(is_retryable_auth_error(&ServiceError::TransportClosed));
        // A JSON-RPC error object from the server is an application error, not
        // an auth/transport failure — re-auth won't fix it and a retry could
        // double-execute a side-effecting call.
        assert!(!is_retryable_auth_error(&ServiceError::McpError(McpError::new(
            ErrorCode::INVALID_PARAMS,
            "invalid params",
            None
        ))));
        // Cancellation / timeout / unexpected-response must NOT trigger retry.
        assert!(!is_retryable_auth_error(&ServiceError::Cancelled { reason: None }));
        assert!(!is_retryable_auth_error(&ServiceError::Timeout { timeout: std::time::Duration::from_secs(1) }));
        assert!(!is_retryable_auth_error(&ServiceError::UnexpectedResponse));
    }
}
