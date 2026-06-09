//! Interactive `authorization_code` + PKCE flow, status, and logout.
//!
//! The flow drives rmcp 1.7's [`AuthorizationManager`] and
//! [`AuthorizationSession`] directly (rather than the higher-level
//! `OAuthState`) so the keychain-backed [`KeychainCredentialStore`] can be
//! injected before discovery — `OAuthState`'s constructors create their own
//! internal managers and don't expose store injection. The protocol itself
//! (discovery, DCR, PKCE, code exchange, refresh) is entirely rmcp's; this
//! module only orchestrates: bind callback → discover → register/configure →
//! open browser → capture code → exchange → persist.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rmcp::transport::auth::{
    AuthorizationManager, AuthorizationSession, CredentialStore, InMemoryStateStore, OAuthClientConfig,
    StoredCredentials,
};
use serde::Serialize;

/// Holds the rmcp object that completes the code→token exchange, chosen by the
/// pinned-client_id branch above. `Manager` drives the exchange directly (no
/// DCR); `Session` defers to rmcp's `AuthorizationSession` (which ran DCR).
enum Exchange {
    Manager(AuthorizationManager),
    Session(AuthorizationSession),
}

use crate::callback::CallbackListener;
use crate::config::{build_secret_store, resolve_principal_id, resolve_server_url};
use crate::keychain_store::KeychainCredentialStore;
use crate::{CALLBACK_TIMEOUT_SECS, DEFAULT_CLIENT_NAME};

/// Successful interactive-auth outcome, returned to the CLI for display.
#[derive(Debug, Clone, Serialize)]
pub struct AuthOutcome {
    pub server: String,
    pub principal: String,
    pub client_id: String,
    pub granted_scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    /// True when the issued token bundle carries a refresh token (so the
    /// proxy can refresh silently without a re-login).
    pub has_refresh_token: bool,
}

/// Per-server auth state for the `auth-status` surface.
#[derive(Debug, Clone, Serialize)]
pub struct ServerAuthState {
    pub server: String,
    pub principal: String,
    pub authenticated: bool,
    pub client_id: Option<String>,
    pub granted_scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub expired: bool,
    pub has_refresh_token: bool,
}

/// Aggregate `auth-status` report.
#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub servers: Vec<ServerAuthState>,
}

/// Run the interactive authorization-code flow for `server`.
///
/// Resolves the server URL (config or `url_override`), binds a loopback
/// callback, discovers the auth server, performs Dynamic Client
/// Registration (or uses a pinned `client_id`), opens the browser, captures
/// the redirect, exchanges the code, and persists tokens in the keychain.
///
/// `scopes_override` (CLI `--scopes`) wins over the config's scope list.
pub async fn run_auth(
    project_root: &Path,
    server: &str,
    url_override: Option<&str>,
    scopes_override: Option<&[String]>,
) -> Result<AuthOutcome> {
    crate::ensure_crypto_provider();
    let resolution = resolve_server_url(project_root, server, url_override)?;
    let principal = resolve_principal_id(project_root);
    let secrets = build_secret_store(project_root)?;
    let cred_store = KeychainCredentialStore::new(secrets, server, &principal, &resolution.url);

    // Explicitly-configured scopes (CLI `--scopes` > config `scopes`) win.
    // When none are configured we defer to rmcp's `select_scopes` AFTER
    // discovery, which picks up the scopes advertised by protected-resource /
    // authorization-server metadata and appends `offline_access` when the AS
    // supports it (so the issued token is refreshable). Freezing an empty
    // scope set here would otherwise skip those required/refresh scopes.
    let configured_scopes: Vec<String> = match scopes_override {
        Some(s) if !s.is_empty() => s.to_vec(),
        _ => resolution.scopes.clone(),
    };

    // The `AuthorizationManager` base URL is BOTH the OAuth `resource`
    // indicator (RFC 8707) and the discovery seed in rmcp 1.7. It is the
    // protected MCP URL so the issued token's audience matches the server the
    // proxy will call; rmcp follows the protected-resource-metadata chain
    // (RFC 9728) to find the authorization server.
    let mut manager = AuthorizationManager::new(&resolution.url)
        .await
        .map_err(|err| anyhow!("failed to initialize OAuth manager for `{server}`: {err}"))?;
    manager.set_credential_store(cred_store);
    manager.set_state_store(InMemoryStateStore::new());

    let metadata = manager
        .discover_metadata()
        .await
        .map_err(|err| anyhow!("OAuth discovery failed for `{server}` at {}: {err}", resolution.url))?;
    manager.set_metadata(metadata);

    // Resolve the final scope set now that metadata is available.
    let scopes: Vec<String> =
        if configured_scopes.is_empty() { manager.select_scopes(None, &[]) } else { configured_scopes };
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();

    // Bind the loopback callback before generating the authorization URL so
    // the redirect_uri is registered with the right port.
    let callback = CallbackListener::bind().await?;
    let redirect_uri = callback.redirect_uri().to_string();

    // Two paths:
    // - Pinned `client_id`: configure the client directly and drive the
    //   manager's authorize/exchange — `AuthorizationSession::new` would
    //   unconditionally run Dynamic Client Registration and clobber the
    //   pinned id, failing on servers that don't support DCR.
    // - No pinned id: `AuthorizationSession::new` performs DCR.
    //
    // A blank/whitespace-only client_id is treated as unset (→ DCR) so a
    // config typo doesn't skip registration with an empty id; validation also
    // rejects it up front.
    let pinned_client_id = resolution.client_id.as_deref().map(str::trim).filter(|id| !id.is_empty());
    let (auth_url, csrf_state, exchange) = if let Some(client_id) = pinned_client_id {
        let config = OAuthClientConfig::new(client_id.to_string(), redirect_uri.clone()).with_scopes(scopes.clone());
        manager
            .configure_client(config)
            .map_err(|err| anyhow!("failed to configure pinned client_id for `{server}`: {err}"))?;
        let auth_url = manager
            .get_authorization_url(&scope_refs)
            .await
            .map_err(|err| anyhow!("failed to build authorization URL for `{server}`: {err}"))?;
        let csrf = extract_state_param(&auth_url)
            .ok_or_else(|| anyhow!("authorization URL is missing the `state` parameter"))?;
        (auth_url, csrf, Exchange::Manager(manager))
    } else {
        let session = AuthorizationSession::new(manager, &scope_refs, &redirect_uri, Some(DEFAULT_CLIENT_NAME), None)
            .await
            .map_err(|err| anyhow!("failed to start authorization session for `{server}`: {err}"))?;
        let auth_url = session.get_authorization_url().to_string();
        let csrf = extract_state_param(&auth_url)
            .ok_or_else(|| anyhow!("authorization URL is missing the `state` parameter"))?;
        (auth_url, csrf, Exchange::Session(session))
    };

    // Open the browser. Never log the full URL — it carries the PKCE
    // challenge + state.
    tracing::info!(server, "opening browser for OAuth login");
    if webbrowser::open(&auth_url).is_err() {
        // Headless / no browser: surface the URL on STDERR so the user can
        // open it manually without corrupting a `--json` stdout envelope.
        // This is the one place the URL is shown, and it goes to the user's
        // own terminal, never the structured logs.
        eprintln!("Open this URL in your browser to authorize `{server}`:\n{auth_url}");
    }

    let captured = callback.wait_for_code(&csrf_state, Duration::from_secs(CALLBACK_TIMEOUT_SECS)).await?;

    let (token, client_id) = match exchange {
        Exchange::Manager(manager) => {
            let token = manager
                .exchange_code_for_token(&captured.code, &captured.state)
                .await
                .map_err(|err| anyhow!("token exchange failed for `{server}`: {err}"))?;
            let (client_id, _) = manager
                .get_credentials()
                .await
                .map_err(|err| anyhow!("failed to read back stored credentials for `{server}`: {err}"))?;
            (token, client_id)
        }
        Exchange::Session(session) => {
            let token = session
                .handle_callback(&captured.code, &captured.state)
                .await
                .map_err(|err| anyhow!("token exchange failed for `{server}`: {err}"))?;
            let (client_id, _) = session
                .get_credentials()
                .await
                .map_err(|err| anyhow!("failed to read back stored credentials for `{server}`: {err}"))?;
            (token, client_id)
        }
    };

    let token_value = serde_json::to_value(&token).unwrap_or_default();
    let granted_scopes = scopes_from_token_value(&token_value).unwrap_or_else(|| scopes.clone());
    let has_refresh_token = token_value.get("refresh_token").and_then(|v| v.as_str()).is_some();
    let expires_at = expires_at_from_token_value(&token_value, Utc::now());

    Ok(AuthOutcome { server: server.to_string(), principal, client_id, granted_scopes, expires_at, has_refresh_token })
}

/// Report auth state for one server (when `server` is `Some`) or for every
/// server with an `authorization_code` oauth block in config.
///
/// `url_override` (only meaningful with a single `server`) addresses a
/// URL-bound token for a server not present in config — the same URL the
/// `mcp auth --url` invocation used.
pub async fn auth_status(project_root: &Path, server: Option<&str>, url_override: Option<&str>) -> Result<AuthStatus> {
    let principal = resolve_principal_id(project_root);
    let secrets = build_secret_store(project_root)?;

    let servers: Vec<String> = match server {
        Some(s) => vec![s.to_string()],
        None => authorization_code_servers(project_root),
    };

    let mut out = Vec::with_capacity(servers.len());
    for name in servers {
        // Tokens are keyed by the upstream URL too, so resolve it. A server
        // that can't be resolved (e.g. dropped from config and no --url) is
        // reported as unauthenticated rather than failing the whole report.
        let Some(url) = resolve_server_url(project_root, &name, url_override).ok().map(|r| r.url) else {
            out.push(server_state_from_creds(&name, &principal, None));
            continue;
        };
        let store = KeychainCredentialStore::new(secrets.clone(), &name, &principal, &url);
        let creds =
            store.load().await.map_err(|err| anyhow!("failed to read stored credentials for `{name}`: {err}"))?;
        out.push(server_state_from_creds(&name, &principal, creds));
    }
    Ok(AuthStatus { servers: out })
}

/// Delete stored tokens for `server`.
///
/// `url_override` addresses a URL-bound token for a server not in config (the
/// same URL the `mcp auth --url` invocation used).
pub async fn auth_logout(project_root: &Path, server: &str, url_override: Option<&str>) -> Result<bool> {
    let principal = resolve_principal_id(project_root);
    let secrets = build_secret_store(project_root)?;
    let url = resolve_server_url(project_root, server, url_override)?.url;
    let store = KeychainCredentialStore::new(secrets, server, &principal, &url);
    let had =
        store.load().await.map_err(|err| anyhow!("failed to read stored credentials for `{server}`: {err}"))?.is_some();
    store.clear().await.map_err(|err| anyhow!("failed to clear tokens for `{server}`: {err}"))?;
    Ok(had)
}

fn server_state_from_creds(server: &str, principal: &str, creds: Option<StoredCredentials>) -> ServerAuthState {
    let now = Utc::now();
    match creds {
        Some(c) if c.token_response.is_some() => {
            let token_value = serde_json::to_value(c.token_response.as_ref().unwrap()).unwrap_or_default();
            let expires_at = c.token_received_at.and_then(|received| expires_at_from_received(&token_value, received));
            let expired = expires_at.map(|e| e <= now).unwrap_or(false);
            let has_refresh_token = token_value.get("refresh_token").and_then(|v| v.as_str()).is_some();
            ServerAuthState {
                server: server.to_string(),
                principal: principal.to_string(),
                authenticated: true,
                client_id: Some(c.client_id),
                granted_scopes: c.granted_scopes,
                expires_at,
                expired,
                has_refresh_token,
            }
        }
        _ => ServerAuthState {
            server: server.to_string(),
            principal: principal.to_string(),
            authenticated: false,
            client_id: None,
            granted_scopes: Vec::new(),
            expires_at: None,
            expired: false,
            has_refresh_token: false,
        },
    }
}

/// Servers in workflow/project config carrying an `authorization_code`
/// oauth flow.
fn authorization_code_servers(project_root: &Path) -> Vec<String> {
    use orchestrator_config::workflow_config::{load_workflow_config_or_default, OauthFlow};
    let mut names = std::collections::BTreeSet::new();

    let loaded = load_workflow_config_or_default(project_root);
    for (name, def) in &loaded.config.mcp_servers {
        if def.oauth.as_ref().is_some_and(|o| o.flow == OauthFlow::AuthorizationCode) {
            names.insert(name.clone());
        }
    }

    if let Ok(project_config) = protocol::Config::load_or_default(&project_root.display().to_string()) {
        for (name, entry) in &project_config.mcp_servers {
            let is_auth_code = entry.oauth.as_ref().is_some_and(|value| {
                serde_json::from_value::<orchestrator_config::OauthConfig>(value.clone())
                    .map(|cfg| cfg.flow == OauthFlow::AuthorizationCode)
                    .unwrap_or(false)
            });
            if is_auth_code {
                names.insert(name.clone());
            }
        }
    }

    names.into_iter().collect()
}

/// Extract the `state` query parameter from an authorization URL.
fn extract_state_param(auth_url: &str) -> Option<String> {
    let parsed = url::Url::parse(auth_url).ok()?;
    parsed.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.into_owned())
}

fn scopes_from_token_value(token: &serde_json::Value) -> Option<Vec<String>> {
    token.get("scope").and_then(|v| v.as_str()).map(|s| s.split_whitespace().map(str::to_string).collect())
}

fn expires_at_from_token_value(token: &serde_json::Value, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let expires_in = token.get("expires_in").and_then(serde_json::Value::as_i64)?;
    Some(now + chrono::Duration::seconds(expires_in))
}

fn expires_at_from_received(token: &serde_json::Value, received_at: u64) -> Option<DateTime<Utc>> {
    let expires_in = token.get("expires_in").and_then(serde_json::Value::as_i64)?;
    let received = DateTime::<Utc>::from_timestamp(received_at as i64, 0)?;
    Some(received + chrono::Duration::seconds(expires_in))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_state_param() {
        let url = "https://auth.example.com/authorize?response_type=code&state=abc123&scope=repo";
        assert_eq!(extract_state_param(url).as_deref(), Some("abc123"));
    }

    #[test]
    fn missing_state_param_returns_none() {
        let url = "https://auth.example.com/authorize?response_type=code";
        assert_eq!(extract_state_param(url), None);
    }

    #[test]
    fn scopes_parse_from_token_value() {
        let token = serde_json::json!({"scope": "repo read:user"});
        assert_eq!(scopes_from_token_value(&token), Some(vec!["repo".to_string(), "read:user".to_string()]));
    }

    #[test]
    fn expires_at_computed_from_expires_in() {
        let token = serde_json::json!({"expires_in": 3600});
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let expires = expires_at_from_token_value(&token, now).unwrap();
        assert_eq!(expires, now + chrono::Duration::seconds(3600));
    }

    #[test]
    fn server_state_unauthenticated_when_no_creds() {
        let state = server_state_from_creds("github", "local", None);
        assert!(!state.authenticated);
        assert_eq!(state.server, "github");
        assert_eq!(state.principal, "local");
        assert!(state.client_id.is_none());
    }

    #[test]
    fn server_state_authenticated_and_expiry_computed() {
        let token_json = serde_json::json!({
            "access_token": "at",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "rt"
        });
        let token: rmcp::transport::auth::OAuthTokenResponse = serde_json::from_value(token_json).unwrap();
        let creds =
            StoredCredentials::new("client-1".to_string(), Some(token), vec!["repo".to_string()], Some(1_700_000_000));
        let state = server_state_from_creds("github", "local", Some(creds));
        assert!(state.authenticated);
        assert_eq!(state.client_id.as_deref(), Some("client-1"));
        assert!(state.has_refresh_token);
        assert_eq!(state.granted_scopes, vec!["repo".to_string()]);
        assert!(state.expires_at.is_some());
    }
}
