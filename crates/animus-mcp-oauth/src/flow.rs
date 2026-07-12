//! Interactive `authorization_code` + PKCE flow, status, and logout.
//!
//! The flow drives rmcp 1.7's [`AuthorizationManager`] and
//! [`AuthorizationSession`] directly (rather than the higher-level
//! `OAuthState`) so the keychain-backed [`KeychainCredentialStore`] can be
//! injected explicitly — `OAuthState`'s constructors create their own internal
//! managers and don't expose store injection. The protocol itself (discovery,
//! DCR, PKCE, code exchange, refresh) is entirely rmcp's; this module only
//! orchestrates: discover -> resolve scopes (explicit, else auto-detect from
//! the server's advertised `scopes_supported`) -> preview/confirm -> (dry-run
//! stops here) -> attach credential store -> bind callback -> register/configure
//! -> open browser -> capture code -> exchange -> persist.
//!
//! Scope resolution runs AFTER discovery because the auto-detect default reads
//! the server's advertised `scopes_supported` from the discovery metadata,
//! which only exists post-discovery. Consent is therefore also gated AFTER
//! discovery, so the preview shows the user the ACTUAL scopes (including
//! auto-detected ones) before the browser opens. Discovery is a read-only GET
//! of public RFC 8414 / RFC 9728 metadata and consults no credentials, so
//! performing it before the consent gate is a benign side effect: a consent
//! "no" still touches no keychain and obtains no token.
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
use crate::config::{build_secret_store, resolve_principal_id, resolve_server_url, ServerResolutionError};
use crate::keychain_store::KeychainCredentialStore;
use crate::pending::{PendingAuth, PendingStore};
use crate::state_store::PersistentStateStore;
use crate::{CALLBACK_TIMEOUT_SECS, DEFAULT_CLIENT_NAME};

/// Successful interactive-auth outcome, returned to the CLI for display.
#[derive(Debug, Clone, Serialize)]
pub struct AuthOutcome {
    pub server: String,
    pub principal: String,
    pub client_id: String,
    /// Scopes the flow asked the authorization server for. Resolution order:
    /// explicit `--scopes` > config `scopes:` > the server's advertised
    /// `scopes_supported` (auto-detected). Empty only when the server
    /// advertised no scopes and none were configured. Surfaced so a caller can
    /// audit the request breadth.
    pub requested_scopes: Vec<String>,
    /// True when `requested_scopes` were auto-detected from the server's
    /// advertised `scopes_supported` (neither `--scopes` nor config set them).
    pub scopes_auto_detected: bool,
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
    /// True when `requested_scopes` were auto-detected from the server's
    /// advertised `scopes_supported`. The preview marks them as such so the
    /// user knows ALL advertised scopes were adopted and can narrow with
    /// `--scopes`.
    pub scopes_auto_detected: bool,
}

/// Result of `animus mcp auth <server> --dry-run`: discovery + scope
/// resolution without opening a browser, binding the callback, or exchanging
/// any token. No credentials are obtained.
#[derive(Debug, Clone, Serialize)]
pub struct DryRunOutcome {
    pub server: String,
    pub base_url: String,
    pub requested_scopes: Vec<String>,
    /// True when `requested_scopes` were auto-detected from the server's
    /// advertised `scopes_supported` (neither `--scopes` nor config set them).
    pub scopes_auto_detected: bool,
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

/// The resolved scope request plus how it was determined.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedScopes {
    scopes: Vec<String>,
    /// True when `scopes` came from the server's advertised `scopes_supported`
    /// (neither `--scopes` nor config `scopes:` was set).
    auto_detected: bool,
}

/// The sentinel value (case-insensitive) for `--scopes` / config `scopes:` that
/// forces an EMPTY scope request, opting out of auto-detection so the
/// authorization server applies its own minimal default. Needed because an
/// empty/omitted scope list is indistinguishable from "not configured" (and
/// `OauthConfig.scopes` defaults to an empty `Vec`), so there would otherwise be
/// no way to request zero scopes against a server that advertises an over-broad
/// optional set.
const SCOPES_NONE_SENTINEL: &str = "none";

/// True when a scope list is the explicit "request nothing" sentinel: exactly
/// one entry equal (case-insensitive) to [`SCOPES_NONE_SENTINEL`].
fn is_none_sentinel(scopes: &[String]) -> bool {
    scopes.len() == 1 && scopes[0].eq_ignore_ascii_case(SCOPES_NONE_SENTINEL)
}

/// Resolve the scopes to request.
///
/// Precedence:
/// 1. CLI `--scopes` (`scopes_override`) — explicit, wins.
/// 2. config `scopes:` — explicit.
/// 3. the server's advertised `scopes_supported` from discovery metadata
///    (`advertised_scopes`) — auto-detected, when neither explicit source set.
/// 4. EMPTY when the server advertised no scopes either (server applies its own
///    minimal default).
///
/// Opt-out: an explicit `--scopes none` (or config `scopes: [none]`),
/// case-insensitive, forces an EMPTY request — skipping auto-detection — for a
/// server that advertises broad optional scopes but accepts a bare (no-`scope`)
/// authorize. This is the explicit way to keep the pre-auto-detect
/// server-default behavior. The sentinel only matters at the highest source
/// that sets it (override beats config), and is treated as "explicitly empty"
/// (NOT auto-detected); it never falls through to the next tier.
///
/// Auto-detection (tier 3) exists because some authorization servers (e.g.
/// Robinhood's MCP) REQUIRE a specific scope and FAIL the authorize step when
/// an empty scope is requested. Their advertised `scopes_supported` names the
/// scope(s) the server expects, so adopting the advertised set makes those
/// servers work out of the box. Auto-detected results are clearly marked in the
/// consent preview / dry-run / `--json` output so the user knows all advertised
/// scopes were adopted and can narrow with `--scopes`. Explicit `--scopes` or
/// config `scopes:` always override the advertised set.
fn resolve_requested_scopes(
    scopes_override: Option<&[String]>,
    config_scopes: &[String],
    advertised_scopes: &[String],
) -> ResolvedScopes {
    // A non-empty `--scopes` override wins. The `none` sentinel forces empty
    // (explicit opt-out from auto-detection), otherwise the listed scopes are
    // requested verbatim.
    if let Some(s) = scopes_override {
        if !s.is_empty() {
            let scopes = if is_none_sentinel(s) { Vec::new() } else { s.to_vec() };
            return ResolvedScopes { scopes, auto_detected: false };
        }
    }
    // Config scopes are the next explicit source, with the same `none` opt-out.
    if !config_scopes.is_empty() {
        let scopes = if is_none_sentinel(config_scopes) { Vec::new() } else { config_scopes.to_vec() };
        return ResolvedScopes { scopes, auto_detected: false };
    }
    // Neither explicit source set: auto-detect the advertised set if any.
    if !advertised_scopes.is_empty() {
        return ResolvedScopes { scopes: advertised_scopes.to_vec(), auto_detected: true };
    }
    // Nothing advertised either: request none (server applies its default).
    ResolvedScopes { scopes: Vec::new(), auto_detected: false }
}

/// Render the requested scopes for the consent preview / dry-run output.
///
/// When `auto_detected` is set, the scopes were adopted from the server's
/// advertised `scopes_supported` (not explicitly requested), so the rendering
/// marks them as such for transparency.
fn scopes_display(scopes: &[String], auto_detected: bool) -> String {
    if scopes.is_empty() {
        "(none — server default / minimal)".to_string()
    } else if auto_detected {
        format!("{} (auto-detected from server metadata)", scopes.join(", "))
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
            eprintln!(
                "  requesting scopes: {}",
                scopes_display(&preview.requested_scopes, preview.scopes_auto_detected)
            );
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
/// server, resolves the requested scope set, previews + confirms the request,
/// binds a loopback callback, performs Dynamic Client Registration (or uses a
/// pinned `client_id`), opens the browser, captures the redirect, exchanges the
/// code, and persists tokens in the keychain.
///
/// Scope precedence: `scopes_override` (CLI `--scopes`) > config `scopes:` >
/// the server's advertised `scopes_supported` (auto-detected from discovery
/// metadata) > empty (server default). Because the auto-detect tier reads
/// discovery metadata, scope resolution AND the consent gate both run AFTER
/// discovery, so the preview shows the user the actual (possibly auto-detected)
/// scopes before the browser opens. Discovery is a read-only public-metadata
/// GET, so a consent "no" still obtains no token and touches no keychain. With
/// [`RunAuthOptions::dry_run`] set, returns [`AuthResult::DryRun`] after scope
/// resolution without opening a browser or obtaining any token.
pub async fn run_auth(project_root: &Path, server: &str, opts: RunAuthOptions<'_>) -> Result<AuthResult> {
    crate::ensure_crypto_provider();
    let RunAuthOptions { url_override, scopes_override, assume_yes, json, dry_run, confirm } = opts;
    let resolution = resolve_server_url(project_root, server, url_override)?;

    // A blank/whitespace-only client_id is treated as unset (→ DCR) so a
    // config typo doesn't skip registration with an empty id; validation also
    // rejects it up front.
    let pinned_client_id = resolution.client_id.as_deref().map(str::trim).filter(|id| !id.is_empty());

    // The `AuthorizationManager` base URL is BOTH the OAuth `resource`
    // indicator (RFC 8707) and the discovery seed in rmcp 1.7. It is the
    // protected MCP URL so the issued token's audience matches the server the
    // proxy will call; rmcp follows the protected-resource-metadata chain
    // (RFC 9728) to find the authorization server.
    //
    // Discovery runs BEFORE scope resolution and the consent gate: the
    // auto-detect default reads the server's advertised `scopes_supported`,
    // which only exists post-discovery, and consent must preview the ACTUAL
    // (possibly auto-detected) scopes. Discovery is a read-only GET of public
    // RFC 8414 / RFC 9728 metadata and consults no credentials, so a consent
    // "no" below still touches no keychain and obtains no token. The
    // keychain-backed credential store is still constructed only AFTER the
    // dry-run / consent-deny returns — building it calls `scoped_state_root`,
    // which creates `~/.animus/<repo-scope>` + marker files; a read-only
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

    // The advertised scope set for auto-detection. `select_scopes(None, &[])`
    // returns rmcp's highest-priority advertised set (SEP-835 order): the
    // `WWW-Authenticate` `scope` from the initial 401 if any, else RFC 9728
    // protected-resource-metadata `scopes_supported`, else RFC 8414
    // authorization-server-metadata `scopes_supported` (plus `offline_access`
    // when the AS advertises it). Passing `None` + EMPTY defaults means it never
    // substitutes a caller fallback, so an empty return genuinely means the
    // server advertised nothing. We read it via `select_scopes` rather than
    // `metadata.scopes_supported` directly because the protected-resource (RFC
    // 9728) and `WWW-Authenticate` scopes are populated during discovery into
    // PRIVATE manager fields with no public accessor — reading only the AS
    // metadata field would miss servers that advertise their required scope
    // solely via protected-resource metadata. Must run AFTER `set_metadata` so
    // the AS-metadata tier is visible.
    let advertised_scopes = manager.select_scopes(None, &[]);

    // Scope resolution (post-discovery): explicit `--scopes`/config win; with
    // neither, auto-detect from the server's advertised scopes; with nothing
    // advertised either, request NOTHING (server default).
    let ResolvedScopes { scopes, auto_detected: scopes_auto_detected } =
        resolve_requested_scopes(scopes_override, &resolution.scopes, &advertised_scopes);
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();

    // `--dry-run`: discovery has now validated the endpoint + auth-server
    // metadata and resolved the (possibly auto-detected) scopes; report them
    // and whether DCR would run WITHOUT binding the callback, registering a
    // client, opening a browser, or exchanging any token. No credentials are
    // obtained and the keychain-backed secret store is never constructed (so no
    // token-store write happens).
    if dry_run {
        return Ok(AuthResult::DryRun(DryRunOutcome {
            server: server.to_string(),
            base_url: resolution.url.clone(),
            requested_scopes: scopes.clone(),
            scopes_auto_detected,
            would_register_client: pinned_client_id.is_none(),
            authorized: false,
        }));
    }

    // Preview + confirm before opening the browser. The preview shows the
    // resolved (possibly auto-detected) scopes so a "no" aborts before the
    // callback bind / browser open with no token obtained. Default is No.
    // Skipped under `--yes`/`--json`.
    {
        let preview = AuthPreview {
            server: server.to_string(),
            base_url: resolution.url.clone(),
            requested_scopes: scopes.clone(),
            scopes_auto_detected,
        };
        let confirm = match confirm {
            Confirm::Interactive if assume_yes || json => Confirm::AutoProceed,
            other => other,
        };
        if resolve_consent(&preview, confirm) == ConfirmDecision::Deny {
            return Err(anyhow!("authorization for `{server}` cancelled before opening the browser"));
        }
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
        let mut config =
            OAuthClientConfig::new(client_id.to_string(), redirect_uri.clone()).with_scopes(scopes.clone());
        // Confidential pre-registered app: attach the resolved client_secret so
        // the token exchange authenticates as that app (public/PKCE clients
        // leave this unset).
        if let Some(secret) = resolution.client_secret.as_deref() {
            config = config.with_client_secret(secret.to_string());
        }
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
        scopes_auto_detected,
        granted_scopes,
        expires_at,
        has_refresh_token,
    }))
}

/// Options for [`begin_auth`] — the first half of the delegated (headless/web)
/// flow. Unlike [`run_auth`], the caller (a remote host such as the portal)
/// supplies the `redirect_uri` its own public callback will receive, and there
/// is no interactive consent here: the host is responsible for gating who may
/// start a connect, and discovery is a read-only public-metadata GET.
pub struct BeginOptions<'a> {
    pub url_override: Option<&'a str>,
    pub scopes_override: Option<&'a [String]>,
    /// The caller's public callback URL (e.g. `https://portal/api/mcp-oauth/callback`).
    /// Registered as the OAuth redirect_uri and required verbatim at exchange.
    pub redirect_uri: String,
}

/// Result of [`begin_auth`]: the URL to send the user's browser to, plus the
/// CSRF `state` that [`complete_auth`] must be called back with. Carries NO PKCE
/// material or token — those stay in the keychain-backed stores.
#[derive(Debug, Clone, Serialize)]
pub struct BeginOutcome {
    pub authorize_url: String,
    pub state: String,
    pub would_register_client: bool,
    pub requested_scopes: Vec<String>,
    pub scopes_auto_detected: bool,
}

/// Options for [`complete_auth`] — the second half of the delegated flow, run in
/// a FRESH process after the user's browser hits the caller's callback. Only the
/// callback's `code` + `state` are needed: the server URL and all other exchange
/// parameters are read back from the pending record located by `state`.
pub struct CompleteOptions {
    pub code: String,
    pub state: String,
}

/// Begin a delegated authorization: resolve + discover + register/configure a
/// (public) client + mint the authorization URL, persisting the PKCE state
/// ([`PersistentStateStore`]) and the non-secret exchange parameters
/// ([`PendingAuth`]) keyed by the CSRF `state` so a separate
/// [`complete_auth`] process can finish the exchange.
///
/// Does NOT bind a loopback listener, open a browser, or exchange any token —
/// the caller drives the browser to `authorize_url` and later calls
/// [`complete_auth`] with the `code`/`state` from its callback. The laptop
/// loopback flow ([`run_auth`]) is unchanged; this shares the same config
/// resolution + keychain stores.
pub async fn begin_auth(project_root: &Path, server: &str, opts: BeginOptions<'_>) -> Result<BeginOutcome> {
    crate::ensure_crypto_provider();
    let BeginOptions { url_override, scopes_override, redirect_uri } = opts;

    let resolution = resolve_server_url(project_root, server, url_override)?;
    let pinned_client_id = resolution.client_id.as_deref().map(str::trim).filter(|id| !id.is_empty());
    let principal = resolve_principal_id(project_root);
    let secrets = build_secret_store(project_root)?;

    // PersistentStateStore (NOT InMemory): the PKCE verifier minted below must
    // survive into the separate `complete_auth` process.
    let mut manager = AuthorizationManager::new(&resolution.url)
        .await
        .map_err(|err| anyhow!("failed to initialize OAuth manager for `{server}`: {err}"))?;
    manager.set_state_store(PersistentStateStore::new(secrets.clone(), server, &principal, &resolution.url));

    let metadata = manager
        .discover_metadata()
        .await
        .map_err(|err| anyhow!("OAuth discovery failed for `{server}` at {}: {err}", resolution.url))?;
    manager.set_metadata(metadata);

    let advertised_scopes = manager.select_scopes(None, &[]);
    let ResolvedScopes { scopes, auto_detected: scopes_auto_detected } =
        resolve_requested_scopes(scopes_override, &resolution.scopes, &advertised_scopes);
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();

    // Attach the keychain credential store so the eventual token bundle (written
    // by `complete_auth`) lands in the same entry the proxy reads.
    manager.set_credential_store(KeychainCredentialStore::new(secrets.clone(), server, &principal, &resolution.url));

    // Resolve the client. Both branches end with a configured PUBLIC client
    // whose id (and any DCR-issued secret) we persist; `complete_auth`
    // re-configures the same client on a fresh manager (no AuthorizationSession —
    // its in-process state can't cross the boundary). DCR usually registers a
    // PUBLIC client (`token_endpoint_auth_method: "none"`), but a server may
    // still return a confidential client; carry the secret so completion can
    // authenticate the exchange with the same client begin registered.
    let (client_id, client_secret) = if let Some(id) = pinned_client_id {
        let mut config = OAuthClientConfig::new(id.to_string(), redirect_uri.clone()).with_scopes(scopes.clone());
        // Confidential pre-registered app: carry the resolved client_secret into
        // the pending record so `complete_auth` authenticates the exchange as the
        // same app. Public/PKCE pinned clients leave it `None`.
        let pinned_secret = resolution.client_secret.clone();
        if let Some(secret) = pinned_secret.as_deref() {
            config = config.with_client_secret(secret.to_string());
        }
        manager
            .configure_client(config)
            .map_err(|err| anyhow!("failed to configure pinned client_id for `{server}`: {err}"))?;
        (id.to_string(), pinned_secret)
    } else {
        // register_client runs DCR and internally calls configure_client.
        let config = manager
            .register_client(DEFAULT_CLIENT_NAME, &redirect_uri, &scope_refs)
            .await
            .map_err(|err| anyhow!("dynamic client registration failed for `{server}`: {err}"))?;
        (config.client_id, config.client_secret)
    };

    // Mint the authorization URL — this writes the PKCE verifier + CSRF into the
    // PersistentStateStore keyed by the state param.
    let auth_url = manager
        .get_authorization_url(&scope_refs)
        .await
        .map_err(|err| anyhow!("failed to build authorization URL for `{server}`: {err}"))?;
    let state =
        extract_state_param(&auth_url).ok_or_else(|| anyhow!("authorization URL is missing the `state` parameter"))?;

    // Persist the non-secret exchange parameters keyed by the same state.
    let pending_store = PendingStore::new(secrets, server, &principal);
    pending_store.save(
        &state,
        PendingAuth {
            server: server.to_string(),
            url: resolution.url.clone(),
            scopes: scopes.clone(),
            scopes_auto_detected,
            principal: principal.clone(),
            redirect_uri,
            client_id,
            client_secret,
            created_at: 0, // stamped by save()
        },
    )?;

    Ok(BeginOutcome {
        authorize_url: auth_url,
        state,
        would_register_client: pinned_client_id.is_none(),
        requested_scopes: scopes,
        scopes_auto_detected,
    })
}

/// Complete a delegated authorization started by [`begin_auth`]: rebuild the
/// same `AuthorizationManager` (same [`PersistentStateStore`] + keychain
/// credential store), re-configure the public client from the persisted
/// [`PendingAuth`], and exchange `code`/`state` for a token — the exchange reads
/// the PKCE verifier back from the persistent state store. On success the token
/// bundle is written to the keychain entry the proxy reads, and the transient
/// pending + state records are deleted.
pub async fn complete_auth(project_root: &Path, server: &str, opts: CompleteOptions) -> Result<AuthOutcome> {
    crate::ensure_crypto_provider();
    let CompleteOptions { code, state } = opts;

    let principal = resolve_principal_id(project_root);
    let secrets = build_secret_store(project_root)?;

    // The pending record is keyed by `(server, principal, state)` and is the
    // source of truth for the URL + exchange parameters — so completion needs
    // only the callback's `state`, never a re-supplied `--url`.
    let pending_store = PendingStore::new(secrets.clone(), server, &principal);
    let pending = pending_store.load(&state)?.ok_or_else(|| {
        anyhow!("no pending authorization for `{server}` (expired or never begun); start over with `animus mcp auth {server} --print-url ...`")
    })?;

    let mut manager = AuthorizationManager::new(&pending.url)
        .await
        .map_err(|err| anyhow!("failed to initialize OAuth manager for `{server}`: {err}"))?;
    manager.set_state_store(PersistentStateStore::new(secrets.clone(), server, &principal, &pending.url));

    let metadata = manager
        .discover_metadata()
        .await
        .map_err(|err| anyhow!("OAuth discovery failed for `{server}` at {}: {err}", pending.url))?;
    manager.set_metadata(metadata);
    manager.set_credential_store(KeychainCredentialStore::new(secrets, server, &principal, &pending.url));

    // Re-configure the SAME public client `begin` resolved/registered. No DCR
    // here — re-registering would mint a different client than the PKCE state
    // was bound to.
    let mut config = OAuthClientConfig::new(pending.client_id.clone(), pending.redirect_uri.clone())
        .with_scopes(pending.scopes.clone());
    if let Some(secret) = pending.client_secret.clone() {
        config = config.with_client_secret(secret);
    }
    manager.configure_client(config).map_err(|err| anyhow!("failed to configure client for `{server}`: {err}"))?;

    let token = manager
        .exchange_code_for_token(&code, &state)
        .await
        .map_err(|err| anyhow!("token exchange failed for `{server}`: {err}"))?;
    let (client_id, _) = manager
        .get_credentials()
        .await
        .map_err(|err| anyhow!("failed to read back stored credentials for `{server}`: {err}"))?;

    let token_value = serde_json::to_value(&token).unwrap_or_default();
    let granted_scopes = scopes_from_token_value(&token_value).unwrap_or_else(|| pending.scopes.clone());
    let has_refresh_token = token_value.get("refresh_token").and_then(|v| v.as_str()).is_some();
    let expires_at = expires_at_from_token_value(&token_value, Utc::now());

    // Best-effort cleanup of the pending record on success. The PKCE state
    // entry is left to its TTL sweep (see PersistentStateStore) — rmcp's
    // exchange consumes it, and re-deleting would need the StateStore trait in
    // scope for no functional gain.
    let _ = pending_store.delete(&state);

    Ok(AuthOutcome {
        server: server.to_string(),
        principal,
        client_id,
        requested_scopes: pending.scopes,
        scopes_auto_detected: pending.scopes_auto_detected,
        granted_scopes,
        expires_at,
        has_refresh_token,
    })
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
        // Non-swallowing enumeration: a transient config_source failure surfaces
        // as an Err here rather than an empty `servers:[]` that would read as
        // "no OAuth servers configured".
        None => authorization_code_servers(project_root)?,
    };

    let mut out = Vec::with_capacity(servers.len());
    for name in servers {
        // Tokens are keyed by the upstream URL too, so resolve it. A server
        // that can't be resolved (e.g. dropped from config and no --url) is
        // reported as unauthenticated rather than failing the whole report —
        // EXCEPT a transient config_source failure, which must surface (not be
        // silently reported as an unauthenticated server) so the user retries.
        let url = match resolve_server_url(project_root, &name, url_override) {
            Ok(resolution) => resolution.url,
            Err(err @ ServerResolutionError::ConfigSourceUnavailable(..)) => return Err(err.into()),
            Err(_) => {
                out.push(server_state_from_creds(&name, &principal, None));
                continue;
            }
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
///
/// Uses the NON-SWALLOWING loader: a transient `config_source` failure returns
/// `Err` so `auth_status` reports a transient error rather than an empty
/// `servers:[]` (which would read as "no OAuth servers configured"). A genuinely
/// absent config source (`NoSource`) is benign — enumeration proceeds from
/// project config only.
fn authorization_code_servers(project_root: &Path) -> Result<Vec<String>> {
    use orchestrator_config::workflow_config::{try_load_workflow_config, OauthFlow, WorkflowConfigAvailability};
    let mut names = std::collections::BTreeSet::new();

    match try_load_workflow_config(project_root, None) {
        WorkflowConfigAvailability::Loaded(loaded) => {
            for (name, def) in &loaded.config.mcp_servers {
                if def.oauth.as_ref().is_some_and(|o| o.flow == OauthFlow::AuthorizationCode) {
                    names.insert(name.clone());
                }
            }
        }
        WorkflowConfigAvailability::NoSource => {}
        WorkflowConfigAvailability::SourceUnavailable(err) => {
            return Err(anyhow!("workflow config source unavailable while listing OAuth servers: {err}"));
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

    Ok(names.into_iter().collect())
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
    fn no_scopes_anywhere_resolves_to_empty_least_privilege() {
        // Neither --scopes, config, nor advertised → request NOTHING (server
        // applies its own minimal default).
        let resolved = resolve_requested_scopes(None, &[], &[]);
        assert!(resolved.scopes.is_empty(), "default scope set must be empty, got {resolved:?}");
        assert!(!resolved.auto_detected);
    }

    #[test]
    fn advertised_scopes_auto_detected_when_no_explicit_source() {
        // The fix: with neither --scopes nor config but the server advertises
        // `scopes_supported`, adopt the advertised set and mark it auto-detected.
        let advertised = vec!["internal".to_string()];
        let resolved = resolve_requested_scopes(None, &[], &advertised);
        assert_eq!(resolved.scopes, advertised);
        assert!(resolved.auto_detected, "advertised scopes must be flagged auto-detected");
    }

    #[test]
    fn config_scopes_used_when_no_override_and_override_advertised() {
        let config = vec!["read:account".to_string()];
        let advertised = vec!["all_accounts".to_string(), "trade".to_string()];
        let resolved = resolve_requested_scopes(None, &config, &advertised);
        assert_eq!(resolved.scopes, config, "config must override the advertised set");
        assert!(!resolved.auto_detected, "config scopes are explicit, not auto-detected");
    }

    #[test]
    fn explicit_scopes_win_over_config_and_advertised() {
        let config = vec!["read:account".to_string(), "trade".to_string()];
        let advertised = vec!["all_accounts".to_string()];
        let override_scopes = vec!["a".to_string(), "b".to_string()];
        let resolved = resolve_requested_scopes(Some(&override_scopes), &config, &advertised);
        assert_eq!(resolved.scopes, override_scopes);
        assert!(!resolved.auto_detected, "explicit --scopes are not auto-detected");
    }

    #[test]
    fn empty_explicit_override_falls_back_to_config() {
        let config = vec!["read:account".to_string()];
        let resolved = resolve_requested_scopes(Some(&[]), &config, &[]);
        assert_eq!(resolved.scopes, config);
        assert!(!resolved.auto_detected);
    }

    #[test]
    fn empty_override_and_config_falls_back_to_advertised() {
        let advertised = vec!["internal".to_string()];
        let resolved = resolve_requested_scopes(Some(&[]), &[], &advertised);
        assert_eq!(resolved.scopes, advertised);
        assert!(resolved.auto_detected);
    }

    #[test]
    fn scopes_none_sentinel_override_forces_empty_skipping_auto_detect() {
        // `--scopes none` opts out: request NOTHING even though the server
        // advertises scopes. Not flagged auto-detected.
        let advertised = vec!["all_accounts".to_string()];
        for sentinel in [vec!["none".to_string()], vec!["NONE".to_string()], vec!["None".to_string()]] {
            let resolved = resolve_requested_scopes(Some(&sentinel), &[], &advertised);
            assert!(resolved.scopes.is_empty(), "`--scopes {sentinel:?}` must force empty");
            assert!(!resolved.auto_detected);
        }
    }

    #[test]
    fn scopes_none_sentinel_in_config_forces_empty() {
        let advertised = vec!["all_accounts".to_string()];
        let config = vec!["none".to_string()];
        let resolved = resolve_requested_scopes(None, &config, &advertised);
        assert!(resolved.scopes.is_empty(), "config `scopes: [none]` must force empty");
        assert!(!resolved.auto_detected);
    }

    #[test]
    fn scopes_none_only_triggers_as_sole_entry() {
        // "none" alongside real scopes is treated as a literal scope name, not
        // the opt-out sentinel.
        let scopes = vec!["none".to_string(), "trade".to_string()];
        let resolved = resolve_requested_scopes(Some(&scopes), &[], &["adv".to_string()]);
        assert_eq!(resolved.scopes, scopes);
        assert!(!resolved.auto_detected);
    }

    #[test]
    fn override_none_sentinel_wins_over_config() {
        // `--scopes none` opts out even when config sets real scopes.
        let resolved =
            resolve_requested_scopes(Some(&["none".to_string()]), &["read:account".to_string()], &["adv".to_string()]);
        assert!(resolved.scopes.is_empty());
        assert!(!resolved.auto_detected);
    }

    #[test]
    fn scopes_display_empty_is_minimal_label() {
        assert_eq!(scopes_display(&[], false), "(none — server default / minimal)");
        assert_eq!(scopes_display(&["a".to_string(), "b".to_string()], false), "a, b");
    }

    #[test]
    fn scopes_display_marks_auto_detected() {
        assert_eq!(scopes_display(&["internal".to_string()], true), "internal (auto-detected from server metadata)");
        // Empty advertised still renders the minimal label even if flagged.
        assert_eq!(scopes_display(&[], true), "(none — server default / minimal)");
    }

    #[test]
    fn confirm_callback_deny_aborts() {
        let preview = AuthPreview {
            server: "robinhood-trading".to_string(),
            base_url: "https://agent.robinhood.com/mcp/trading".to_string(),
            requested_scopes: vec![],
            scopes_auto_detected: false,
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
            scopes_auto_detected: false,
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
            scopes_auto_detected: false,
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
