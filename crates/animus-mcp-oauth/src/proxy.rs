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
use rmcp::transport::auth::{AuthError, AuthorizationManager, CredentialStore};
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
    /// Build the proxy: resolve the server URL from config, load the
    /// keychain-backed auth manager, fetch the first access token, and open the
    /// upstream connection.
    ///
    /// Prefer [`connect_authorization_code`](Self::connect_authorization_code)
    /// when the upstream URL is ALREADY resolved (the daemon/CLI contract passes
    /// it via `--url`): that path skips the `config_source` round-trip this one
    /// performs, which is what saturates the source under bulk `mcp call`.
    pub async fn connect(project_root: &Path, server_name: &str, url_override: Option<&str>) -> Result<Self> {
        let resolution = resolve_server_url(project_root, server_name, url_override)?;
        Self::connect_authorization_code(project_root, server_name, &resolution.url).await
    }

    /// Build the proxy for an `authorization_code` (keychain) server whose
    /// upstream URL is ALREADY resolved — skipping the `config_source` lookup
    /// [`connect`](Self::connect) does. The keychain principal + secret store
    /// are read from the OS keychain / on-disk scoped state (NOT the config
    /// source), so this performs ZERO `config_source` spawns.
    pub async fn connect_authorization_code(
        project_root: &Path,
        server_name: &str,
        upstream_url: &str,
    ) -> Result<Self> {
        let principal = resolve_principal_id(project_root);
        let secrets = build_secret_store(project_root)?;
        let cache_dir = discovery_cache::cache_dir_for(project_root);
        Self::connect_with_store(server_name, upstream_url, secrets, &principal, cache_dir.as_deref()).await
    }

    /// Connect with an explicit keychain store, principal, and upstream URL.
    /// The production [`connect`](Self::connect) resolves these from config;
    /// tests inject a `MockSecretStore` + a local mock upstream.
    ///
    /// `upstream_url` is the `AuthorizationManager` base URL, which is BOTH
    /// the OAuth `resource` indicator (RFC 8707) and the discovery seed in
    /// rmcp 1.7 — it must match the URL the auth flow logged in against so
    /// refresh issues a token for the same audience.
    ///
    /// `discovery_cache_dir` (when `Some`) is a directory used to cache the
    /// discovered RFC 8414/9728 OAuth metadata per `(server, url)` so repeated
    /// proxy spawns don't re-hit the (throttled) upstream discovery endpoint on
    /// every connect. `None` disables the cache (used by tests that want a live
    /// discovery each connect).
    pub async fn connect_with_store(
        server_name: &str,
        upstream_url: &str,
        secrets: Arc<dyn SecretStore>,
        principal: &str,
        discovery_cache_dir: Option<&Path>,
    ) -> Result<Self> {
        crate::ensure_crypto_provider();
        let cred_store = KeychainCredentialStore::new(secrets, server_name, principal, upstream_url);

        // Gate discovery priming on a stored token existing. Priming (below)
        // runs a live `.well-known` discovery on a cache miss; doing that for an
        // unauthenticated server (no stored token) would turn the fast-fail
        // startup path into a needless network round-trip against the (throttled)
        // upstream. When the credential store has no token we skip priming and
        // let `initialize_from_store` take the unauthenticated fast-fail path.
        let has_stored_token = matches!(cred_store.load().await, Ok(Some(_)));

        let mut manager = AuthorizationManager::new(upstream_url)
            .await
            .map_err(|err| anyhow!("failed to init OAuth manager for `{server_name}`: {err}"))?;
        manager.set_credential_store(cred_store);

        // (a) Prime the manager with cached OAuth discovery metadata BEFORE
        // `initialize_from_store` (which only discovers when metadata is unset —
        // rmcp 1.7 `transport/auth.rs`). Under bulk `mcp call` the upstream
        // throttles the per-spawn `.well-known` discovery, surfacing as
        // `NoAuthorizationSupport` ("No authorization support detected"); a
        // cache hit avoids the network call entirely. On a miss we discover once
        // and persist for subsequent spawns. Best-effort: a cache/discovery
        // failure here is non-fatal — `initialize_from_store` still runs. Only
        // primed when a token is stored (see `has_stored_token` above).
        if has_stored_token {
            prime_discovery_metadata(&mut manager, server_name, upstream_url, discovery_cache_dir).await;
        }

        // Hydrate the manager from the stored token (discovers metadata when not
        // already primed above + configures the client id). `false` means no
        // usable stored token.
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

/// Drive the proxy: resolve the server URL from config, then serve the agent
/// over stdio until the connection closes.
///
/// Prefer [`run_authorization_code`] when the URL is already resolved — it
/// avoids the `config_source` round-trip this path performs.
pub async fn run(project_root: &Path, server_name: &str, url_override: Option<&str>) -> Result<()> {
    let proxy = McpProxy::connect(project_root, server_name, url_override).await?;
    serve_until_closed(proxy, server_name).await
}

/// Drive the proxy for an `authorization_code` (keychain) server whose upstream
/// URL is ALREADY resolved (passed by the daemon/CLI contract via `--url`).
/// Skips the `config_source` lookup [`run`] performs, so a bulk `mcp call`
/// burst doesn't re-saturate the source on every proxy spawn.
pub async fn run_authorization_code(server_name: &str, upstream_url: &str, project_root: &Path) -> Result<()> {
    let proxy = McpProxy::connect_authorization_code(project_root, server_name, upstream_url).await?;
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

/// Prime `manager` with OAuth discovery metadata for `(server_name,
/// upstream_url)` so `initialize_from_store` skips its per-spawn `.well-known`
/// discovery. On a cache hit, load + `set_metadata`. On a miss, discover live,
/// `set_metadata`, and persist for next time. All failures are non-fatal (we
/// simply leave the manager to discover itself).
async fn prime_discovery_metadata(
    manager: &mut AuthorizationManager,
    server_name: &str,
    upstream_url: &str,
    cache_dir: Option<&Path>,
) {
    let Some(cache_dir) = cache_dir else {
        return;
    };
    if let Some(cached) = discovery_cache::load(cache_dir, server_name, upstream_url) {
        manager.set_metadata(cached);
        return;
    }
    match manager.discover_metadata().await {
        Ok(metadata) => {
            discovery_cache::store(cache_dir, server_name, upstream_url, &metadata);
            manager.set_metadata(metadata);
        }
        Err(err) => {
            // Non-fatal: leave `metadata` unset so `initialize_from_store`
            // attempts its own discovery (which will surface the real error if
            // the upstream is genuinely unreachable).
            tracing::warn!(server = server_name, error = %err, "oauth discovery failed while priming cache; will retry via initialize_from_store");
        }
    }
}

/// Best-effort on-disk cache of discovered OAuth [`AuthorizationMetadata`],
/// keyed by `(server, upstream_url)`. The metadata is the set of public
/// `.well-known` OAuth endpoints (RFC 8414/9728) — NOT a secret — so it lives
/// next to (not inside) the encrypted token store. A stale entry self-heals via
/// the TTL below; any read/parse error is treated as a miss.
mod discovery_cache {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use rmcp::transport::auth::AuthorizationMetadata;
    use sha2::{Digest, Sha256};

    /// Cache subdirectory under the scoped state root. Versioned so a metadata
    /// schema change can bump the directory without colliding with old entries.
    const CACHE_SUBDIR: &str = "mcp-oauth-discovery.v1";
    /// Entries older than this (24h) are ignored (and overwritten on the next
    /// miss), so a genuinely rotated upstream discovery document eventually
    /// takes hold.
    const TTL: Duration = Duration::from_hours(24);

    /// The discovery-cache directory for `project_root` (its scoped state root),
    /// or `None` when the scope can't be resolved.
    pub(super) fn cache_dir_for(project_root: &Path) -> Option<PathBuf> {
        protocol::repository_scope::scoped_state_root(project_root).map(|root| root.join(CACHE_SUBDIR))
    }

    /// Stable filename for a `(server, url)` pair.
    fn entry_path(cache_dir: &Path, server: &str, url: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(server.as_bytes());
        hasher.update([0x1f]);
        hasher.update(url.as_bytes());
        cache_dir.join(format!("{:x}.json", hasher.finalize()))
    }

    /// Load cached metadata for `(server, url)`, or `None` on miss / expiry /
    /// any read or parse error.
    pub(super) fn load(cache_dir: &Path, server: &str, url: &str) -> Option<AuthorizationMetadata> {
        let path = entry_path(cache_dir, server, url);
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        if SystemTime::now().duration_since(modified).map(|age| age > TTL).unwrap_or(true) {
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Persist `metadata` for `(server, url)`. Best-effort — any error is
    /// ignored (the next connect simply re-discovers).
    pub(super) fn store(cache_dir: &Path, server: &str, url: &str, metadata: &AuthorizationMetadata) {
        if std::fs::create_dir_all(cache_dir).is_err() {
            return;
        }
        if let Ok(bytes) = serde_json::to_vec(metadata) {
            let _ = std::fs::write(entry_path(cache_dir, server, url), bytes);
        }
    }
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
