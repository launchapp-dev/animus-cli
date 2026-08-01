//! Resolution helpers shared by the auth flow, the proxy, and the CLI:
//! keychain store construction, MCP-server URL resolution, and the RBAC
//! principal that namespaces stored tokens.

use std::path::Path;
use std::sync::Arc;

use orchestrator_config::workflow_config::{
    load_workflow_config_or_default, load_workflow_config_with_metadata, OauthConfig, OauthFlow,
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

/// Build the configured [`SecretStore`] for `project_root`, mirroring the
/// `animus secret` surface so OAuth tokens share the project's secret-store
/// scope.
///
/// Consults both the global `~/.animus/config.json` and the project-level
/// `.animus/config.json` for the `secrets` configuration block so that
/// per-project key-source overrides (e.g. `key_source = user-key`) are
/// honored by `mcp auth --complete` and every other OAuth code path.
pub fn build_secret_store(project_root: &Path) -> Result<Arc<dyn SecretStore>, ServerResolutionError> {
    let scoped_root = scoped_state_root(project_root)
        .ok_or_else(|| ServerResolutionError::NoScopedRoot(project_root.display().to_string()))?;
    Ok(build_secret_store_at(project_root, scoped_root))
}

fn build_secret_store_at(project_root: &Path, scoped_root: impl Into<std::path::PathBuf>) -> Arc<dyn SecretStore> {
    let scoped_root = scoped_root.into();
    let scope = resolve_keychain_scope(project_root, &scoped_root);
    Arc::from(orchestrator_core::build_secret_store_for_project(&scope, scoped_root, project_root))
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
    // A malformed `.animus/workflows.yaml` must surface its YAML/validation
    // error rather than silently falling back to the builtin default (which
    // would mislead the user into "unknown server"). But an *absent* workflow
    // config is normal (project-config-only setups), and
    // `load_workflow_config_with_metadata` returns a "missing" error in that
    // case. So: only propagate the load error when a workflow YAML source
    // actually exists; otherwise fall back to the builtin default and continue
    // to project config.
    let loaded = match load_workflow_config_with_metadata(project_root, None) {
        Ok(loaded) => loaded,
        Err(err) if workflow_yaml_present(project_root) => {
            return Err(ServerResolutionError::WorkflowConfig(err.to_string()));
        }
        Err(_) => load_workflow_config_or_default(project_root),
    };
    if let Some(def) = loaded.config.mcp_servers.get(server) {
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

/// True when a workflow YAML source exists at `.animus/workflows.yaml` or
/// any `.animus/workflows/*.yaml`. Used to decide whether a workflow-config
/// load failure is a real (malformed-config) error worth propagating vs the
/// benign "no workflow config" case.
fn workflow_yaml_present(project_root: &Path) -> bool {
    let animus = project_root.join(".animus");
    if animus.join("workflows.yaml").exists() {
        return true;
    }
    let dir = animus.join("workflows");
    std::fs::read_dir(&dir)
        .map(|entries| entries.flatten().any(|e| e.path().extension().is_some_and(|ext| ext == "yaml" || ext == "yml")))
        .unwrap_or(false)
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
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        name: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn remove(name: &'static str) -> Self {
            let previous = std::env::var(name).ok();
            std::env::remove_var(name);
            Self { name, previous }
        }

        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    fn assert_oauth_store_uses_project_key_file(
        key_source: Option<&str>,
        backend: Option<&str>,
        empty_env_key: bool,
    ) {
        // Secret-key environment variables are process-global. Keep removal,
        // store construction, and restoration in one serialized window so
        // these tests cannot borrow or overwrite another test's key.
        let _env_guard = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().join("project");
        let animus_dir = project_root.join(".animus");
        std::fs::create_dir_all(&animus_dir).unwrap();

        let key_file = tmp.path().join("server.key");
        std::fs::write(&key_file, "a5".repeat(32)).unwrap();
        let config = serde_json::json!({
            "secrets": {
                "backend": backend,
                "key_source": key_source,
                "key_file": key_file
            }
        });
        std::fs::write(animus_dir.join("config.json"), serde_json::to_vec(&config).unwrap()).unwrap();

        let _key_guard = if empty_env_key {
            EnvVarGuard::set("ANIMUS_SECRET_KEY", "  ")
        } else {
            EnvVarGuard::remove("ANIMUS_SECRET_KEY")
        };
        let store = build_secret_store_at(&project_root, tmp.path().join("state"));
        assert_eq!(
            store.backend_label(),
            "device-encrypted store",
            "a project key_file must select the headless-safe device backend"
        );
        store.set("oauth_test", "token")
            .expect("OAuth secret write must use the project-configured key file");
        drop(store);

        // Rebuild the store as the separate `mcp auth --complete` invocation
        // does. This verifies that config is consulted on every path, not just
        // that one in-memory store can read back its own write.
        let reopened = build_secret_store_at(&project_root, tmp.path().join("state"));
        assert_eq!(
            reopened.backend_label(),
            "device-encrypted store",
            "OAuth completion must reselect the project-configured device backend"
        );
        let stored = reopened.get("oauth_test");

        assert_eq!(
            stored.expect("OAuth secret operations must use the project-configured key file").as_deref(),
            Some("token")
        );
    }

    #[test]
    fn oauth_secret_store_honors_project_user_key_without_env_key() {
        assert_oauth_store_uses_project_key_file(Some("user-key"), Some("device"), false);
    }

    #[test]
    fn oauth_secret_store_user_key_ignores_empty_env_key() {
        assert_oauth_store_uses_project_key_file(Some("user-key"), Some("device"), true);
    }

    #[test]
    fn oauth_secret_store_auto_backend_honors_project_user_key_without_env_key() {
        assert_oauth_store_uses_project_key_file(Some("user-key"), Some("auto"), false);
    }

    #[test]
    fn oauth_secret_store_default_backend_honors_project_user_key_without_env_key() {
        assert_oauth_store_uses_project_key_file(Some("user-key"), None, false);
    }

    #[test]
    fn oauth_secret_store_auto_uses_project_key_file_without_env_key() {
        assert_oauth_store_uses_project_key_file(Some("auto"), Some("auto"), false);
    }

    #[test]
    fn oauth_secret_store_auto_ignores_empty_env_key_when_project_key_file_is_configured() {
        assert_oauth_store_uses_project_key_file(Some("auto"), Some("auto"), true);
    }

    #[test]
    fn oauth_secret_store_device_backend_auto_source_uses_project_key_file_without_env_key() {
        assert_oauth_store_uses_project_key_file(Some("auto"), Some("device"), false);
    }

    #[test]
    fn oauth_secret_store_device_backend_default_source_uses_project_key_file_without_env_key() {
        assert_oauth_store_uses_project_key_file(None, Some("device"), false);
    }

    #[test]
    fn oauth_secret_store_default_auto_uses_project_key_file_without_env_key() {
        assert_oauth_store_uses_project_key_file(None, None, false);
    }
}
