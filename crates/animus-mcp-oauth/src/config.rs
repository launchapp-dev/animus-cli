//! Resolution helpers shared by the auth flow, the proxy, and the CLI:
//! keychain store construction, MCP-server URL resolution, and the RBAC
//! principal that namespaces stored tokens.

use std::path::Path;
use std::sync::Arc;

use orchestrator_config::workflow_config::{
    load_workflow_config_or_default, load_workflow_config_with_metadata, McpServerDefinition, OauthFlow,
};
use orchestrator_core::secret_store::KeyringSecretStore;
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
}

/// Build a keychain-backed [`SecretStore`] for `project_root`, mirroring the
/// `animus secret` surface so OAuth tokens share the project's keychain
/// scope.
pub fn build_secret_store(project_root: &Path) -> Result<Arc<dyn SecretStore>, ServerResolutionError> {
    let scoped_root = scoped_state_root(project_root)
        .ok_or_else(|| ServerResolutionError::NoScopedRoot(project_root.display().to_string()))?;
    let scope = resolve_keychain_scope(project_root, &scoped_root);
    Ok(Arc::new(KeyringSecretStore::new(&scope, scoped_root)))
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
    let loaded = match load_workflow_config_with_metadata(project_root) {
        Ok(loaded) => loaded,
        Err(err) if workflow_yaml_present(project_root) => {
            return Err(ServerResolutionError::WorkflowConfig(err.to_string()));
        }
        Err(_) => load_workflow_config_or_default(project_root),
    };
    if let Some(def) = loaded.config.mcp_servers.get(server) {
        return finalize(server, url_override, def.url.clone(), authorization_code_fields(def));
    }

    // Project config mcp_servers next.
    let project_config = protocol::Config::load_or_default(&project_root.display().to_string())
        .map_err(|err| ServerResolutionError::ProjectConfig(err.to_string()))?;
    if let Some(entry) = project_config.mcp_servers.get(server) {
        let oauth_fields = entry.oauth.as_ref().and_then(|value| {
            serde_json::from_value::<orchestrator_config::OauthConfig>(value.clone())
                .ok()
                .filter(|cfg| cfg.flow == OauthFlow::AuthorizationCode)
                .map(|cfg| (cfg.scopes, cfg.client_id))
        });
        return finalize(server, url_override, entry.url.clone(), oauth_fields);
    }

    // Not in config: only proceed if the user passed an explicit URL.
    match url_override {
        Some(url) => Ok(ServerResolution {
            url: url.to_string(),
            scopes: Vec::new(),
            client_id: None,
            is_authorization_code: true,
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

type AuthCodeFields = (Vec<String>, Option<String>);

fn authorization_code_fields(def: &McpServerDefinition) -> Option<AuthCodeFields> {
    def.oauth
        .as_ref()
        .filter(|cfg| cfg.flow == OauthFlow::AuthorizationCode)
        .map(|cfg| (cfg.scopes.clone(), cfg.client_id.clone()))
}

fn finalize(
    server: &str,
    url_override: Option<&str>,
    config_url: Option<String>,
    oauth_fields: Option<AuthCodeFields>,
) -> Result<ServerResolution, ServerResolutionError> {
    let url = url_override
        .map(str::to_string)
        .or(config_url)
        .ok_or_else(|| ServerResolutionError::MissingUrl(server.to_string()))?;
    let (scopes, client_id) = oauth_fields.clone().unwrap_or_default();
    Ok(ServerResolution { url, scopes, client_id, is_authorization_code: oauth_fields.is_some() })
}
