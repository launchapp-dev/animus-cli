//! Interactive OAuth (authorization-code + PKCE) for protected MCP servers,
//! plus the [`proxy`] stdio bridge that serves agents an auth-free local MCP
//! endpoint while injecting live keychain-backed bearer tokens upstream.
//!
//! The machine-to-machine OAuth flows (`client_credentials`, `refresh_token`,
//! `manual_bearer`) continue to live in `animus_runtime_shared::oauth_broker`,
//! which injects an `Authorization` header at runtime-contract assembly. This
//! crate is the NEW interactive `authorization_code` flow: the daemon drives
//! discovery + Dynamic Client Registration + browser login + token exchange
//! via [`flow::run_auth`], tokens persist in the OS keychain via
//! [`keychain_store::KeychainCredentialStore`], and agents are repointed at
//! the [`proxy`] (`animus-mcp-proxy`) instead of receiving a bearer header.
//!
//! No OAuth, PKCE, or token-exchange code is hand-rolled here: rmcp 1.7's
//! `AuthorizationManager` / `AuthorizationSession` drive the protocol.

pub mod callback;
pub mod config;
pub mod flow;
pub mod keychain_store;
pub mod proxy;

pub use config::{resolve_principal_id, resolve_server_url, ServerResolution, ServerResolutionError};
pub use flow::{
    auth_logout, auth_status, run_auth, AuthOutcome, AuthPreview, AuthResult, AuthStatus, Confirm, ConfirmDecision,
    DryRunOutcome, RunAuthOptions, ServerAuthState,
};
pub use keychain_store::{derive_keychain_key, KeychainCredentialStore};

/// Install the process-default rustls crypto provider (aws-lc-rs) once.
///
/// rmcp 1.7's auth manager and the streamable-http client build their own
/// `reqwest` clients over rustls, which panics on the first HTTPS request
/// unless a process-default crypto provider is installed. This is a no-op if
/// a provider is already installed (e.g. by another component), so it is safe
/// to call from every entry point (`run_auth`, the proxy).
pub fn ensure_crypto_provider() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// Default client name advertised during Dynamic Client Registration.
pub const DEFAULT_CLIENT_NAME: &str = "Animus";

/// Loopback host the redirect-callback listener binds. Loopback only — the
/// authorization code never leaves the local machine.
pub const CALLBACK_HOST: &str = "127.0.0.1";

/// Maximum time (seconds) the interactive flow waits for the browser
/// redirect before giving up.
pub const CALLBACK_TIMEOUT_SECS: u64 = 300;
