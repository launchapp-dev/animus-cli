//! Resolution helpers shared by the auth flow, the proxy, and the CLI:
//! keychain store construction, MCP-server URL resolution, and the RBAC
//! principal that namespaces stored tokens.

use std::path::Path;
use std::sync::Arc;

use orchestrator_config::workflow_config::{
    try_load_workflow_config, OauthConfig, OauthFlow, WorkflowConfigAvailability,
};
use orchestrator_core::SecretStore;
use protocol::repository_scope::{repository_scope_for_path, scoped_state_root};
use thiserror::Error;

/// Errors surfaced when resolving an MCP server's OAuth configuration.
#[derive(Debug, Error)]
pub enum ServerResolutionError {
    #[error("could not resolve scoped state root for project at {0}")]
    NoScopedRoot(String),
    #[error("MCP server `{0}` is not defined in workflow or project config; pass --url to authenticate it explicitly")]
    UnknownServer(String),
    #[error("MCP server `{0}` has no `url`; an HTTP-transport URL is required for OAuth")]
    MissingUrl(String),
    /// The `config_source` failed to load (spawn / RPC / DB overload / validation),
    /// so the server's config could NOT be determined. This is distinct from
    /// [`UnknownServer`], which fires only when the config loaded and the server
    /// is genuinely absent. Callers should retry / report a transient source
    /// error rather than "server not configured".
    #[error("MCP server `{0}` could not be resolved: the workflow config source failed to load ({1}). This is a config-source error, not a missing-server error — retry shortly or check the `config_source` plugin.")]
    ConfigSourceUnavailable(String, String),
    #[error("failed to load workflow config: {0}")]
    WorkflowConfig(String),
    #[error("failed to load project config: {0}")]
    ProjectConfig(String),
}

/// Outcome of resolving a server's OAuth shape from config.
#[derive(Debug, Clone)]
pub struct ServerResolution {
    /// Upstream HTTP MCP endpoint. This is the `AuthorizationManager` base
    /// URL, which is BOTH the OAuth `resource` indicator (RFC 8707) and the
    /// discovery seed (RFC 9728 protected-resource metadata) in rmcp 1.7.
    pub url: String,
    /// Requested scopes from the `authorization_code` oauth block, if any.
    pub scopes: Vec<String>,
    /// Pre-registered client id, if the config pins one (skips DCR).
    pub client_id: Option<String>,
    /// True when the resolved server's oauth block is an
    /// `authorization_code` flow (vs absent / a machine-to-machine flow).
    pub is_authorization_code: bool,
    /// The full oauth block when the resolved server uses a
    /// machine-to-machine flow (`manual_bearer` / `client_credentials` /
    /// `refresh_token`). Those servers are served by the proxy's
    /// broker-backed bearer source rather than the keychain
    /// `AuthorizationManager`; `None` for `authorization_code` or no oauth.
    pub broker_oauth: Option<OauthConfig>,
}

/// Build a keychain-backed [`SecretStore`] for `project_root`, mirroring the
/// `animus secret` surface so OAuth tokens share the project's keychain
/// scope.
pub fn build_secret_store(project_root: &Path) -> Result<Arc<dyn SecretStore>, ServerResolutionError> {
    let scoped_root = scoped_state_root(project_root)
        .ok_or_else(|| ServerResolutionError::NoScopedRoot(project_root.display().to_string()))?;
    let scope = resolve_keychain_scope(project_root, &scoped_root);
    Ok(Arc::from(orchestrator_core::build_secret_store(&scope, scoped_root)))
}

/// Pick the keychain service-scope string from the adopted scoped state
/// directory name when present, otherwise fall back to the freshly-derived
/// `repo-scope`. Matches `ops_secret::resolve_keychain_scope` so the same
/// keychain entries are read by both surfaces.
fn resolve_keychain_scope(project_root: &Path, scoped_root: &Path) -> String {
    if let Some(name) = scoped_root.file_name().and_then(|s| s.to_str()) {
        return name.to_string();
    }
    repository_scope_for_path(project_root)
}

/// The RBAC principal id used to namespace stored OAuth tokens.
///
/// v0.5.8 secrets are single-scope; tokens are additionally keyed by a
/// principal so a future multi-user surface can hold distinct tokens per
/// user without a schema change. Today this resolves to the configured
/// default principal from the global `principals.yaml` (or `"local"`).
///
/// `_project_root` is accepted for forward-compatibility with a
/// project-scoped principals file but is not consulted today.
#[must_use]
pub fn resolve_principal_id(_project_root: &Path) -> String {
    use orchestrator_core::{default_principals_path, load_principals_file};
    match load_principals_file(&default_principals_path()) {
        Ok(Some(file)) => file.policy.default_principal.unwrap_or_else(|| "local".to_string()),
        _ => "local".to_string(),
    }
}

/// Resolve a server's URL + OAuth shape from workflow/project config.
///
/// `url_override` (the CLI `--url` flag) wins over config: it lets a user
/// authenticate a server that isn't in config yet, or override a stale URL.
pub fn resolve_server_url(
    project_root: &Path,
    server: &str,
    url_override: Option<&str>,
) -> Result<ServerResolution, ServerResolutionError> {
    // Workflow config mcp_servers first (authoritative for daemon runs).
    //
    // Use the NON-SWALLOWING loader so a transient `config_source` failure (DB
    // overload under bulk `mcp call`) is NOT degraded into an empty config that
    // then misreports the server as "not defined". `try_load_workflow_config`
    // distinguishes three cases:
    //   * Loaded          — use its `mcp_servers`.
    //   * NoSource        — no config_source configured; benign, fall through to
    //                       project config (project-config-only setups).
    //   * SourceUnavailable — the source failed; surface a DISTINCT retryable
    //                       error. We do this EVEN when the caller supplied a
    //                       `--url`: a `--url` overrides only the upstream
    //                       endpoint, not the server's oauth FLOW/block, so
    //                       synthesizing an `authorization_code` resolution from
    //                       the URL alone would misroute a broker-flow server
    //                       (`manual_bearer` / `client_credentials` /
    //                       `refresh_token`) to the keychain path. Erroring lets
    //                       the caller retry once the source recovers.
    let loaded = match try_load_workflow_config(project_root, None) {
        WorkflowConfigAvailability::Loaded(loaded) => Some(loaded),
        WorkflowConfigAvailability::NoSource => None,
        WorkflowConfigAvailability::SourceUnavailable(err) => {
            return Err(ServerResolutionError::ConfigSourceUnavailable(server.to_string(), err.to_string()));
        }
    };
    if let Some(def) = loaded.as_ref().and_then(|loaded| loaded.config.mcp_servers.get(server)) {
        return finalize(server, url_override, def.url.clone(), def.oauth.clone());
    }

    // Project config mcp_servers next.
    let project_config = protocol::Config::load_or_default(&project_root.display().to_string())
        .map_err(|err| ServerResolutionError::ProjectConfig(err.to_string()))?;
    if let Some(entry) = project_config.mcp_servers.get(server) {
        let oauth = entry.oauth.as_ref().and_then(|value| serde_json::from_value::<OauthConfig>(value.clone()).ok());
        return finalize(server, url_override, entry.url.clone(), oauth);
    }

    // Not in config: only proceed if the user passed an explicit URL.
    match url_override {
        Some(url) => Ok(ServerResolution {
            url: url.to_string(),
            scopes: Vec::new(),
            client_id: None,
            is_authorization_code: true,
            broker_oauth: None,
        }),
        None => Err(ServerResolutionError::UnknownServer(server.to_string())),
    }
}

fn finalize(
    server: &str,
    url_override: Option<&str>,
    config_url: Option<String>,
    oauth: Option<OauthConfig>,
) -> Result<ServerResolution, ServerResolutionError> {
    let url = url_override
        .map(str::to_string)
        .or(config_url)
        .ok_or_else(|| ServerResolutionError::MissingUrl(server.to_string()))?;
    match oauth {
        Some(cfg) if cfg.flow == OauthFlow::AuthorizationCode => Ok(ServerResolution {
            url,
            scopes: cfg.scopes,
            client_id: cfg.client_id,
            is_authorization_code: true,
            broker_oauth: None,
        }),
        Some(cfg) => Ok(ServerResolution {
            url,
            scopes: Vec::new(),
            client_id: None,
            is_authorization_code: false,
            broker_oauth: Some(cfg),
        }),
        None => Ok(ServerResolution {
            url,
            scopes: Vec::new(),
            client_id: None,
            is_authorization_code: false,
            broker_oauth: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_config::workflow_config::builtin_workflow_config;
    use orchestrator_config::workflow_config::config_source_client::test_seam;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    // These tests mutate the process-global config_source test seam + HOME env,
    // so serialize them under one lock held for the whole test body.
    fn serial() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// RAII guard that points `HOME` at a temp dir for the test and RESTORES the
    /// previous value on drop, so the mutation never leaks to sibling tests.
    struct HomeGuard {
        prev: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let prev = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self { prev }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    /// Common fixture: hold the serial lock, isolate `HOME` to a fresh tempdir
    /// (restored on drop). Bound in the caller in declaration order so the
    /// tempdir drops LAST — after `HomeGuard` has restored `HOME`.
    fn fixture() -> (MutexGuard<'static, ()>, tempfile::TempDir, HomeGuard) {
        let guard = serial().lock().unwrap_or_else(|p| p.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let home = HomeGuard::set(temp.path());
        (guard, temp, home)
    }

    /// TASK-326 (fix d): a transient config_source failure must surface as the
    /// DISTINCT `ConfigSourceUnavailable`, NOT `UnknownServer` — otherwise the
    /// caller concludes "krisp isn't configured" when the source is merely down.
    #[test]
    fn source_failure_yields_config_source_unavailable_not_unknown_server() {
        let (_guard, temp, _home) = fixture();
        let root = temp.path();

        let _fail = test_seam::install_failure(root, "config_source RPC timed out");
        let err = resolve_server_url(root, "krisp", None).expect_err("must fail when source is down");
        assert!(
            matches!(err, ServerResolutionError::ConfigSourceUnavailable(ref s, _) if s == "krisp"),
            "expected ConfigSourceUnavailable, got {err:?}"
        );
    }

    /// Even with an explicit `--url`, a source outage yields
    /// `ConfigSourceUnavailable` (never a synthesized `authorization_code`
    /// resolution): a `--url` overrides only the upstream endpoint, not the
    /// server's oauth FLOW, so guessing the flow could misroute a broker server
    /// to the keychain path. The caller retries once the source recovers.
    #[test]
    fn explicit_url_still_errors_when_source_unavailable() {
        let (_guard, temp, _home) = fixture();
        let root = temp.path();

        let _fail = test_seam::install_failure(root, "config_source RPC timed out");
        let err = resolve_server_url(root, "krisp", Some("https://example.test/mcp"))
            .expect_err("a source outage must error even with --url, to avoid misrouting a broker server");
        assert!(
            matches!(err, ServerResolutionError::ConfigSourceUnavailable(ref s, _) if s == "krisp"),
            "expected ConfigSourceUnavailable, got {err:?}"
        );
    }

    /// When the config LOADS but the server is genuinely absent, the error is
    /// `UnknownServer` (the pre-existing behavior) — never `ConfigSourceUnavailable`.
    #[test]
    fn loaded_config_missing_server_yields_unknown_server() {
        let (_guard, temp, _home) = fixture();
        let root = temp.path();

        // A base with no mcp_servers loads cleanly; the server is simply absent.
        let _base = test_seam::install(root, builtin_workflow_config());
        let err = resolve_server_url(root, "krisp", None).expect_err("absent server must error");
        assert!(
            matches!(err, ServerResolutionError::UnknownServer(ref s) if s == "krisp"),
            "expected UnknownServer, got {err:?}"
        );
    }
}
