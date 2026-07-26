//! Integration test for `animus-mcp-proxy`'s upstream client side.
//!
//! Stands up a single axum server that plays three roles:
//! 1. OAuth Authorization Server metadata (`.well-known/oauth-authorization-server`)
//!    so the proxy's `AuthorizationManager::initialize_from_store` discovery
//!    succeeds.
//! 2. OAuth token endpoint (`/token`) for the refresh-token grant.
//! 3. Streamable-HTTP MCP upstream (`/mcp`) that captures the `Authorization`
//!    bearer header and serves `initialize` + `tools/list`.
//!
//! With a token pre-stored in a `MockSecretStore`, the proxy connects, and a
//! `tools/list` forwarded through `forward_request_for_test` is observed to
//! carry the live bearer. A second scenario seeds an expired access token +
//! refresh token; the upstream returns 401 once, and the proxy is expected to
//! refresh (hitting `/token`) and retry successfully.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use orchestrator_core::secret_store::MockSecretStore;
use orchestrator_core::SecretStore;
use rmcp::model::{ClientRequest, ListToolsRequest, ServerResult};
use serde_json::json;
use tokio::sync::Mutex;

const ACCESS_TOKEN_FRESH: &str = "fresh-access-token";
const ACCESS_TOKEN_REFRESHED: &str = "refreshed-access-token";
const REFRESH_TOKEN: &str = "the-refresh-token";

#[derive(Clone)]
struct MockState {
    base_url: Arc<Mutex<String>>,
    /// Authorization headers observed on `/mcp` requests, in order.
    seen_auth_headers: Arc<Mutex<Vec<String>>>,
    /// When true, the first non-initialize `/mcp` request returns 401 and the
    /// token endpoint must be hit to recover.
    require_refresh: bool,
    /// Set once the `/token` refresh endpoint has been called.
    refreshed: Arc<Mutex<bool>>,
    token_calls: Arc<Mutex<usize>>,
    /// Count of OAuth Authorization-Server discovery (`.well-known`) hits, used
    /// to prove the discovery-metadata cache avoids re-discovery per spawn.
    well_known_calls: Arc<Mutex<usize>>,
}

async fn well_known(State(state): State<MockState>) -> impl IntoResponse {
    *state.well_known_calls.lock().await += 1;
    let base = state.base_url.lock().await.clone();
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": ["S256"]
        })
        .to_string(),
    )
}

async fn token_endpoint(State(state): State<MockState>) -> impl IntoResponse {
    *state.refreshed.lock().await = true;
    *state.token_calls.lock().await += 1;
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        json!({
            "access_token": ACCESS_TOKEN_REFRESHED,
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": REFRESH_TOKEN
        })
        .to_string(),
    )
}

async fn mcp_handler(State(state): State<MockState>, headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();

    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let method = parsed.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = parsed.get("id").cloned().unwrap_or(json!(1));

    // Record auth header on substantive requests (skip pure notifications).
    if !method.is_empty() {
        state.seen_auth_headers.lock().await.push(auth.clone());
    }

    let session_header =
        (axum::http::HeaderName::from_static("mcp-session-id"), axum::http::HeaderValue::from_static("test-session"));

    match method {
        "initialize" => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json")),
                session_header,
            ],
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "mock-upstream", "version": "1.0.0" }
                }
            })
            .to_string(),
        )
            .into_response(),
        "notifications/initialized" => (
            StatusCode::ACCEPTED,
            [
                (axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json")),
                session_header,
            ],
            String::new(),
        )
            .into_response(),
        "tools/list" => {
            let refreshed = *state.refreshed.lock().await;
            // Stale-token scenario: reject until the token has been refreshed.
            if state.require_refresh && !refreshed {
                return (
                    StatusCode::UNAUTHORIZED,
                    [(
                        axum::http::header::WWW_AUTHENTICATE,
                        axum::http::HeaderValue::from_static("Bearer error=\"invalid_token\""),
                    )],
                    String::new(),
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json")),
                    session_header,
                ],
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "tools": [ { "name": "echo", "inputSchema": { "type": "object" } } ] }
                })
                .to_string(),
            )
                .into_response()
        }
        _ => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json")),
                session_header,
            ],
            json!({ "jsonrpc": "2.0", "id": id, "result": {} }).to_string(),
        )
            .into_response(),
    }
}

async fn spawn_mock(require_refresh: bool) -> (String, MockState) {
    let state = MockState {
        base_url: Arc::new(Mutex::new(String::new())),
        seen_auth_headers: Arc::new(Mutex::new(Vec::new())),
        require_refresh,
        refreshed: Arc::new(Mutex::new(false)),
        token_calls: Arc::new(Mutex::new(0)),
        well_known_calls: Arc::new(Mutex::new(0)),
    };
    let app = Router::new()
        .route("/.well-known/oauth-authorization-server", get(well_known))
        .route("/token", post(token_endpoint))
        .route("/mcp", post(mcp_handler))
        .with_state(state.clone());

    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    *state.base_url.lock().await = base.clone();

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // Poll the discovery endpoint until the server accepts connections rather
    // than relying on a fixed sleep, which is flaky under heavy parallel load.
    let probe = format!("{base}/.well-known/oauth-authorization-server");
    let client = reqwest::Client::new();
    for _ in 0..100 {
        if client.get(&probe).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    (base, state)
}

/// Seed a `MockSecretStore` with a `StoredCredentials` JSON blob under the
/// proxy's derived key, mimicking what `animus mcp auth` would write.
///
/// `fresh` controls `token_received_at`: when true the token was received
/// "now" (so a long `expires_in` is genuinely fresh); when false it was
/// received in 2001 (so any `expires_in` looks expired and forces a refresh).
fn seed_token(
    server: &str,
    principal: &str,
    url: &str,
    access_token: &str,
    expires_in: i64,
    fresh: bool,
) -> Arc<dyn SecretStore> {
    let key = animus_mcp_oauth::derive_keychain_key(server, principal, url);
    let received_at = if fresh {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    } else {
        1_000_000_000u64
    };
    let stored = json!({
        "client_id": "mock-client",
        "token_response": {
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": expires_in,
            "refresh_token": REFRESH_TOKEN,
            "scope": "repo"
        },
        "granted_scopes": ["repo"],
        "token_received_at": received_at
    });
    let store = MockSecretStore::new();
    store.set(&key, &stored.to_string()).unwrap();
    Arc::new(store)
}

fn tools_list_request() -> ClientRequest {
    ClientRequest::ListToolsRequest(ListToolsRequest::default())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_passes_bearer_through_to_upstream() {
    let (base, state) = spawn_mock(false).await;
    let url = format!("{base}/mcp");
    // Fresh token, long expiry → no refresh needed.
    let secrets = seed_token("github", "local", &url, ACCESS_TOKEN_FRESH, 3600, true);

    let proxy = animus_mcp_oauth::proxy::McpProxy::connect_with_store("github", &url, secrets, "local", None)
        .await
        .expect("proxy connects with stored token");

    let result = proxy.forward_request_for_test(tools_list_request()).await.expect("tools/list forwards");
    match result {
        ServerResult::ListToolsResult(list) => {
            assert_eq!(list.tools.len(), 1);
            assert_eq!(list.tools[0].name, "echo");
        }
        other => panic!("expected ListToolsResult, got {other:?}"),
    }

    // The upstream observed the live bearer on every request.
    let headers = state.seen_auth_headers.lock().await;
    assert!(!headers.is_empty(), "upstream should have seen requests");
    assert!(
        headers.iter().all(|h| h == &format!("Bearer {ACCESS_TOKEN_FRESH}")),
        "all upstream requests must carry the live bearer, saw: {headers:?}"
    );
    // No refresh should have happened.
    assert_eq!(*state.token_calls.lock().await, 0, "fresh token must not trigger a refresh");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upstream_401_triggers_refresh_and_retry() {
    let (base, state) = spawn_mock(true).await;
    let url = format!("{base}/mcp");
    // Expired access token + refresh token present. `expires_in: 1` with an
    // ancient `token_received_at` makes the proxy refresh proactively, and the
    // upstream additionally 401s until the refresh lands.
    let secrets = seed_token("linear", "local", &url, "stale-token", 1, false);

    let proxy = animus_mcp_oauth::proxy::McpProxy::connect_with_store("linear", &url, secrets, "local", None)
        .await
        .expect("proxy connects");

    let result = proxy.forward_request_for_test(tools_list_request()).await.expect("tools/list succeeds after refresh");
    match result {
        ServerResult::ListToolsResult(list) => assert_eq!(list.tools.len(), 1),
        other => panic!("expected ListToolsResult after refresh, got {other:?}"),
    }

    // The token endpoint must have been hit (refresh happened).
    assert!(*state.refreshed.lock().await, "refresh-token grant must have been exercised");
    // The final successful upstream call must carry the refreshed bearer.
    let headers = state.seen_auth_headers.lock().await;
    assert!(
        headers.iter().any(|h| h == &format!("Bearer {ACCESS_TOKEN_REFRESHED}")),
        "a request must carry the refreshed bearer, saw: {headers:?}"
    );
}

/// TASK-326 (fix a): with a discovery-metadata cache directory supplied, the
/// SECOND proxy connect for the same `(server, url)` must reuse the cached
/// `.well-known` OAuth metadata instead of re-running discovery against the
/// (throttle-prone) upstream. We assert the discovery endpoint is hit exactly
/// ONCE across two connects.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_metadata_cache_skips_rediscovery() {
    let (base, state) = spawn_mock(false).await;
    let url = format!("{base}/mcp");
    // Ignore discovery hits from the readiness probe in `spawn_mock`.
    *state.well_known_calls.lock().await = 0;

    let cache = tempfile::tempdir().expect("cache tempdir");

    // First connect: cache miss → one live discovery, then cached.
    let secrets1 = seed_token("krisp", "local", &url, ACCESS_TOKEN_FRESH, 3600, true);
    let proxy1 =
        animus_mcp_oauth::proxy::McpProxy::connect_with_store("krisp", &url, secrets1, "local", Some(cache.path()))
            .await
            .expect("first proxy connect");
    proxy1.forward_request_for_test(tools_list_request()).await.expect("tools/list on first connect");
    assert_eq!(*state.well_known_calls.lock().await, 1, "first connect must discover exactly once");

    // Second connect (same server+url+cache): cache hit → NO further discovery.
    let secrets2 = seed_token("krisp", "local", &url, ACCESS_TOKEN_FRESH, 3600, true);
    let proxy2 =
        animus_mcp_oauth::proxy::McpProxy::connect_with_store("krisp", &url, secrets2, "local", Some(cache.path()))
            .await
            .expect("second proxy connect");
    proxy2.forward_request_for_test(tools_list_request()).await.expect("tools/list on second connect");

    assert_eq!(
        *state.well_known_calls.lock().await,
        1,
        "second connect must reuse cached discovery metadata, not re-hit the upstream .well-known endpoint"
    );
}

/// TASK-326 (fix a, unauthenticated gate): with NO stored token for the server,
/// connecting must fail fast WITHOUT running discovery priming. Priming a live
/// `.well-known` discovery for an unauthenticated server would regress the
/// fast-fail startup path into a needless network round-trip. We assert the
/// connect errors and the discovery endpoint is never hit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauthenticated_server_skips_discovery_priming() {
    let (base, state) = spawn_mock(false).await;
    let url = format!("{base}/mcp");
    // Ignore discovery hits from the readiness probe in `spawn_mock`.
    *state.well_known_calls.lock().await = 0;

    let cache = tempfile::tempdir().expect("cache tempdir");

    // Empty secret store: no token stored for this server.
    let secrets: Arc<dyn SecretStore> = Arc::new(MockSecretStore::new());
    let result =
        animus_mcp_oauth::proxy::McpProxy::connect_with_store("unauthed", &url, secrets, "local", Some(cache.path()))
            .await;

    assert!(result.is_err(), "connect must fail when no token is stored");
    assert_eq!(
        *state.well_known_calls.lock().await,
        0,
        "an unauthenticated server (no stored token) must skip discovery priming and fast-fail"
    );
}
