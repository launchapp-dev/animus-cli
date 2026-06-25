//! [`SecretStore`]-backed persistence for an in-flight delegated authorization.
//!
//! The delegated begin/complete flow runs across two processes (see
//! [`crate::state_store`]). rmcp's [`StateStore`](rmcp::transport::auth::StateStore)
//! carries the PKCE verifier, but the `complete` process also needs the
//! non-secret parameters `begin` resolved — the protected URL, the resolved
//! scopes, the principal, the exact `redirect_uri` the authorize was minted
//! with (the token endpoint requires it to match), and the resolved/registered
//! public `client_id` — so it can rebuild the same `AuthorizationManager` and
//! exchange without re-running discovery consent or Dynamic Client Registration.
//!
//! That bundle is [`PendingAuth`]. It holds NO secret: the PKCE verifier lives
//! in the [`StateStore`](crate::state_store), and DCR registers a PUBLIC client
//! (`token_endpoint_auth_method: "none"`), so there is no client secret to
//! persist. It is keyed by the OAuth CSRF/`state` token (one per in-flight
//! authorize) and TTL-swept exactly like the PKCE state.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use orchestrator_core::SecretStore;
use serde::{Deserialize, Serialize};

/// Prefix applied to every derived keychain KEY so pending-auth records are
/// visually distinct from token bundles (`MCP_OAUTH__`) and PKCE state
/// (`MCP_OAUTH_STATE__`) in the index.
const PENDING_KEY_PREFIX: &str = "MCP_OAUTH_PENDING__";

/// Cap on the sanitized human-readable portion of the derived KEY. Matches
/// the sibling stores.
const SANITIZED_CAP: usize = 80;

/// Maximum age of a pending record before [`PendingStore::load`] treats it as
/// expired. Matches [`crate::state_store::STATE_TTL_SECS`] so the PKCE state and
/// its companion pending record expire together.
pub const PENDING_TTL_SECS: u64 = 15 * 60;

/// The non-secret parameters a delegated `begin` resolved, persisted so a fresh
/// `complete` process can rebuild the same authorization exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAuth {
    /// The server name the user authorized (`animus mcp auth <server>`).
    pub server: String,
    /// The protected MCP base URL (OAuth `resource` + discovery seed).
    pub url: String,
    /// The scopes `begin` requested (post-resolution / auto-detection).
    pub scopes: Vec<String>,
    /// True when `scopes` were auto-detected from the server's advertised set.
    pub scopes_auto_detected: bool,
    /// The repo-scope principal the token is bound to.
    pub principal: String,
    /// The exact redirect_uri the authorize URL was minted with. The token
    /// endpoint requires the exchange's redirect_uri to match, so `complete`
    /// must reuse this verbatim.
    pub redirect_uri: String,
    /// The resolved client id (a pinned id, or the id DCR registered in
    /// `begin`).
    pub client_id: String,
    /// The DCR-issued client secret, when the registration server returned a
    /// non-empty one (a confidential client). `None` for public clients (the
    /// common case — DCR registers `token_endpoint_auth_method: "none"`). Stored
    /// here so `complete_auth` can authenticate the token exchange with the same
    /// client `begin` registered. Lives in the keychain, same as the token.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Unix seconds when the record was written, for TTL expiry.
    pub created_at: u64,
}

/// Current unix time in seconds, saturating at 0 before the epoch.
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Derive a [`validate_key`](orchestrator_core::secret_store::validate_key)-safe
/// keychain KEY for the pending record of one in-flight authorize, bound to
/// `(server, principal, state)`. Deliberately NOT bound to the URL: `complete`
/// must locate the record from the callback's `state` alone (the URL it would
/// need lives *inside* the record), and the unguessable random `state` is the
/// entropy that scopes the entry. The `MCP_OAUTH_PENDING__` prefix keeps it
/// distinct from the PKCE state ([`crate::state_store`]) and token entries.
#[must_use]
pub fn derive_pending_key(server: &str, principal: &str, state: &str) -> String {
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

    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(server.as_bytes());
    hasher.update([0u8]);
    hasher.update(principal.as_bytes());
    hasher.update([0u8]);
    hasher.update(state.as_bytes());
    let digest = hasher.finalize();
    let mut hash_hex = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(hash_hex, "{byte:02x}");
    }

    format!("{PENDING_KEY_PREFIX}{sanitized}__{hash_hex}")
}

/// [`SecretStore`]-backed store for [`PendingAuth`] records, one per in-flight
/// authorize keyed by CSRF/`state` token. Bound to `(server, principal)` so
/// `complete` can locate the record from the callback `state` without knowing
/// the URL up front (the URL is read back from the record).
pub struct PendingStore {
    secrets: Arc<dyn SecretStore>,
    server: String,
    principal: String,
}

impl PendingStore {
    /// Bind a store to `(server, principal)` over the given keychain.
    #[must_use]
    pub fn new(secrets: Arc<dyn SecretStore>, server: &str, principal: &str) -> Self {
        Self { secrets, server: server.to_string(), principal: principal.to_string() }
    }

    fn key_for(&self, state: &str) -> String {
        derive_pending_key(&self.server, &self.principal, state)
    }

    /// Persist a pending record keyed by `state`. `created_at` is stamped here.
    pub fn save(&self, state: &str, mut pending: PendingAuth) -> anyhow::Result<()> {
        pending.created_at = now_secs();
        let json = serde_json::to_string(&pending)?;
        self.secrets.set(&self.key_for(state), &json).map_err(|err| anyhow::anyhow!("keychain write failed: {err}"))?;
        Ok(())
    }

    /// Load the pending record for `state`, or `None` if absent, expired, or
    /// unparseable. Expired records are best-effort swept.
    pub fn load(&self, state: &str) -> anyhow::Result<Option<PendingAuth>> {
        let key = self.key_for(state);
        let Some(json) = self.secrets.get(&key).map_err(|err| anyhow::anyhow!("keychain read failed: {err}"))? else {
            return Ok(None);
        };
        let pending = match serde_json::from_str::<PendingAuth>(&json) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(key = %key, error = %err, "pending MCP OAuth record is unparseable; treating as expired");
                return Ok(None);
            }
        };
        if now_secs().saturating_sub(pending.created_at) > PENDING_TTL_SECS {
            let _ = self.secrets.delete(&key);
            return Ok(None);
        }
        Ok(Some(pending))
    }

    /// Delete the pending record for `state` (called on successful complete).
    pub fn delete(&self, state: &str) -> anyhow::Result<()> {
        self.secrets.delete(&self.key_for(state)).map_err(|err| anyhow::anyhow!("keychain delete failed: {err}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::secret_store::{validate_key, MockSecretStore};

    fn sample(client_id: &str) -> PendingAuth {
        PendingAuth {
            server: "github".to_string(),
            url: "https://h/mcp".to_string(),
            scopes: vec!["repo".to_string(), "read".to_string()],
            scopes_auto_detected: true,
            principal: "default".to_string(),
            redirect_uri: "https://portal/api/mcp-oauth/callback".to_string(),
            client_id: client_id.to_string(),
            client_secret: None,
            created_at: 0,
        }
    }

    #[test]
    fn derived_key_is_validate_key_safe() {
        for (server, principal, state) in
            [("github", "default", "abc"), ("api.host/mcp/", "u@e.com", ":weird/state"), ("", "", "")]
        {
            let key = derive_pending_key(server, principal, state);
            assert!(key.starts_with(PENDING_KEY_PREFIX), "prefix missing: {key}");
            validate_key(&key).unwrap_or_else(|e| panic!("derived key {key} not validate-safe: {e}"));
        }
    }

    #[test]
    fn pending_and_state_keys_never_collide() {
        let pending = derive_pending_key("s", "p", "state-x");
        let state = crate::state_store::derive_state_key("s", "p", "u", "state-x");
        assert_ne!(pending, state, "pending and state entries must use distinct keys");
    }

    #[test]
    fn save_load_delete_round_trip() {
        let secrets = Arc::new(MockSecretStore::new());
        let store = PendingStore::new(secrets, "github", "default");
        let state = "state-roundtrip";

        assert!(store.load(state).unwrap().is_none(), "empty store loads None");
        store.save(state, sample("client-123")).unwrap();
        let loaded = store.load(state).unwrap().expect("present after save");
        assert_eq!(loaded.client_id, "client-123");
        assert_eq!(loaded.redirect_uri, "https://portal/api/mcp-oauth/callback");
        assert_eq!(loaded.scopes, vec!["repo".to_string(), "read".to_string()]);
        assert!(loaded.created_at > 0, "save stamps created_at");

        store.delete(state).unwrap();
        assert!(store.load(state).unwrap().is_none(), "gone after delete");
    }

    #[test]
    fn expired_pending_loads_none_and_is_swept() {
        let secrets = Arc::new(MockSecretStore::new());
        let store = PendingStore::new(secrets.clone(), "s", "p");
        let state = "state-expired";
        // Write a record with a stale created_at directly (bypass save()'s stamp).
        let mut stale = sample("c");
        stale.created_at = now_secs().saturating_sub(PENDING_TTL_SECS + 60);
        secrets.set(&derive_pending_key("s", "p", state), &serde_json::to_string(&stale).unwrap()).unwrap();

        assert!(store.load(state).unwrap().is_none(), "expired record loads None");
        assert!(secrets.get(&derive_pending_key("s", "p", state)).unwrap().is_none(), "expired record swept on load");
    }
}
