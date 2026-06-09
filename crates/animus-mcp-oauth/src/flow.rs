//! Interactive `authorization_code` + PKCE flow, status, and logout.
//!
//! The flow drives rmcp 1.7's [`AuthorizationManager`] and
//! [`AuthorizationSession`] directly (rather than the higher-level
//! `OAuthState`) so the keychain-backed [`KeychainCredentialStore`] can be
//! injected explicitly — `OAuthState`'s constructors create their own internal
//! managers and don't expose store injection. The protocol itself (discovery,
//! DCR, PKCE, code exchange, refresh) is entirely rmcp's; this module only
//! orchestrates: resolve least-privilege scopes → preview/confirm → discover →
//! (dry-run stops here) → attach credential store → bind callback →
//! register/configure → open browser → capture code → exchange → persist.
//!
//! The keychain-backed credential store is attached only AFTER the
//! consent/dry-run returns: constructing it materializes the OS-keychain token
//! store, which a read-only `--dry-run` or an aborted login must not touch.
//! Discovery does not consult the credential store, so attaching it
//! post-discovery is safe.

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
    /// Scopes the flow asked the authorization server for (least-privilege:
    /// empty when none were configured, so the server applies its own
    /// default). Surfaced so a caller can audit the request breadth.
    pub requested_scopes: Vec<String>,
    pub granted_scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    /// True when the issued token bundle carries a refresh token (so the
    /// proxy can refresh silently without a re-login).
    pub has_refresh_token: bool,
}

/// Decision returned by a confirm callback before the browser opens.
///
/// Injectable so tests can drive the confirm gate without a TTY. Default is
/// [`ConfirmDecision::Deny`] for any non-affirmative answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmDecision {
    /// Proceed: open the browser and complete the flow.
    Proceed,
    /// Abort before opening the browser or binding the callback listener.
    Deny,
}

/// How the consent gate is resolved before opening the browser.
///
/// The variants exist so the security-sensitive "open browser?" decision is
/// injectable in tests (no TTY required) while production reads a real y/N.
pub enum Confirm {
    /// Skip the prompt and proceed (used under `--yes` / `--json`).
    AutoProceed,
    /// Read a y/N answer from stdin (interactive default, default No).
    Interactive,
    /// Test/embedding hook: decide from the resolved preview.
    Callback(Box<dyn FnOnce(&AuthPreview) -> ConfirmDecision + Send>),
}

/// What `run_auth` resolved before any browser is opened: the exact scopes,
/// server, and base URL shown in the preview / consent prompt.
///
/// Never carries the authorization URL, code, state, or PKCE material — only
/// the breadth a user needs to make a consent decision.
#[derive(Debug, Clone, Serialize)]
pub struct AuthPreview {
    pub server: String,
    pub base_url: String,
    pub requested_scopes: Vec<String>,
}

/// Result of `animus mcp auth <server> --dry-run`: discovery + scope
/// resolution without opening a browser, binding the callback, or exchanging
/// any token. No credentials are obtained.
#[derive(Debug, Clone, Serialize)]
pub struct DryRunOutcome {
    pub server: String,
    pub base_url: String,
    pub requested_scopes: Vec<String>,
    /// True when the flow would run Dynamic Client Registration (no pinned
    /// `client_id` configured); false when a pinned id would be used.
    pub would_register_client: bool,
    /// Always false: a dry run never reaches token exchange.
    pub authorized: bool,
}

/// Either a completed interactive auth or a dry-run preview.
#[derive(Debug, Clone)]
pub enum AuthResult {
    Completed(AuthOutcome),
    DryRun(DryRunOutcome),
}

/// Options for [`run_auth`].
///
/// `assume_yes` and `json` both skip the interactive consent prompt; `json`
/// additionally signals the caller will render a machine envelope (so the
/// preview is not printed to stderr). `dry_run` stops after scope resolution.
pub struct RunAuthOptions<'a> {
    pub url_override: Option<&'a str>,
    pub scopes_override: Option<&'a [String]>,
    pub assume_yes: bool,
    pub json: bool,
    pub dry_run: bool,
    pub confirm: Confirm,
}

impl Default for RunAuthOptions<'_> {
    fn default() -> Self {
        Self {
            url_override: None,
            scopes_override: None,
            assume_yes: false,
            json: false,
            dry_run: false,
            confirm: Confirm::Interactive,
        }
    }
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

/// Resolve the scopes to request from the configured scope sources.
///
/// Precedence: CLI `--scopes` (`scopes_override`) > config `scopes:`. When
/// neither is set, returns an EMPTY set.
///
/// Security: an empty result is deliberate least-privilege. We do NOT fall
/// back to the server's full advertised `scopes_supported` set (rmcp's
/// `select_scopes`), because that path surfaced an over-broad "all accounts"
/// consent screen — the authorization server applies its own minimal default
/// when we request nothing.
///
/// Known limitation (rmcp 1.7 API): rmcp's `select_scopes` blends THREE
/// sources with no way to pick just one — (a) the `WWW-Authenticate` `scope`
/// the server demanded in its initial 401 (the genuine "required" set), then
/// (b)/(c) the full advertised `scopes_supported` from protected-resource and
/// authorization-server metadata (the over-broad set). The required set (a) is
/// stored in a PRIVATE field with no public accessor, so we cannot request
/// "required only" without also pulling in the broad (b)/(c) fallback that
/// caused the bug. We therefore default to empty (the safe choice) and require
/// explicit `--scopes`/config `scopes:` for servers that demand specific
/// scopes — the consent screen or a `403 insufficient_scope` from the proxy
/// will name them. See `docs/reference/mcp-oauth.md#scope-posture`.
fn resolve_requested_scopes(scopes_override: Option<&[String]>, config_scopes: &[String]) -> Vec<String> {
    // TODO(codex-p2): if rmcp later exposes the WWW-Authenticate "required"
    // scopes in isolation (separate from the full advertised set), default to
    // requesting exactly those instead of empty, so required-scope servers work
    // out of the box without losing least-privilege.
    match scopes_override {
        Some(s) if !s.is_empty() => s.to_vec(),
        _ => config_scopes.to_vec(),
    }
}

/// Render the requested scopes for the consent preview.
fn scopes_display(scopes: &[String]) -> String {
    if scopes.is_empty() {
        "(none — server default / minimal)".to_string()
    } else {
        scopes.join(", ")
    }
}

/// Read an interactive y/N consent from stdin. Default No: anything other than
/// an explicit `y`/`yes` (case-insensitive) is a deny.
fn prompt_consent_stdin() -> ConfirmDecision {
    use std::io::BufRead;
    let mut line = String::new();
    let stdin = std::io::stdin();
    if stdin.lock().read_line(&mut line).is_err() {
        return ConfirmDecision::Deny;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => ConfirmDecision::Proceed,
        _ => ConfirmDecision::Deny,
    }
}

/// Print the consent preview to stderr and resolve the gate.
///
/// Never prints the authorization URL — only the server, base URL, and the
/// exact scopes being requested, which is all a user needs to consent.
fn resolve_consent(preview: &AuthPreview, confirm: Confirm) -> ConfirmDecision {
    match confirm {
        Confirm::AutoProceed => ConfirmDecision::Proceed,
        Confirm::Callback(cb) => cb(preview),
        Confirm::Interactive => {
            eprintln!("animus mcp auth: about to authorize '{}' ({})", preview.server, preview.base_url);
            eprintln!("  requesting scopes: {}", scopes_display(&preview.requested_scopes));
            eprint!("Open browser to authorize? [y/N] ");
            use std::io::Write;
            let _ = std::io::stderr().flush();
            prompt_consent_stdin()
        }
    }
}

/// Run the interactive authorization-code flow for `server`.
///
/// Resolves the server URL (config or `url_override`), discovers the auth
/// server, resolves the least-privilege scope set, previews + confirms the
/// request, binds a loopback callback, performs Dynamic Client Registration
/// (or uses a pinned `client_id`), opens the browser, captures the redirect,
/// exchanges the code, and persists tokens in the keychain.
///
/// `scopes_override` (CLI `--scopes`) wins over the config's scope list. With
/// neither set, NO scopes are requested (server default applies). With
/// [`RunAuthOptions::dry_run`] set, returns [`AuthResult::DryRun`] after scope
/// resolution without opening a browser or obtaining any token.
pub async fn run_auth(project_root: &Path, server: &str, opts: RunAuthOptions<'_>) -> Result<AuthResult> {
    crate::ensure_crypto_provider();
    let RunAuthOptions { url_override, scopes_override, assume_yes, json, dry_run, confirm } = opts;
    let resolution = resolve_server_url(project_root, server, url_override)?;

    // Least-privilege scope resolution: explicit `--scopes`/config win; with
    // neither, request NOTHING and let the server apply its minimal default.
    let scopes = resolve_requested_scopes(scopes_override, &resolution.scopes);
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();

    // A blank/whitespace-only client_id is treated as unset (→ DCR) so a
    // config typo doesn't skip registration with an empty id; validation also
    // rejects it up front.
    let pinned_client_id = resolution.client_id.as_deref().map(str::trim).filter(|id| !id.is_empty());

    // Preview + confirm BEFORE any network side effect or browser open, so a
    // "no" aborts with ZERO side effects (no discovery, no callback bind, no
    // browser). Default is No. Skipped under `--yes`/`--json`. A dry run is a
    // read-only inspection and is never gated by consent — it stops before the
    // browser regardless.
    if !dry_run {
        let preview = AuthPreview {
            server: server.to_string(),
            base_url: resolution.url.clone(),
            requested_scopes: scopes.clone(),
        };
        let confirm = match confirm {
            Confirm::Interactive if assume_yes || json => Confirm::AutoProceed,
            other => other,
        };
        if resolve_consent(&preview, confirm) == ConfirmDecision::Deny {
            return Err(anyhow!("authorization for `{server}` cancelled before opening the browser"));
        }
    }

    // The `AuthorizationManager` base URL is BOTH the OAuth `resource`
    // indicator (RFC 8707) and the discovery seed in rmcp 1.7. It is the
    // protected MCP URL so the issued token's audience matches the server the
    // proxy will call; rmcp follows the protected-resource-metadata chain
    // (RFC 9728) to find the authorization server.
    //
    // Credential-store construction is deferred until AFTER the dry-run /
    // consent-deny returns: building it calls `scoped_state_root`, which
    // creates `~/.animus/<repo-scope>` + marker files on disk. A read-only
    // dry-run (or an aborted login) must not mutate scoped state.
    let mut manager = AuthorizationManager::new(&resolution.url)
        .await
        .map_err(|err| anyhow!("failed to initialize OAuth manager for `{server}`: {err}"))?;
    manager.set_state_store(InMemoryStateStore::new());

    let metadata = manager
        .discover_metadata()
        .await
        .map_err(|err| anyhow!("OAuth discovery failed for `{server}` at {}: {err}", resolution.url))?;
    manager.set_metadata(metadata);

    // `--dry-run`: discovery has now validated the endpoint + auth-server
    // metadata; report the resolved scopes and whether DCR would run WITHOUT
    // binding the callback, registering a client, opening a browser, or
    // exchanging any token. No credentials are obtained and the keychain-backed
    // secret store is never constructed (so no token-store write happens).
    if dry_run {
        return Ok(AuthResult::DryRun(DryRunOutcome {
            server: server.to_string(),
            base_url: resolution.url.clone(),
            requested_scopes: scopes.clone(),
            would_register_client: pinned_client_id.is_none(),
            authorized: false,
        }));
    }

    // Past the read-only / abort paths: now it is safe to materialize scoped
    // state and attach the keychain-backed credential store for token persist.
    let principal = resolve_principal_id(project_root);
    let secrets = build_secret_store(project_root)?;
    manager.set_credential_store(KeychainCredentialStore::new(secrets, server, &principal, &resolution.url));

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

    Ok(AuthResult::Completed(AuthOutcome {
        server: server.to_string(),
        principal,
        client_id,
        requested_scopes: scopes,
        granted_scopes,
        expires_at,
        has_refresh_token,
    }))
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
    fn no_scopes_configured_resolves_to_empty_least_privilege() {
        // The bug fix: with neither --scopes nor config scopes, request NOTHING
        // (server applies its default) instead of the full advertised set.
        let resolved = resolve_requested_scopes(None, &[]);
        assert!(resolved.is_empty(), "default scope set must be empty, got {resolved:?}");
    }

    #[test]
    fn config_scopes_used_when_no_override() {
        let config = vec!["read:account".to_string()];
        let resolved = resolve_requested_scopes(None, &config);
        assert_eq!(resolved, config);
    }

    #[test]
    fn explicit_scopes_win_over_config_and_are_requested_as_is() {
        let config = vec!["read:account".to_string(), "trade".to_string()];
        let override_scopes = vec!["a".to_string(), "b".to_string()];
        let resolved = resolve_requested_scopes(Some(&override_scopes), &config);
        assert_eq!(resolved, override_scopes);
    }

    #[test]
    fn empty_explicit_override_falls_back_to_config() {
        let config = vec!["read:account".to_string()];
        let resolved = resolve_requested_scopes(Some(&[]), &config);
        assert_eq!(resolved, config);
    }

    #[test]
    fn scopes_display_empty_is_minimal_label() {
        assert_eq!(scopes_display(&[]), "(none — server default / minimal)");
        assert_eq!(scopes_display(&["a".to_string(), "b".to_string()]), "a, b");
    }

    #[test]
    fn confirm_callback_deny_aborts() {
        let preview = AuthPreview {
            server: "robinhood-trading".to_string(),
            base_url: "https://agent.robinhood.com/mcp/trading".to_string(),
            requested_scopes: vec![],
        };
        let decision = resolve_consent(&preview, Confirm::Callback(Box::new(|_| ConfirmDecision::Deny)));
        assert_eq!(decision, ConfirmDecision::Deny);
    }

    #[test]
    fn confirm_callback_sees_resolved_scopes() {
        let preview = AuthPreview {
            server: "s".to_string(),
            base_url: "https://example.com".to_string(),
            requested_scopes: vec!["only-this".to_string()],
        };
        let decision = resolve_consent(
            &preview,
            Confirm::Callback(Box::new(|p| {
                assert_eq!(p.requested_scopes, vec!["only-this".to_string()]);
                ConfirmDecision::Proceed
            })),
        );
        assert_eq!(decision, ConfirmDecision::Proceed);
    }

    #[test]
    fn yes_or_json_resolves_interactive_to_auto_proceed() {
        // Mirrors the gate in `run_auth`: --yes OR --json turns an Interactive
        // confirm into AutoProceed (no stdin read), so scripts never block.
        for (assume_yes, json) in [(true, false), (false, true), (true, true)] {
            let resolved = match Confirm::Interactive {
                Confirm::Interactive if assume_yes || json => Confirm::AutoProceed,
                other => other,
            };
            assert!(matches!(resolved, Confirm::AutoProceed), "yes={assume_yes} json={json} should auto-proceed");
        }
        // Neither flag set leaves it Interactive (a real prompt).
        let (assume_yes, json) = (false, false);
        let resolved = match Confirm::Interactive {
            Confirm::Interactive if assume_yes || json => Confirm::AutoProceed,
            other => other,
        };
        assert!(matches!(resolved, Confirm::Interactive));
    }

    #[test]
    fn auto_proceed_skips_prompt() {
        let preview = AuthPreview {
            server: "s".to_string(),
            base_url: "https://example.com".to_string(),
            requested_scopes: vec![],
        };
        assert_eq!(resolve_consent(&preview, Confirm::AutoProceed), ConfirmDecision::Proceed);
    }

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
