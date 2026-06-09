//! Keychain-backed [`rmcp::transport::auth::CredentialStore`].
//!
//! rmcp 1.7's `AuthorizationManager` persists `StoredCredentials` (the
//! access/refresh token bundle) through a pluggable `CredentialStore`
//! trait. This module backs that trait with the v0.5.8 OS keychain
//! ([`orchestrator_core::SecretStore`]) so OAuth tokens land in the same
//! secure store as the rest of Animus's project secrets — no bespoke
//! token files.
//!
//! # Key derivation
//!
//! Conceptually each authed server is keyed `mcp-oauth:<server>:<principal>`.
//! The keychain KEY alphabet (`[A-Za-z_][A-Za-z0-9_]*`, enforced by
//! [`orchestrator_core::secret_store::validate_key`]) forbids `:` and `-`,
//! so the logical key is mapped to a validated KEY of the shape
//! `MCP_OAUTH__<sanitized>__<hash>` where `<sanitized>` keeps the
//! human-readable server/principal and `<hash>` is a SHA-256 prefix of the
//! raw `server\x00principal` pair so two inputs that sanitize to the same
//! prefix never collide. The value is the JSON-serialized
//! `StoredCredentials`.

use std::sync::Arc;

use async_trait::async_trait;
use orchestrator_core::SecretStore;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use sha2::{Digest, Sha256};

/// Prefix applied to every derived keychain KEY so OAuth token entries are
/// visually distinct from ordinary project secrets in the index.
const KEY_PREFIX: &str = "MCP_OAUTH__";

/// Cap on the sanitized human-readable portion of the derived KEY so an
/// unusually long server or principal name cannot blow past keychain KEY
/// length limits.
const SANITIZED_CAP: usize = 80;

/// Derive a [`validate_key`](orchestrator_core::secret_store::validate_key)-safe
/// keychain KEY for the logical `mcp-oauth:<server>:<principal>` entry,
/// **bound to the upstream `url`**.
///
/// The KEY is `MCP_OAUTH__<sanitized>__<hash16>` where `<sanitized>` is the
/// `server` and `principal` joined by `_`, with every character outside
/// `[A-Za-z0-9_]` replaced by `_`, and `<hash16>` is a 16-hex-char SHA-256
/// prefix of the raw `server\x00principal\x00url` triple.
///
/// Binding the URL into the hash is a security control: if a server name is
/// reused but repointed at a different upstream (a workflow override or an
/// untrusted branch swapping `github`'s host), the derived key changes, so the
/// existing bearer — minted for the *original* host — is simply not found.
/// Resolution fails closed (forcing a fresh `animus mcp auth`) rather than
/// transmitting a token to a host it was never issued for.
#[must_use]
pub fn derive_keychain_key(server: &str, principal: &str, url: &str) -> String {
    let mut sanitized = String::with_capacity(server.len() + principal.len() + 1);
    let push_sanitized = |s: &str, out: &mut String| {
        for ch in s.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
    };
    push_sanitized(server, &mut sanitized);
    sanitized.push('_');
    push_sanitized(principal, &mut sanitized);
    sanitized.truncate(SANITIZED_CAP);

    let mut hasher = Sha256::new();
    hasher.update(server.as_bytes());
    hasher.update([0u8]);
    hasher.update(principal.as_bytes());
    hasher.update([0u8]);
    hasher.update(url.as_bytes());
    let digest = hasher.finalize();
    let mut hash_hex = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(hash_hex, "{byte:02x}");
    }

    format!("{KEY_PREFIX}{sanitized}__{hash_hex}")
}

/// rmcp [`CredentialStore`] persisting `StoredCredentials` JSON into the OS
/// keychain under a single derived KEY.
///
/// The store is bound to one `(server, principal, url)` triple at construction
/// so the rmcp `AuthorizationManager` (which calls `load`/`save`/`clear` with
/// no arguments) reads and writes exactly that server+endpoint's tokens. The
/// URL binding prevents a token minted for one upstream from being reused when
/// the same server name is repointed at a different host.
pub struct KeychainCredentialStore {
    secrets: Arc<dyn SecretStore>,
    key: String,
}

impl KeychainCredentialStore {
    /// Bind a store to `(server, principal, url)` over the given keychain.
    #[must_use]
    pub fn new(secrets: Arc<dyn SecretStore>, server: &str, principal: &str, url: &str) -> Self {
        Self { secrets, key: derive_keychain_key(server, principal, url) }
    }

    /// The derived keychain KEY this store reads/writes. Exposed for
    /// status/logout surfaces that operate by KEY.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
}

#[async_trait]
impl CredentialStore for KeychainCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let raw = self
            .secrets
            .get(&self.key)
            .map_err(|err| AuthError::InternalError(format!("keychain read failed: {err}")))?;
        match raw {
            Some(json) => {
                let creds: StoredCredentials = serde_json::from_str(&json)
                    .map_err(|err| AuthError::InternalError(format!("stored credential parse failed: {err}")))?;
                Ok(Some(creds))
            }
            None => Ok(None),
        }
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let json = serde_json::to_string(&credentials)
            .map_err(|err| AuthError::InternalError(format!("stored credential serialize failed: {err}")))?;
        self.secrets
            .set(&self.key, &json)
            .map_err(|err| AuthError::InternalError(format!("keychain write failed: {err}")))?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.secrets
            .delete(&self.key)
            .map_err(|err| AuthError::InternalError(format!("keychain delete failed: {err}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::secret_store::{validate_key, MockSecretStore};
    use rmcp::transport::auth::OAuthTokenResponse;
    use serde_json::Value;

    #[test]
    fn derived_key_is_validate_key_safe() {
        for (server, principal) in [
            ("github", "default"),
            ("api.githubcopilot.com/mcp/", "user@example.com"),
            ("../../escape", "p:rin-cipal"),
            ("", ""),
            ("animus.requirements/ao", "sami"),
        ] {
            let key = derive_keychain_key(server, principal, "https://api.example.com/mcp/");
            validate_key(&key).unwrap_or_else(|e| panic!("derived key {key:?} invalid: {e}"));
        }
    }

    #[test]
    fn derived_key_breaks_sanitization_collisions() {
        // Two distinct raw pairs that sanitize to the same prefix must
        // still produce distinct keys (the hash suffix differs).
        let a = derive_keychain_key("foo/bar", "p", "https://h/mcp");
        let b = derive_keychain_key("foo_bar", "p", "https://h/mcp");
        assert_ne!(a, b, "sanitization collision must be broken by the hash suffix");
    }

    #[test]
    fn derived_key_is_stable() {
        assert_eq!(
            derive_keychain_key("github", "default", "https://h/mcp"),
            derive_keychain_key("github", "default", "https://h/mcp")
        );
    }

    #[test]
    fn derived_key_is_bound_to_url() {
        // Security control: the same (server, principal) with a different URL
        // must produce a different key, so a token minted for one upstream is
        // never found (and thus never sent) when the server is repointed.
        let a = derive_keychain_key("github", "default", "https://api.githubcopilot.com/mcp/");
        let b = derive_keychain_key("github", "default", "https://evil.example.com/mcp/");
        assert_ne!(a, b, "different upstream URL must produce a different keychain key");
    }

    fn sample_credentials() -> StoredCredentials {
        // Build a minimal OAuthTokenResponse via JSON so we don't depend on
        // oauth2 constructors. access_token + token_type are the required
        // standard fields.
        let token_json = serde_json::json!({
            "access_token": "at-12345",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "rt-67890",
            "scope": "repo read:user"
        });
        let token_response: OAuthTokenResponse =
            serde_json::from_value(token_json).expect("token response deserializes");
        StoredCredentials::new(
            "client-abc".to_string(),
            Some(token_response),
            vec!["repo".to_string(), "read:user".to_string()],
            Some(1_700_000_000),
        )
    }

    #[tokio::test]
    async fn round_trips_stored_credentials_through_mock_keychain() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MockSecretStore::new());
        let store = KeychainCredentialStore::new(secrets, "github", "default", "https://api.githubcopilot.com/mcp/");

        assert!(store.load().await.unwrap().is_none(), "empty store loads None");

        store.save(sample_credentials()).await.unwrap();
        let loaded = store.load().await.unwrap().expect("credentials persisted");
        assert_eq!(loaded.client_id, "client-abc");
        let token = loaded.token_response.expect("token response round-trips");
        // Re-serialize the token bundle to JSON and assert the access token
        // survived the keychain round-trip, without depending on oauth2's
        // TokenResponse accessor trait directly.
        let token_value: Value = serde_json::to_value(&token).expect("token re-serializes");
        assert_eq!(token_value.get("access_token").and_then(Value::as_str), Some("at-12345"));
        assert_eq!(loaded.granted_scopes, vec!["repo".to_string(), "read:user".to_string()]);

        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none(), "cleared store loads None");
    }
}
