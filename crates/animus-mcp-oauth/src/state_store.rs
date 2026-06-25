//! [`SecretStore`]-backed [`rmcp::transport::auth::StateStore`].
//!
//! The interactive laptop flow ([`crate::flow::run_auth`]) runs the whole
//! OAuth2 PKCE handshake in ONE process, so rmcp's default
//! [`InMemoryStateStore`](rmcp::transport::auth::InMemoryStateStore) is fine:
//! the PKCE verifier minted by `get_authorization_url` lives in memory until
//! `exchange_code_for_token` consumes it.
//!
//! The delegated begin/complete flow ([`crate::flow::begin_auth`] /
//! [`crate::flow::complete_auth`]) splits that handshake across TWO processes:
//! a remote host (the portal) drives the browser redirect, then a *fresh*
//! `animus mcp auth --complete` process exchanges the code. The PKCE verifier
//! must survive that process boundary, so this store persists rmcp's
//! [`StoredAuthorizationState`] (PKCE verifier + CSRF token + `created_at`, all
//! serializable) into the OS keychain ([`orchestrator_core::SecretStore`]) —
//! the same secure store the token bundle ([`crate::keychain_store`]) lands in.
//!
//! # Key derivation
//!
//! rmcp keys state by the OAuth CSRF token (one entry per in-flight authorize).
//! Each entry maps to a validated keychain KEY
//! `MCP_OAUTH_STATE__<sanitized>__<hash16>` where `<sanitized>` keeps a
//! human-readable `server_principal` prefix and `<hash16>` is a SHA-256 prefix
//! of `server\x00principal\x00url\x00csrf` — binding the entry to both the
//! upstream and the specific authorize attempt so concurrent or repointed
//! flows never collide. The value is the JSON-serialized
//! `StoredAuthorizationState`.
//!
//! # Expiry
//!
//! rmcp delegates TTL to the store. `load` rejects (and best-effort deletes)
//! any state older than [`STATE_TTL_SECS`], so an abandoned `begin` cannot leave
//! a usable PKCE verifier in the keychain indefinitely.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use orchestrator_core::SecretStore;
use rmcp::transport::auth::{AuthError, StateStore, StoredAuthorizationState};
use sha2::{Digest, Sha256};

/// Prefix applied to every derived keychain KEY so transient OAuth state
/// entries are visually distinct from token bundles (`MCP_OAUTH__`) and
/// ordinary project secrets in the index.
const STATE_KEY_PREFIX: &str = "MCP_OAUTH_STATE__";

/// Cap on the sanitized human-readable portion of the derived KEY so an
/// unusually long server or principal name cannot blow past keychain KEY
/// length limits. Matches [`crate::keychain_store`]'s cap.
const SANITIZED_CAP: usize = 80;

/// Maximum age of a persisted authorization state before `load` treats it as
/// expired. A delegated begin -> browser consent -> complete round trip is
/// interactive but short; 15 minutes is generous while bounding how long an
/// abandoned flow's PKCE verifier lingers in the keychain.
pub const STATE_TTL_SECS: u64 = 15 * 60;

/// Derive a [`validate_key`](orchestrator_core::secret_store::validate_key)-safe
/// keychain KEY for the transient authorization state of one in-flight
/// authorize, bound to `(server, principal, url, csrf)`.
///
/// The KEY is `MCP_OAUTH_STATE__<sanitized>__<hash16>` where `<sanitized>` is
/// `server` and `principal` joined by `_` (every character outside
/// `[A-Za-z0-9_]` replaced by `_`, truncated) and `<hash16>` is a 16-hex-char
/// SHA-256 prefix of `server\x00principal\x00url\x00csrf`. Including the CSRF
/// token in the hash gives each authorize attempt its own entry; including the
/// URL keeps it consistent with the token store's host binding.
#[must_use]
pub fn derive_state_key(server: &str, principal: &str, url: &str, csrf: &str) -> String {
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
    hasher.update([0u8]);
    hasher.update(csrf.as_bytes());
    let digest = hasher.finalize();
    let mut hash_hex = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(hash_hex, "{byte:02x}");
    }

    format!("{STATE_KEY_PREFIX}{sanitized}__{hash_hex}")
}

/// Current unix time in seconds, saturating at 0 if the clock is before the
/// epoch (which would only happen on a badly misconfigured host).
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// rmcp [`StateStore`] persisting [`StoredAuthorizationState`] into the OS
/// keychain, one entry per in-flight authorize keyed by CSRF token.
///
/// Bound to one `(server, principal, url)` triple at construction so the
/// begin and complete processes — which each build their own
/// `AuthorizationManager` for the same server — derive identical keys for a
/// given CSRF token and thus see the same persisted state.
pub struct PersistentStateStore {
    secrets: Arc<dyn SecretStore>,
    server: String,
    principal: String,
    url: String,
}

impl PersistentStateStore {
    /// Bind a store to `(server, principal, url)` over the given keychain.
    #[must_use]
    pub fn new(secrets: Arc<dyn SecretStore>, server: &str, principal: &str, url: &str) -> Self {
        Self { secrets, server: server.to_string(), principal: principal.to_string(), url: url.to_string() }
    }

    fn key_for(&self, csrf: &str) -> String {
        derive_state_key(&self.server, &self.principal, &self.url, csrf)
    }
}

#[async_trait]
impl StateStore for PersistentStateStore {
    async fn save(&self, csrf_token: &str, state: StoredAuthorizationState) -> Result<(), AuthError> {
        let json = serde_json::to_string(&state)
            .map_err(|err| AuthError::InternalError(format!("authorization state serialize failed: {err}")))?;
        self.secrets
            .set(&self.key_for(csrf_token), &json)
            .map_err(|err| AuthError::InternalError(format!("keychain write failed: {err}")))?;
        Ok(())
    }

    async fn load(&self, csrf_token: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        let key = self.key_for(csrf_token);
        let raw =
            self.secrets.get(&key).map_err(|err| AuthError::InternalError(format!("keychain read failed: {err}")))?;
        let Some(json) = raw else {
            return Ok(None);
        };
        let state = match serde_json::from_str::<StoredAuthorizationState>(&json) {
            Ok(state) => state,
            Err(err) => {
                // A corrupt/unparseable entry degrades to "no state" rather
                // than an error: an in-flight complete should fail closed
                // (re-auth) instead of surfacing an InternalError. Leave the
                // entry in place — a fresh save() overwrites it.
                tracing::warn!(
                    key = %key,
                    error = %err,
                    "stored MCP OAuth authorization state is unparseable; treating as expired"
                );
                return Ok(None);
            }
        };
        // rmcp delegates TTL to the store: reject and best-effort sweep any
        // state past its lifetime so an abandoned begin can't be completed.
        if now_secs().saturating_sub(state.created_at) > STATE_TTL_SECS {
            let _ = self.secrets.delete(&key);
            return Ok(None);
        }
        Ok(Some(state))
    }

    async fn delete(&self, csrf_token: &str) -> Result<(), AuthError> {
        self.secrets
            .delete(&self.key_for(csrf_token))
            .map_err(|err| AuthError::InternalError(format!("keychain delete failed: {err}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::secret_store::{validate_key, MockSecretStore};
    use serde_json::json;

    fn state_json(csrf: &str, created_at: u64) -> StoredAuthorizationState {
        // StoredAuthorizationState is #[non_exhaustive] with no public
        // constructor we can call without rmcp's PkceCodeVerifier/CsrfToken
        // newtypes, but it derives Deserialize, so build one via serde.
        serde_json::from_value(json!({
            "pkce_verifier": "verifier-secret",
            "csrf_token": csrf,
            "created_at": created_at,
        }))
        .expect("construct StoredAuthorizationState via serde")
    }

    #[test]
    fn derived_key_is_validate_key_safe() {
        for (server, principal, csrf) in [
            ("github", "default", "abc123"),
            ("api.githubcopilot.com/mcp/", "user@example.com", "state-with-dashes"),
            ("../../escape", "p:rin-cipal", ":weird/csrf"),
            ("", "", ""),
        ] {
            let key = derive_state_key(server, principal, "https://h/mcp", csrf);
            assert!(key.starts_with(STATE_KEY_PREFIX), "prefix missing: {key}");
            validate_key(&key).unwrap_or_else(|e| panic!("derived key {key} not validate-safe: {e}"));
        }
    }

    #[test]
    fn distinct_csrf_tokens_get_distinct_keys() {
        let a = derive_state_key("s", "p", "u", "csrf-a");
        let b = derive_state_key("s", "p", "u", "csrf-b");
        assert_ne!(a, b, "different csrf tokens must not collide");
    }

    #[tokio::test]
    async fn save_load_delete_round_trip() {
        let secrets = Arc::new(MockSecretStore::new());
        let store = PersistentStateStore::new(secrets, "github", "default", "https://h/mcp");
        let csrf = "csrf-roundtrip";

        assert!(store.load(csrf).await.unwrap().is_none(), "empty store loads None");

        store.save(csrf, state_json(csrf, now_secs())).await.unwrap();
        let loaded = store.load(csrf).await.unwrap().expect("state present after save");
        assert_eq!(loaded.pkce_verifier, "verifier-secret");
        assert_eq!(loaded.csrf_token, csrf);

        store.delete(csrf).await.unwrap();
        assert!(store.load(csrf).await.unwrap().is_none(), "state gone after delete");
    }

    #[tokio::test]
    async fn expired_state_loads_none_and_is_swept() {
        let secrets = Arc::new(MockSecretStore::new());
        let store = PersistentStateStore::new(secrets.clone(), "s", "p", "u");
        let csrf = "csrf-expired";
        let stale = now_secs().saturating_sub(STATE_TTL_SECS + 60);
        store.save(csrf, state_json(csrf, stale)).await.unwrap();

        assert!(store.load(csrf).await.unwrap().is_none(), "expired state must load as None");
        // Swept on load: the underlying key is gone.
        assert!(
            secrets.get(&derive_state_key("s", "p", "u", csrf)).unwrap().is_none(),
            "expired state should be deleted on load"
        );
    }

    #[tokio::test]
    async fn corrupt_entry_degrades_to_none() {
        let secrets = Arc::new(MockSecretStore::new());
        let store = PersistentStateStore::new(secrets.clone(), "s", "p", "u");
        let csrf = "csrf-corrupt";
        secrets.set(&derive_state_key("s", "p", "u", csrf), "not json").unwrap();
        assert!(store.load(csrf).await.unwrap().is_none(), "corrupt entry must load as None");
    }
}
