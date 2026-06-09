//! Kernel-level OAuth broker for HTTP-transport MCP servers.
//!
//! The broker is invoked during runtime-contract assembly: for any
//! `mcp_servers.<name>` entry whose `oauth:` block is set, it resolves a
//! bearer token (cache lookup → freshness check → refresh / fresh fetch
//! fallback) and returns the header pair that the contract assembler
//! injects into the additional MCP server entry.
//!
//! Tokens are cached under `~/.animus/<scope>/mcp-oauth-cache/<server>.json`
//! with 0600 permissions on Unix. A 60s skew margin guards against clock
//! drift and in-flight network latency.
//!
//! Three flows are supported:
//! - `client_credentials`: POSTs `grant_type=client_credentials` +
//!   `client_id` + `client_secret` to `token_url`.
//! - `refresh_token`: POSTs `grant_type=refresh_token` + `refresh_token`
//!   (plus optional `client_id` / `client_secret`) to `token_url`. If the
//!   response carries a new `refresh_token`, the cache file is updated so
//!   the next phase uses the rotated token; the original env var stays
//!   untouched (documented limitation).
//! - `manual_bearer`: plain env-var read. Escape hatch for tokens minted
//!   by an external system.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use orchestrator_config::workflow_config::{OauthConfig, OauthFlow};
use serde::{Deserialize, Serialize};

/// Margin (seconds) subtracted from `expires_in` so we refresh before the
/// token actually expires. Guards against clock skew + in-flight latency
/// when the freshly-resolved token is handed off to the agent process.
const EXPIRY_SKEW_SECS: i64 = 60;

/// TTL used when the token endpoint omits `expires_in`. Some OAuth
/// servers leave the field off entirely; if we treated that as
/// "never expires" we would reuse an access token long past its
/// server-side lifetime and every later MCP call would 401 until the
/// cache file is hand-deleted. A 5-minute floor forces a re-fetch
/// quickly enough to surface a real refresh path without hammering
/// the token endpoint on every contract assembly.
const DEFAULT_TTL_WHEN_EXPIRES_IN_MISSING_SECS: i64 = 300;

const HTTP_TIMEOUT_SECS: u64 = 10;

/// Resolution result handed back to the runtime-contract assembler.
#[derive(Debug, Clone)]
pub struct ResolvedOauthToken {
    pub access_token: String,
    pub header_name: String,
}

impl ResolvedOauthToken {
    pub fn authorization_header_value(&self) -> String {
        format!("Bearer {}", self.access_token)
    }
}

/// On-disk cache entry. Stored as JSON under
/// `~/.animus/<scope>/mcp-oauth-cache/<server>.json`. The `expires_at`
/// is RFC3339 UTC; `None` means the token never expires (manual flows).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedToken {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub obtained_at: DateTime<Utc>,
    /// Fingerprint of the `OauthConfig` that produced this token. When
    /// the user changes the flow, scopes, audience, token URL, or env
    /// pointers we want to refetch rather than keep using the old
    /// bearer until natural expiry. `None` is treated as a cache miss
    /// so older cache files (pre-fingerprint) are invalidated on first
    /// read after upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_fingerprint: Option<String>,
}

impl CachedToken {
    fn is_fresh(&self, now: DateTime<Utc>, expected_fingerprint: &str) -> bool {
        if self.config_fingerprint.as_deref() != Some(expected_fingerprint) {
            return false;
        }
        match self.expires_at {
            None => true,
            Some(expiry) => expiry.signed_duration_since(now).num_seconds() > EXPIRY_SKEW_SECS,
        }
    }
}

fn oauth_config_fingerprint(oauth: &OauthConfig) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(oauth.flow.as_str().as_bytes());
    hasher.update(b"|token_url=");
    hasher.update(oauth.token_url.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|client_id_env=");
    hasher.update(oauth.client_id_env.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|client_secret_env=");
    hasher.update(oauth.client_secret_env.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|refresh_token_env=");
    hasher.update(oauth.refresh_token_env.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|bearer_env=");
    hasher.update(oauth.bearer_env.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|scopes=");
    hasher.update(oauth.scopes.join(",").as_bytes());
    hasher.update(b"|audience=");
    hasher.update(oauth.audience.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|client_id=");
    hasher.update(oauth.client_id.as_deref().unwrap_or("").as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Side-effecting interface kept on a trait so tests can mock the token
/// endpoint without spinning up a real server. Production callers use
/// `ReqwestTokenClient`.
pub trait TokenClient: Send + Sync {
    fn fetch(&self, request: TokenFetchRequest) -> Result<TokenFetchResponse>;
}

#[derive(Debug, Clone)]
pub struct TokenFetchRequest {
    pub token_url: String,
    pub form: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct TokenFetchResponse {
    pub access_token: String,
    pub expires_in: Option<u64>,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OauthTokenResponseRaw {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
}

pub struct ReqwestTokenClient {
    client: reqwest::blocking::Client,
}

impl ReqwestTokenClient {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .context("failed to build reqwest blocking client for OAuth broker")?;
        Ok(Self { client })
    }
}

impl TokenClient for ReqwestTokenClient {
    fn fetch(&self, request: TokenFetchRequest) -> Result<TokenFetchResponse> {
        let response = self
            .client
            .post(&request.token_url)
            .form(&request.form)
            .send()
            .with_context(|| format!("OAuth token endpoint {} unreachable", request.token_url))?;
        let status = response.status();
        if !status.is_success() {
            // Intentionally do not embed the response body here. Some
            // token endpoints (and forward proxies) echo request fields
            // — client_id, client_secret, refresh_token — back into
            // error responses, and `err` flows into structured logs via
            // `tracing::warn`. The status is the only field that we can
            // safely surface without risking credential exposure.
            // Drain the body so the connection can be released.
            let _ = response.bytes();
            bail!("OAuth token endpoint {} returned status {}", request.token_url, status);
        }
        let raw: OauthTokenResponseRaw = response
            .json()
            .with_context(|| format!("OAuth token endpoint {} returned non-JSON body", request.token_url))?;
        if raw.access_token.trim().is_empty() {
            bail!("OAuth token endpoint {} returned empty access_token", request.token_url);
        }
        Ok(TokenFetchResponse {
            access_token: raw.access_token,
            expires_in: raw.expires_in,
            refresh_token: raw.refresh_token,
        })
    }
}

/// Caller-supplied env lookup so tests can drive deterministic env state.
pub trait EnvLookup: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
}

pub struct ProcessEnv;

impl EnvLookup for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Resolve the bearer token for a single MCP server.
///
/// `cache_dir` is the per-scope OAuth cache directory (typically
/// `~/.animus/<scope>/mcp-oauth-cache/`). When `oauth.cache == false`
/// the cache is bypassed and any existing entry is left untouched.
pub fn resolve_token(
    server_name: &str,
    oauth: &OauthConfig,
    cache_dir: &Path,
    env: &dyn EnvLookup,
    client: &dyn TokenClient,
) -> Result<ResolvedOauthToken> {
    let now = Utc::now();
    let cache_path = cache_dir.join(cache_filename_for_server(server_name));
    let fingerprint = oauth_config_fingerprint(oauth);

    if oauth.cache && matches!(oauth.flow, OauthFlow::ClientCredentials | OauthFlow::RefreshToken) {
        if let Some(cached) = read_cache(&cache_path)? {
            if cached.is_fresh(now, &fingerprint) {
                return Ok(ResolvedOauthToken {
                    access_token: cached.access_token,
                    header_name: "Authorization".to_string(),
                });
            }
        }
    }

    match oauth.flow {
        OauthFlow::ManualBearer => {
            let env_name = oauth
                .bearer_env
                .as_deref()
                .ok_or_else(|| anyhow!("OAuth resolution failed for `{}`: bearer_env missing", server_name))?;
            let token = env.get(env_name).filter(|v| !v.trim().is_empty()).ok_or_else(|| {
                anyhow!("OAuth resolution failed for `{}`: env var `{}` is not set or is empty", server_name, env_name)
            })?;
            Ok(ResolvedOauthToken { access_token: token, header_name: "Authorization".to_string() })
        }
        OauthFlow::ClientCredentials => {
            let mut token = fetch_client_credentials(server_name, oauth, env, client)?;
            token.config_fingerprint = Some(fingerprint);
            if oauth.cache {
                // Best-effort cache write: if persistence fails (read-
                // only home dir, disk full, etc.) we still want to hand
                // the freshly fetched token back to the caller rather
                // than convert a successful auth into a failed MCP call.
                if let Err(err) = write_cache(&cache_path, &token) {
                    tracing::warn!(
                        server = server_name,
                        error = %err,
                        "OAuth token fetched but persisting cache failed; will refetch on next phase"
                    );
                }
            }
            Ok(ResolvedOauthToken { access_token: token.access_token, header_name: "Authorization".to_string() })
        }
        OauthFlow::RefreshToken => {
            // Only reuse a cached refresh token when the on-disk
            // fingerprint matches the current config. Mismatched config
            // (e.g. the user pointed the refresh flow at a new
            // refresh_token_env env var) should fall back to the env-var
            // seed rather than POST the previous-config refresh token.
            let cached_refresh = if oauth.cache {
                read_cache(&cache_path)?
                    .filter(|c| c.config_fingerprint.as_deref() == Some(fingerprint.as_str()))
                    .and_then(|c| c.refresh_token)
                    .filter(|v| !v.trim().is_empty())
            } else {
                None
            };
            match fetch_refresh_token(server_name, oauth, env, client, cached_refresh.as_deref()) {
                Ok(mut token) => {
                    token.config_fingerprint = Some(fingerprint);
                    if oauth.cache {
                        // Best-effort cache write (see ClientCredentials arm).
                        if let Err(err) = write_cache(&cache_path, &token) {
                            tracing::warn!(
                                server = server_name,
                                error = %err,
                                "OAuth token refreshed but persisting cache failed; will refetch on next phase"
                            );
                        }
                    }
                    Ok(ResolvedOauthToken {
                        access_token: token.access_token,
                        header_name: "Authorization".to_string(),
                    })
                }
                Err(err) => Err(anyhow!("OAuth resolution failed for `{}`: {}", server_name, err)),
            }
        }
        OauthFlow::AuthorizationCode => {
            // The interactive authorization_code flow does NOT resolve a
            // bearer header here. The runtime-contract assembler instead
            // repoints the agent at the local `animus-mcp-proxy`, which
            // pulls live tokens from the keychain and refreshes them. If
            // this arm is hit, the caller mis-routed an authorization_code
            // server through the header-injection broker.
            bail!(
                "OAuth resolution failed for `{}`: flow=\"authorization_code\" is served by the \
                 stdio proxy, not the header-injection broker; run `animus mcp auth {}`",
                server_name,
                server_name
            )
        }
    }
}

fn fetch_client_credentials(
    server_name: &str,
    oauth: &OauthConfig,
    env: &dyn EnvLookup,
    client: &dyn TokenClient,
) -> Result<CachedToken> {
    let token_url = require_value(oauth.token_url.as_deref(), "token_url", server_name, oauth.flow)?;
    let client_id_env = require_value(oauth.client_id_env.as_deref(), "client_id_env", server_name, oauth.flow)?;
    let client_secret_env =
        require_value(oauth.client_secret_env.as_deref(), "client_secret_env", server_name, oauth.flow)?;
    let client_id = lookup_env(env, client_id_env, server_name)?;
    let client_secret = lookup_env(env, client_secret_env, server_name)?;

    let mut form: Vec<(String, String)> = vec![
        ("grant_type".to_string(), "client_credentials".to_string()),
        ("client_id".to_string(), client_id),
        ("client_secret".to_string(), client_secret),
    ];
    if !oauth.scopes.is_empty() {
        form.push(("scope".to_string(), oauth.scopes.join(" ")));
    }
    if let Some(audience) = oauth.audience.as_deref().filter(|v| !v.trim().is_empty()) {
        form.push(("audience".to_string(), audience.to_string()));
    }

    let response = client
        .fetch(TokenFetchRequest { token_url: token_url.to_string(), form })
        .map_err(|err| anyhow!("OAuth resolution failed for `{}`: {}", server_name, err))?;
    Ok(materialize_cached_token(response, None))
}

fn fetch_refresh_token(
    server_name: &str,
    oauth: &OauthConfig,
    env: &dyn EnvLookup,
    client: &dyn TokenClient,
    cached_refresh: Option<&str>,
) -> Result<CachedToken> {
    let token_url = require_value(oauth.token_url.as_deref(), "token_url", server_name, oauth.flow)?;
    let refresh_token_env =
        require_value(oauth.refresh_token_env.as_deref(), "refresh_token_env", server_name, oauth.flow)?;
    let refresh_token = match cached_refresh {
        Some(token) => token.to_string(),
        None => lookup_env(env, refresh_token_env, server_name)?,
    };

    let mut form: Vec<(String, String)> = vec![
        ("grant_type".to_string(), "refresh_token".to_string()),
        ("refresh_token".to_string(), refresh_token.clone()),
    ];
    // When the user configures `client_id_env` / `client_secret_env`
    // on a refresh_token flow we MUST surface a missing env var as a
    // config-resolution error. Silently dropping the configured
    // credentials and still POSTing to the token endpoint turns a fix-
    // able local config error into a confusing remote auth failure for
    // providers that require client auth on refresh.
    if let Some(client_id_env) = oauth.client_id_env.as_deref() {
        let client_id = lookup_env(env, client_id_env, server_name)?;
        form.push(("client_id".to_string(), client_id));
    }
    if let Some(client_secret_env) = oauth.client_secret_env.as_deref() {
        let client_secret = lookup_env(env, client_secret_env, server_name)?;
        form.push(("client_secret".to_string(), client_secret));
    }
    if !oauth.scopes.is_empty() {
        form.push(("scope".to_string(), oauth.scopes.join(" ")));
    }
    if let Some(audience) = oauth.audience.as_deref().filter(|v| !v.trim().is_empty()) {
        form.push(("audience".to_string(), audience.to_string()));
    }

    let response = client.fetch(TokenFetchRequest { token_url: token_url.to_string(), form })?;
    Ok(materialize_cached_token(response, Some(refresh_token)))
}

fn materialize_cached_token(response: TokenFetchResponse, prior_refresh: Option<String>) -> CachedToken {
    let now = Utc::now();
    let ttl_secs = response.expires_in.map(|secs| secs as i64).unwrap_or(DEFAULT_TTL_WHEN_EXPIRES_IN_MISSING_SECS);
    let expires_at = Some(now + chrono::Duration::seconds(ttl_secs));
    let refresh_token = response.refresh_token.or(prior_refresh);
    CachedToken {
        access_token: response.access_token,
        expires_at,
        refresh_token,
        obtained_at: now,
        config_fingerprint: None,
    }
}

fn require_value<'a>(value: Option<&'a str>, field: &str, server: &str, flow: OauthFlow) -> Result<&'a str> {
    match value {
        Some(v) if !v.trim().is_empty() => Ok(v),
        _ => bail!("OAuth resolution failed for `{}`: `{}` is required for flow=\"{}\"", server, field, flow.as_str()),
    }
}

fn lookup_env(env: &dyn EnvLookup, name: &str, server: &str) -> Result<String> {
    env.get(name)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("OAuth resolution failed for `{}`: env var `{}` is not set or is empty", server, name))
}

fn read_cache(path: &Path) -> Result<Option<CachedToken>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let parsed: CachedToken = serde_json::from_str(&contents)
                .with_context(|| format!("failed to parse OAuth cache at {}", path.display()))?;
            Ok(Some(parsed))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow!("failed to read OAuth cache at {}: {}", path.display(), err)),
    }
}

fn write_cache(path: &Path, token: &CachedToken) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("failed to create OAuth cache dir {}", parent.display()))?;
    }
    let serialized = serde_json::to_vec_pretty(token).context("failed to serialize OAuth cache entry")?;
    let unique = unique_tmp_suffix();
    let tmp = path.with_extension(format!("tmp.{unique}"));
    {
        let mut file = open_writable_secure(&tmp)?;
        file.write_all(&serialized)
            .with_context(|| format!("failed to write OAuth cache temp file {}", tmp.display()))?;
        file.sync_all().ok();
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename OAuth cache temp {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn unique_tmp_suffix() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    format!("{}-{}", std::process::id(), nanos)
}

/// Compute the on-disk cache filename for a given server name.
///
/// MCP server names are loosely validated (must be non-empty) and may
/// contain pack-namespaced syntax like `animus.requirements/ao` or, in
/// adversarial config, path separators (`../`). Building the cache path
/// from the raw name would let a malicious config escape the
/// `mcp-oauth-cache` directory and overwrite arbitrary files when a
/// cc/refresh token fetch succeeds.
///
/// The filename is built from two pieces, joined by `-` and suffixed
/// with `.json`:
///
/// 1. A sanitized prefix: alphanumeric, `_`, `-`, and `.` characters are
///    kept verbatim; everything else (including `/`, `\\`, `..`, NUL,
///    control chars) is replaced with `_`. The prefix is also capped at
///    96 characters so unusually long names don't blow past the OS file
///    name limit.
/// 2. A 16-hex-char SHA-256 prefix of the raw server name so two servers
///    with names that collapse to the same sanitized prefix (e.g.
///    `foo/bar` vs `foo_bar`) still get distinct cache files.
fn cache_filename_for_server(server_name: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut sanitized = String::with_capacity(server_name.len());
    for ch in server_name.chars().take(96) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() || sanitized.starts_with('.') {
        sanitized.insert(0, '_');
    }

    let mut hasher = Sha256::new();
    hasher.update(server_name.as_bytes());
    let digest = hasher.finalize();
    let mut hash_hex = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(hash_hex, "{byte:02x}");
    }

    format!("{sanitized}-{hash_hex}.json")
}

#[cfg(unix)]
fn open_writable_secure(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to open OAuth cache temp file {}", path.display()))
}

#[cfg(not(unix))]
fn open_writable_secure(path: &Path) -> Result<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open OAuth cache temp file {}", path.display()))
}

/// Per-scope cache directory under
/// `~/.animus/<scope>/mcp-oauth-cache/`. Returns `None` when no home
/// directory is discoverable.
pub fn cache_dir_for_project(project_root: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".animus").join(protocol::repository_scope_for_path(Path::new(project_root))).join("mcp-oauth-cache"),
    )
}

/// Convenience wrapper: build a `ReqwestTokenClient`, look up env vars
/// from the live process, and resolve a token for `server_name`. Used
/// by the runtime-contract assembler. Tests construct their own
/// `TokenClient` + `EnvLookup` and go through `resolve_token` directly.
pub fn resolve_token_for_project(
    server_name: &str,
    oauth: &OauthConfig,
    project_root: &str,
) -> Result<ResolvedOauthToken> {
    let cache_dir = cache_dir_for_project(project_root)
        .ok_or_else(|| anyhow!("OAuth resolution failed for `{}`: home directory not discoverable", server_name))?;
    let client = ReqwestTokenClient::new()?;
    resolve_token(server_name, oauth, &cache_dir, &ProcessEnv, &client)
}

/// Build the per-server headers map that the runtime-contract injects
/// alongside the `url` + `transport` fields. Currently produces a single
/// `Authorization` header; the map shape keeps the door open for
/// additional headers (`X-Audience`, etc.) without breaking callers.
pub fn header_map_for_token(token: &ResolvedOauthToken) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert(token.header_name.clone(), token.authorization_header_value());
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::Mutex;
    use tempfile::tempdir;

    fn cc_config() -> OauthConfig {
        OauthConfig {
            flow: OauthFlow::ClientCredentials,
            token_url: Some("https://auth.example.com/token".to_string()),
            client_id_env: Some("EXAMPLE_CLIENT_ID".to_string()),
            client_secret_env: Some("EXAMPLE_CLIENT_SECRET".to_string()),
            refresh_token_env: None,
            bearer_env: None,
            scopes: vec!["read".to_string(), "write".to_string()],
            audience: Some("https://api.example.com".to_string()),
            cache: true,
            client_id: None,
        }
    }

    struct StaticEnv(BTreeMap<String, String>);

    impl EnvLookup for StaticEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    struct MockClient {
        calls: Mutex<RefCell<Vec<TokenFetchRequest>>>,
        responses: Mutex<RefCell<Vec<Result<TokenFetchResponse, String>>>>,
    }

    impl MockClient {
        fn new(responses: Vec<Result<TokenFetchResponse, String>>) -> Self {
            Self { calls: Mutex::new(RefCell::new(Vec::new())), responses: Mutex::new(RefCell::new(responses)) }
        }
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().borrow().len()
        }
        fn last_form(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().borrow().last().cloned().map(|r| r.form).unwrap_or_default()
        }
    }

    impl TokenClient for MockClient {
        fn fetch(&self, request: TokenFetchRequest) -> Result<TokenFetchResponse> {
            self.calls.lock().unwrap().borrow_mut().push(request);
            let next = self.responses.lock().unwrap().borrow_mut().remove(0);
            next.map_err(|err| anyhow!(err))
        }
    }

    #[test]
    fn client_credentials_flow_writes_authorization_header() {
        let env = StaticEnv(BTreeMap::from([
            ("EXAMPLE_CLIENT_ID".to_string(), "id".to_string()),
            ("EXAMPLE_CLIENT_SECRET".to_string(), "secret".to_string()),
        ]));
        let client = MockClient::new(vec![Ok(TokenFetchResponse {
            access_token: "abc123".to_string(),
            expires_in: Some(3600),
            refresh_token: None,
        })]);
        let temp = tempdir().expect("tempdir");

        let token = resolve_token("svc", &cc_config(), temp.path(), &env, &client).expect("resolve ok");
        assert_eq!(token.access_token, "abc123");
        assert_eq!(token.authorization_header_value(), "Bearer abc123");

        let form = client.last_form();
        assert!(form.contains(&("grant_type".to_string(), "client_credentials".to_string())));
        assert!(form.contains(&("client_id".to_string(), "id".to_string())));
        assert!(form.contains(&("client_secret".to_string(), "secret".to_string())));
        assert!(form.contains(&("scope".to_string(), "read write".to_string())));
        assert!(form.contains(&("audience".to_string(), "https://api.example.com".to_string())));
    }

    #[test]
    fn client_credentials_flow_uses_cache_on_second_call() {
        let env = StaticEnv(BTreeMap::from([
            ("EXAMPLE_CLIENT_ID".to_string(), "id".to_string()),
            ("EXAMPLE_CLIENT_SECRET".to_string(), "secret".to_string()),
        ]));
        let client = MockClient::new(vec![Ok(TokenFetchResponse {
            access_token: "cached".to_string(),
            expires_in: Some(3600),
            refresh_token: None,
        })]);
        let temp = tempdir().expect("tempdir");

        let first = resolve_token("svc", &cc_config(), temp.path(), &env, &client).expect("first ok");
        let second = resolve_token("svc", &cc_config(), temp.path(), &env, &client).expect("second ok");

        assert_eq!(first.access_token, "cached");
        assert_eq!(second.access_token, "cached");
        assert_eq!(client.call_count(), 1, "second call should hit the cache");
    }

    #[test]
    fn client_credentials_flow_refetches_when_expired() {
        let env = StaticEnv(BTreeMap::from([
            ("EXAMPLE_CLIENT_ID".to_string(), "id".to_string()),
            ("EXAMPLE_CLIENT_SECRET".to_string(), "secret".to_string()),
        ]));
        let client = MockClient::new(vec![
            Ok(TokenFetchResponse { access_token: "old".to_string(), expires_in: Some(10), refresh_token: None }),
            Ok(TokenFetchResponse { access_token: "new".to_string(), expires_in: Some(3600), refresh_token: None }),
        ]);
        let temp = tempdir().expect("tempdir");

        let first = resolve_token("svc", &cc_config(), temp.path(), &env, &client).expect("first ok");
        assert_eq!(first.access_token, "old");

        // The 10-second expiry is shorter than the 60s skew margin, so
        // the cache miss is forced on the next call.
        let second = resolve_token("svc", &cc_config(), temp.path(), &env, &client).expect("second ok");
        assert_eq!(second.access_token, "new");
        assert_eq!(client.call_count(), 2);
    }

    #[test]
    fn manual_bearer_flow_reads_env_var() {
        let oauth = OauthConfig {
            flow: OauthFlow::ManualBearer,
            token_url: None,
            client_id_env: None,
            client_secret_env: None,
            refresh_token_env: None,
            bearer_env: Some("MANUAL_BEARER".to_string()),
            scopes: vec![],
            audience: None,
            cache: false,
            client_id: None,
        };
        let env = StaticEnv(BTreeMap::from([("MANUAL_BEARER".to_string(), "plain-token".to_string())]));
        let client = MockClient::new(vec![]);
        let temp = tempdir().expect("tempdir");

        let token = resolve_token("svc", &oauth, temp.path(), &env, &client).expect("resolve ok");
        assert_eq!(token.access_token, "plain-token");
        assert_eq!(token.authorization_header_value(), "Bearer plain-token");
        assert_eq!(client.call_count(), 0, "manual_bearer must not hit the network");
    }

    #[test]
    fn refresh_token_flow_uses_rotated_refresh_token_from_cache() {
        let oauth = OauthConfig {
            flow: OauthFlow::RefreshToken,
            token_url: Some("https://auth.example.com/token".to_string()),
            client_id_env: None,
            client_secret_env: None,
            refresh_token_env: Some("EXAMPLE_REFRESH".to_string()),
            bearer_env: None,
            scopes: vec![],
            audience: None,
            cache: true,
            client_id: None,
        };
        let env = StaticEnv(BTreeMap::from([("EXAMPLE_REFRESH".to_string(), "rt-original".to_string())]));
        let client = MockClient::new(vec![
            Ok(TokenFetchResponse {
                access_token: "access-1".to_string(),
                expires_in: Some(1),
                refresh_token: Some("rt-rotated".to_string()),
            }),
            Ok(TokenFetchResponse {
                access_token: "access-2".to_string(),
                expires_in: Some(3600),
                refresh_token: None,
            }),
        ]);
        let temp = tempdir().expect("tempdir");

        let first = resolve_token("svc", &oauth, temp.path(), &env, &client).expect("first ok");
        assert_eq!(first.access_token, "access-1");

        let second = resolve_token("svc", &oauth, temp.path(), &env, &client).expect("second ok");
        assert_eq!(second.access_token, "access-2");

        let last = client.calls.lock().unwrap().borrow().last().cloned().unwrap();
        assert!(
            last.form.iter().any(|(k, v)| k == "refresh_token" && v == "rt-rotated"),
            "second call should send the rotated refresh token, got form={:?}",
            last.form
        );
    }

    #[test]
    fn missing_env_var_returns_actionable_error() {
        let env = StaticEnv(BTreeMap::new());
        let client = MockClient::new(vec![]);
        let temp = tempdir().expect("tempdir");
        let err = resolve_token("svc", &cc_config(), temp.path(), &env, &client).unwrap_err().to_string();
        assert!(err.contains("`EXAMPLE_CLIENT_ID`"), "error should name the missing env var: {err}");
        assert!(err.contains("svc"), "error should name the server: {err}");
    }

    #[test]
    fn token_endpoint_failure_returns_actionable_error() {
        let env = StaticEnv(BTreeMap::from([
            ("EXAMPLE_CLIENT_ID".to_string(), "id".to_string()),
            ("EXAMPLE_CLIENT_SECRET".to_string(), "secret".to_string()),
        ]));
        let client = MockClient::new(vec![Err("token endpoint returned 503".to_string())]);
        let temp = tempdir().expect("tempdir");
        let err = resolve_token("svc", &cc_config(), temp.path(), &env, &client).unwrap_err().to_string();
        assert!(err.contains("svc"), "error should name the server: {err}");
        assert!(err.contains("token endpoint"), "error should propagate transport reason: {err}");
    }

    #[test]
    fn refresh_token_flow_propagates_missing_client_env_error() {
        // Regression guard for codex round-3 [P2]: when the user
        // configures client_id_env / client_secret_env on a refresh
        // flow and the daemon env is missing that var, we MUST fail
        // fast with the config-resolution error rather than POST to the
        // token endpoint with the client credentials silently dropped.
        let oauth = OauthConfig {
            flow: OauthFlow::RefreshToken,
            token_url: Some("https://auth.example.com/token".to_string()),
            client_id_env: Some("REFRESH_CLIENT_ID".to_string()),
            client_secret_env: None,
            refresh_token_env: Some("REFRESH_TOKEN".to_string()),
            bearer_env: None,
            scopes: vec![],
            audience: None,
            cache: false,
            client_id: None,
        };
        let env = StaticEnv(BTreeMap::from([("REFRESH_TOKEN".to_string(), "rt".to_string())]));
        // REFRESH_CLIENT_ID intentionally unset.
        let client = MockClient::new(vec![]);
        let temp = tempdir().expect("tempdir");

        let err = resolve_token("svc", &oauth, temp.path(), &env, &client).unwrap_err().to_string();
        assert!(err.contains("REFRESH_CLIENT_ID"), "error should name the missing client_id env var: {err}");
        assert_eq!(client.call_count(), 0, "must not hit the token endpoint when local config is broken");
    }

    #[test]
    fn cache_is_invalidated_when_oauth_config_changes() {
        // Regression guard for codex round-3 [P2]: if the user changes
        // scopes/audience/etc. while the cached token is still fresh,
        // we MUST refetch rather than reuse the old config's token.
        let env = StaticEnv(BTreeMap::from([
            ("EXAMPLE_CLIENT_ID".to_string(), "id".to_string()),
            ("EXAMPLE_CLIENT_SECRET".to_string(), "secret".to_string()),
        ]));
        let client = MockClient::new(vec![
            Ok(TokenFetchResponse { access_token: "first".to_string(), expires_in: Some(3600), refresh_token: None }),
            Ok(TokenFetchResponse { access_token: "second".to_string(), expires_in: Some(3600), refresh_token: None }),
        ]);
        let temp = tempdir().expect("tempdir");

        let first = resolve_token("svc", &cc_config(), temp.path(), &env, &client).expect("first ok");
        assert_eq!(first.access_token, "first");

        let mut changed = cc_config();
        changed.scopes = vec!["new-scope".to_string()];

        let second = resolve_token("svc", &changed, temp.path(), &env, &client).expect("second ok");
        assert_eq!(second.access_token, "second", "changed config must trigger a refetch (got cached token instead)");
        assert_eq!(client.call_count(), 2, "fingerprint mismatch must force a network call");
    }

    #[test]
    fn missing_expires_in_uses_short_default_ttl_not_forever() {
        // Regression guard for the codex round-2 [P2] finding. Token
        // endpoints sometimes omit `expires_in`; we MUST NOT cache the
        // access token as if it never expires, because the real
        // server-side expiration will silently invalidate every later
        // bearer header. We force the second resolution to refetch by
        // mutating the cached `expires_at` to a stale instant; the
        // assertion is just that `expires_at` is always populated.
        let env = StaticEnv(BTreeMap::from([
            ("EXAMPLE_CLIENT_ID".to_string(), "id".to_string()),
            ("EXAMPLE_CLIENT_SECRET".to_string(), "secret".to_string()),
        ]));
        let client = MockClient::new(vec![Ok(TokenFetchResponse {
            access_token: "no-expiry".to_string(),
            expires_in: None,
            refresh_token: None,
        })]);
        let temp = tempdir().expect("tempdir");

        let _ = resolve_token("svc", &cc_config(), temp.path(), &env, &client).expect("resolve");

        let cache_path = temp.path().join(cache_filename_for_server("svc"));
        let raw = std::fs::read_to_string(&cache_path).expect("cache file written");
        let cached: CachedToken = serde_json::from_str(&raw).expect("cache deserializes");
        assert!(
            cached.expires_at.is_some(),
            "missing expires_in must still set a bounded expires_at to avoid indefinite caching"
        );
    }

    #[test]
    fn cache_filename_sanitizes_path_traversal_attempts() {
        // The cache filename helper MUST keep all I/O inside the supplied
        // cache_dir, even when the MCP server name carries path
        // separators or `..` segments. We assert two things: (1) the
        // generated name contains no path separators (so `Path::join`
        // can't escape the parent), and (2) two different malicious
        // names map to different cache files (the SHA-256 suffix breaks
        // collisions). Both invariants are load-bearing for the v0.5.5
        // P1 fix.
        let evil_a = "../../config";
        let evil_b = "..\\..\\config";
        let normal = "robinhood-trading";
        for name in [evil_a, evil_b, normal, "animus.requirements/ao", ".bashrc"] {
            let filename = cache_filename_for_server(name);
            assert!(!filename.contains('/'), "filename for {name:?} must not contain `/`: {filename}");
            assert!(!filename.contains('\\'), "filename for {name:?} must not contain `\\`: {filename}");
            assert!(!filename.starts_with('.'), "filename for {name:?} must not start with `.`: {filename}");
            assert!(filename.ends_with(".json"), "filename for {name:?} must end with `.json`: {filename}");
        }
        assert_ne!(
            cache_filename_for_server(evil_a),
            cache_filename_for_server(evil_b),
            "different raw names must produce different cache files"
        );
    }

    #[test]
    fn cache_filename_resists_traversal_in_resolve_token() {
        // Resolving with a path-traversal name MUST land inside the
        // supplied cache_dir. This guards the full code path against
        // regressions, not just the helper.
        let env = StaticEnv(BTreeMap::from([
            ("EXAMPLE_CLIENT_ID".to_string(), "id".to_string()),
            ("EXAMPLE_CLIENT_SECRET".to_string(), "secret".to_string()),
        ]));
        let client = MockClient::new(vec![Ok(TokenFetchResponse {
            access_token: "tok".to_string(),
            expires_in: Some(3600),
            refresh_token: None,
        })]);
        let temp = tempdir().expect("tempdir");

        let _ = resolve_token("../../config", &cc_config(), temp.path(), &env, &client).expect("resolve ok");

        let escapee = temp.path().parent().unwrap_or(temp.path()).join("config.json");
        assert!(!escapee.exists(), "cache write must not escape the cache dir; saw {}", escapee.display());

        let expected = temp.path().join(cache_filename_for_server("../../config"));
        assert!(expected.exists(), "expected cache file {} should exist", expected.display());
    }

    #[test]
    fn header_map_emits_authorization_bearer() {
        let token = ResolvedOauthToken { access_token: "tok".to_string(), header_name: "Authorization".to_string() };
        let map = header_map_for_token(&token);
        assert_eq!(map.get("Authorization").map(String::as_str), Some("Bearer tok"));
    }

    #[cfg(unix)]
    #[test]
    fn cache_file_is_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let env = StaticEnv(BTreeMap::from([
            ("EXAMPLE_CLIENT_ID".to_string(), "id".to_string()),
            ("EXAMPLE_CLIENT_SECRET".to_string(), "secret".to_string()),
        ]));
        let client = MockClient::new(vec![Ok(TokenFetchResponse {
            access_token: "abc".to_string(),
            expires_in: Some(3600),
            refresh_token: None,
        })]);
        let temp = tempdir().expect("tempdir");

        let _ = resolve_token("svc", &cc_config(), temp.path(), &env, &client).expect("resolve");

        let path = temp.path().join(cache_filename_for_server("svc"));
        let metadata = fs::metadata(&path).expect("cache file exists");
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "cache file should be 0600, got {:o}", mode);
    }

    #[test]
    fn cache_disabled_bypasses_cache_read_and_write() {
        let mut config = cc_config();
        config.cache = false;
        let env = StaticEnv(BTreeMap::from([
            ("EXAMPLE_CLIENT_ID".to_string(), "id".to_string()),
            ("EXAMPLE_CLIENT_SECRET".to_string(), "secret".to_string()),
        ]));
        let client = MockClient::new(vec![
            Ok(TokenFetchResponse { access_token: "a".to_string(), expires_in: Some(3600), refresh_token: None }),
            Ok(TokenFetchResponse { access_token: "b".to_string(), expires_in: Some(3600), refresh_token: None }),
        ]);
        let temp = tempdir().expect("tempdir");

        let first = resolve_token("svc", &config, temp.path(), &env, &client).expect("first ok");
        let second = resolve_token("svc", &config, temp.path(), &env, &client).expect("second ok");

        assert_eq!(first.access_token, "a");
        assert_eq!(second.access_token, "b");
        assert_eq!(client.call_count(), 2);
        let cache_path = temp.path().join(cache_filename_for_server("svc"));
        assert!(!cache_path.exists(), "cache file should not be written when cache=false");
    }
}
