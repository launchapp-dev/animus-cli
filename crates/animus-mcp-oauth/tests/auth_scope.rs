//! Integration tests for `run_auth`'s scope posture and consent gate.
//!
//! These exercise the public `run_auth` entry point end to end up to (but not
//! through) the browser open / token exchange. The consent gate is injected via
//! `Confirm::Callback` so no TTY is required, and `--dry-run` is covered with a
//! mock authorization server so scope resolution is observable without a token.

use std::net::SocketAddr;
use std::sync::Arc;

use animus_mcp_oauth::{AuthResult, Confirm, ConfirmDecision, RunAuthOptions};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use tempfile::TempDir;
use tokio::sync::Mutex;

/// The advertised `scopes_supported` returned by the mock authorization
/// server's discovery metadata.
const ADVERTISED_SCOPES: &[&str] = &["all_accounts", "trade", "read:positions", "offline_access"];

#[derive(Clone)]
struct AsState {
    base_url: Arc<Mutex<String>>,
    /// Set true when the well-known discovery endpoint is hit.
    discovered: Arc<Mutex<bool>>,
    /// When true, discovery omits `scopes_supported` entirely.
    advertise_no_scopes: bool,
}

async fn well_known(State(state): State<AsState>) -> impl IntoResponse {
    *state.discovered.lock().await = true;
    let base = state.base_url.lock().await.clone();
    let mut body = serde_json::json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
    });
    if !state.advertise_no_scopes {
        // Advertise scopes. With no --scopes / config, the flow auto-detects and
        // adopts this advertised set (marked auto-detected), so a server that
        // requires a scope works out of the box. Explicit --scopes / config
        // still override it.
        body["scopes_supported"] = serde_json::json!(ADVERTISED_SCOPES);
    }
    (axum::http::StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "application/json")], body.to_string())
}

async fn spawn_as() -> (String, AsState) {
    spawn_as_with(false).await
}

/// Spawn a mock AS whose discovery metadata omits `scopes_supported`.
async fn spawn_as_no_scopes() -> (String, AsState) {
    spawn_as_with(true).await
}

async fn spawn_as_with(advertise_no_scopes: bool) -> (String, AsState) {
    let state = AsState {
        base_url: Arc::new(Mutex::new(String::new())),
        discovered: Arc::new(Mutex::new(false)),
        advertise_no_scopes,
    };
    let app = Router::new().route("/.well-known/oauth-authorization-server", get(well_known)).with_state(state.clone());

    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    *state.base_url.lock().await = base.clone();

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let probe = format!("{base}/.well-known/oauth-authorization-server");
    let client = reqwest::Client::new();
    for _ in 0..100 {
        if client.get(&probe).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // Reset the discovered flag the probe above may have set.
    *state.discovered.lock().await = false;
    (base, state)
}

fn project_root() -> TempDir {
    TempDir::new().unwrap()
}

/// Dry run with no configured scopes auto-detects the server's advertised
/// `scopes_supported` and marks them auto-detected. (This is the Robinhood fix:
/// a server that requires a scope now works out of the box.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_default_scopes_auto_detected_from_advertisement() {
    let (base, _state) = spawn_as().await;
    let root = project_root();

    let opts = RunAuthOptions {
        url_override: Some(base.as_str()),
        scopes_override: None,
        assume_yes: false,
        json: true,
        dry_run: true,
        confirm: Confirm::Interactive,
    };
    let result = animus_mcp_oauth::run_auth(root.path(), "robinhood-trading", opts).await.unwrap();
    match result {
        AuthResult::DryRun(dry) => {
            assert_eq!(
                dry.requested_scopes, ADVERTISED_SCOPES,
                "default must adopt the advertised scopes, got {:?}",
                dry.requested_scopes
            );
            assert!(dry.scopes_auto_detected, "advertised scopes must be flagged auto-detected");
            assert!(!dry.authorized, "dry run must not authorize");
            assert!(dry.would_register_client, "no pinned client_id → DCR");
            assert_eq!(dry.base_url, base);
        }
        AuthResult::Completed(_) => panic!("dry run must not complete a real auth"),
    }
}

/// A server that advertises NO `scopes_supported` keeps today's behavior:
/// request none (empty, not flagged auto-detected) so the server applies its
/// own minimal default.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_no_advertised_scopes_requests_none() {
    let (base, _state) = spawn_as_no_scopes().await;
    let root = project_root();

    let opts = RunAuthOptions {
        url_override: Some(base.as_str()),
        scopes_override: None,
        assume_yes: false,
        json: true,
        dry_run: true,
        confirm: Confirm::Interactive,
    };
    let result = animus_mcp_oauth::run_auth(root.path(), "minimal", opts).await.unwrap();
    match result {
        AuthResult::DryRun(dry) => {
            assert!(
                dry.requested_scopes.is_empty(),
                "no advertised scopes → request none, got {:?}",
                dry.requested_scopes
            );
            assert!(!dry.scopes_auto_detected, "empty set is not auto-detected");
        }
        AuthResult::Completed(_) => panic!("dry run must not complete"),
    }
}

/// Explicit `--scopes` are requested as-is in a dry run and override the
/// advertised set (NOT flagged auto-detected).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_explicit_scopes_override_advertised() {
    let (base, _state) = spawn_as().await;
    let root = project_root();

    let scopes = vec!["read:positions".to_string(), "trade".to_string()];
    let opts = RunAuthOptions {
        url_override: Some(base.as_str()),
        scopes_override: Some(&scopes),
        assume_yes: false,
        json: true,
        dry_run: true,
        confirm: Confirm::Interactive,
    };
    let result = animus_mcp_oauth::run_auth(root.path(), "robinhood-trading", opts).await.unwrap();
    match result {
        AuthResult::DryRun(dry) => {
            assert_eq!(dry.requested_scopes, scopes);
            assert!(!dry.scopes_auto_detected, "explicit --scopes are not auto-detected");
        }
        AuthResult::Completed(_) => panic!("dry run must not complete"),
    }
}

/// `--scopes none` opts out of auto-detection: against a server that advertises
/// scopes, it forces an EMPTY request (not flagged auto-detected) so the server
/// applies its own minimal default.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_scopes_none_sentinel_opts_out_of_auto_detect() {
    let (base, _state) = spawn_as().await;
    let root = project_root();

    let scopes = vec!["none".to_string()];
    let opts = RunAuthOptions {
        url_override: Some(base.as_str()),
        scopes_override: Some(&scopes),
        assume_yes: false,
        json: true,
        dry_run: true,
        confirm: Confirm::Interactive,
    };
    let result = animus_mcp_oauth::run_auth(root.path(), "robinhood-trading", opts).await.unwrap();
    match result {
        AuthResult::DryRun(dry) => {
            assert!(dry.requested_scopes.is_empty(), "`--scopes none` must force an empty request");
            assert!(!dry.scopes_auto_detected, "opt-out empty is not auto-detected");
        }
        AuthResult::Completed(_) => panic!("dry run must not complete"),
    }
}

/// A dry run validates the OAuth endpoint via discovery: an unreachable /
/// misconfigured server surfaces a discovery error rather than reporting a
/// misleading success. (No browser is opened and no token is obtained either
/// way.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_surfaces_discovery_failure() {
    let root = project_root();
    // Port 0 on loopback is not listening: discovery must fail.
    let opts = RunAuthOptions {
        url_override: Some("http://127.0.0.1:1/"),
        scopes_override: None,
        assume_yes: false,
        json: true,
        dry_run: true,
        confirm: Confirm::Interactive,
    };
    let err = animus_mcp_oauth::run_auth(root.path(), "broken", opts).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("discovery") || msg.contains("OAuth manager"),
        "dry run should surface a discovery/connection error, got: {msg}"
    );
}

/// A dry run is read-only with respect to credentials: it must not construct
/// the keychain-backed secret store, so no `secrets`/`secrets.json` token-store
/// file is materialized under the scoped state root. (The scope DIRECTORY may
/// still be created by config resolution, which dry-run needs to resolve the
/// URL + scopes — that is read-path infrastructure, not a credential write.)
#[tokio::test(flavor = "current_thread")]
async fn dry_run_does_not_build_the_secret_store() {
    let (base, _state) = spawn_as().await;
    let root = project_root();
    let home = TempDir::new().unwrap();
    let _home_guard = protocol::test_utils::EnvVarGuard::set("HOME", Some(home.path().to_str().unwrap()));

    let opts = RunAuthOptions {
        url_override: Some(base.as_str()),
        scopes_override: None,
        assume_yes: false,
        json: true,
        dry_run: true,
        confirm: Confirm::Interactive,
    };
    let result = animus_mcp_oauth::run_auth(root.path(), "robinhood-trading", opts).await.unwrap();
    assert!(matches!(result, AuthResult::DryRun(_)));

    // No keyring/secrets token-store file is written anywhere under the pinned
    // home. (The keyring backend used in CI may be a JSON file under the scope
    // root; assert none exists.)
    let animus = home.path().join(".animus");
    if animus.exists() {
        let mut found_secret_artifact = false;
        for entry in walk(&animus) {
            let name = entry.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_ascii_lowercase();
            if name.contains("secret") || name.contains("keyring") || name.contains("keychain") {
                found_secret_artifact = true;
            }
        }
        assert!(!found_secret_artifact, "dry run must not write a secret/keyring store under {}", animus.display());
    }
}

/// Recursively collect every path under `dir` (files + dirs).
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}

/// A "no" consent answer aborts before the browser opens and obtains no token.
/// Consent now runs AFTER discovery (the preview must show the actual,
/// possibly auto-detected, scopes), so discovery DOES run — but a deny still
/// returns the cancellation error before any callback bind / browser open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consent_deny_aborts_before_browser() {
    let (base, state) = spawn_as().await;
    let root = project_root();

    let opts = RunAuthOptions {
        url_override: Some(base.as_str()),
        scopes_override: None,
        assume_yes: false,
        json: false,
        dry_run: false,
        confirm: Confirm::Callback(Box::new(|_| ConfirmDecision::Deny)),
    };
    let err = animus_mcp_oauth::run_auth(root.path(), "robinhood-trading", opts).await.unwrap_err();
    assert!(err.to_string().contains("cancelled"), "expected cancellation error, got: {err}");
    assert!(
        *state.discovered.lock().await,
        "consent now runs after discovery so the preview can show auto-detected scopes"
    );
}

/// The consent callback receives the resolved scope set — including scopes
/// auto-detected from the server's advertisement — so a caller can audit
/// request breadth (and the auto-detected flag) before approving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consent_callback_sees_auto_detected_scopes() {
    let (base, _state) = spawn_as().await;
    let root = project_root();

    let seen: Arc<std::sync::Mutex<Option<(Vec<String>, bool)>>> = Arc::new(std::sync::Mutex::new(None));
    let seen_cb = seen.clone();
    let opts = RunAuthOptions {
        url_override: Some(base.as_str()),
        scopes_override: None,
        assume_yes: false,
        json: false,
        dry_run: false,
        confirm: Confirm::Callback(Box::new(move |preview| {
            *seen_cb.lock().unwrap() = Some((preview.requested_scopes.clone(), preview.scopes_auto_detected));
            // Deny so we stop before binding the loopback listener / browser.
            ConfirmDecision::Deny
        })),
    };
    let _ = animus_mcp_oauth::run_auth(root.path(), "robinhood-trading", opts).await;
    let captured = seen.lock().unwrap().clone();
    assert_eq!(
        captured,
        Some((ADVERTISED_SCOPES.iter().map(|s| s.to_string()).collect(), true)),
        "callback must see the auto-detected advertised set, flagged auto-detected"
    );
}
