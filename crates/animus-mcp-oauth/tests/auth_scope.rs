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

#[derive(Clone)]
struct AsState {
    base_url: Arc<Mutex<String>>,
    /// Set true when the well-known discovery endpoint is hit.
    discovered: Arc<Mutex<bool>>,
}

async fn well_known(State(state): State<AsState>) -> impl IntoResponse {
    *state.discovered.lock().await = true;
    let base = state.base_url.lock().await.clone();
    (
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": ["S256"],
            // Advertise several OPTIONAL scopes. The least-privilege default
            // must NOT request any of these when none are configured.
            "scopes_supported": ["all_accounts", "trade", "read:positions", "offline_access"]
        })
        .to_string(),
    )
}

async fn spawn_as() -> (String, AsState) {
    let state = AsState { base_url: Arc::new(Mutex::new(String::new())), discovered: Arc::new(Mutex::new(false)) };
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

/// Dry run with no configured scopes must resolve to an EMPTY request set —
/// NOT the server's full advertised `scopes_supported` list — and obtain no
/// token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_default_scopes_are_empty_not_full_advertised_set() {
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
            assert!(
                dry.requested_scopes.is_empty(),
                "default must request NO scopes (least-privilege), got {:?}",
                dry.requested_scopes
            );
            assert!(!dry.authorized, "dry run must not authorize");
            assert!(dry.would_register_client, "no pinned client_id → DCR");
            assert_eq!(dry.base_url, base);
        }
        AuthResult::Completed(_) => panic!("dry run must not complete a real auth"),
    }
}

/// Explicit `--scopes` are requested as-is in a dry run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_explicit_scopes_requested_as_is() {
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
        AuthResult::DryRun(dry) => assert_eq!(dry.requested_scopes, scopes),
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

/// A "no" consent answer aborts BEFORE any network discovery or browser open.
/// Proven by pointing at an unreachable URL: a deny returns the cancellation
/// error, never a discovery error, and the mock AS is never contacted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consent_deny_aborts_before_discovery_and_browser() {
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
    assert!(!*state.discovered.lock().await, "consent deny must abort before discovery");
}

/// The consent callback receives the resolved (least-privilege) scope set so a
/// caller can audit request breadth before approving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consent_callback_sees_least_privilege_scopes() {
    let (base, _state) = spawn_as().await;
    let root = project_root();

    let seen: Arc<std::sync::Mutex<Option<Vec<String>>>> = Arc::new(std::sync::Mutex::new(None));
    let seen_cb = seen.clone();
    let opts = RunAuthOptions {
        url_override: Some(base.as_str()),
        scopes_override: None,
        assume_yes: false,
        json: false,
        dry_run: false,
        confirm: Confirm::Callback(Box::new(move |preview| {
            *seen_cb.lock().unwrap() = Some(preview.requested_scopes.clone());
            // Deny so we stop before binding the loopback listener / browser.
            ConfirmDecision::Deny
        })),
    };
    let _ = animus_mcp_oauth::run_auth(root.path(), "robinhood-trading", opts).await;
    let captured = seen.lock().unwrap().clone();
    assert_eq!(captured, Some(Vec::<String>::new()), "callback must see the empty least-privilege set");
}
