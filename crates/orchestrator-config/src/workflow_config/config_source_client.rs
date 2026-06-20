//! v0.6: resolve an installed `config_source` plugin and load the base
//! `WorkflowConfig` from it. When no such plugin is installed this returns
//! `Ok(None)` and the caller falls back to the in-tree YAML acquisition (the
//! default), so existing `.animus/workflows/*.yaml` projects are unaffected.
//!
//! This is the seam that lets a deployment source its workflow/agent config
//! from anywhere (Postgres, an API) instead of YAML files. The kernel still
//! owns the compiler (pack overlays + validate + cache + state-machine
//! derivation) — the plugin only produces the canonical base.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use orchestrator_plugin_host::{discover_by_kind, DiscoveredPlugin, PluginHost, PluginSpawnOptions};

use super::types::WorkflowConfig;

const CONFIG_SOURCE_KIND: &str = "config_source";
const CONFIG_LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// True if a `config_source` plugin is installed (cheap discovery, no spawn).
/// Used by callers that early-return when there's nothing to compile.
pub fn config_source_installed(project_root: &Path) -> bool {
    discover_by_kind(project_root.to_path_buf(), CONFIG_SOURCE_KIND)
        .map(|plugins| !plugins.is_empty())
        .unwrap_or(false)
}

/// Resolve a `config_source` plugin (if installed) and load the base config.
/// `Ok(None)` => no plugin installed; the caller uses the in-tree YAML path.
/// Returns `(base WorkflowConfig, cache_token_version)`.
pub fn resolve_plugin_base(project_root: &Path) -> Result<Option<(WorkflowConfig, String)>> {
    let plugins = discover_by_kind(project_root.to_path_buf(), CONFIG_SOURCE_KIND)
        .with_context(|| format!("discovering config_source plugins for {}", project_root.display()))?;
    let Some(plugin) = plugins.into_iter().next() else {
        return Ok(None);
    };
    let loaded = run_blocking(load_from_plugin(plugin, project_root.to_path_buf()))?;
    Ok(Some(loaded?))
}

async fn load_from_plugin(plugin: DiscoveredPlugin, project_root: PathBuf) -> Result<(WorkflowConfig, String)> {
    // The host spawns plugins with a CLEAN env; forward the plugin's
    // manifest-declared env (e.g. DATABASE_URL for animus-config-postgres).
    let options = PluginSpawnOptions::for_manifest(
        plugin.name.clone(),
        &plugin.manifest.env_required,
        Vec::<String>::new(),
        None,
    );
    let host = PluginHost::spawn_with_options(&plugin.path, &[], options)
        .await
        .with_context(|| format!("spawning config_source plugin {}", plugin.name))?;
    host.handshake()
        .await
        .with_context(|| format!("handshake with config_source plugin {}", plugin.name))?;

    let params = serde_json::json!({
        "project_root": project_root,
        "repo_scope": serde_json::Value::Null,
    });
    let value = host
        .request_typed_with_timeout("config/load", Some(params), CONFIG_LOAD_TIMEOUT)
        .await
        .with_context(|| format!("config/load on config_source plugin {}", plugin.name))?;

    let resp: animus_config_protocol::ConfigLoadResponse =
        serde_json::from_value(value).context("decoding ConfigLoadResponse")?;
    if resp.config.schema != animus_config_protocol::CONFIG_MODEL_SCHEMA_ID {
        return Err(anyhow!(
            "config_source plugin {} returned unexpected config schema '{}' (expected '{}')",
            plugin.name,
            resp.config.schema,
            animus_config_protocol::CONFIG_MODEL_SCHEMA_ID
        ));
    }
    let config: WorkflowConfig = serde_json::from_value(resp.config.config)
        .with_context(|| format!("deserializing {}'s config into WorkflowConfig", plugin.name))?;
    Ok((config, resp.cache_token.version))
}

/// Bridge an async future into the sync config-load path. Works whether or not
/// a tokio runtime is already running (daemon = inside a runtime; CLI = none).
fn run_blocking<F: Future>(fut: F) -> Result<F::Output> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => Ok(tokio::task::block_in_place(|| handle.block_on(fut))),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("building tokio runtime for config_source load")?;
            Ok(rt.block_on(fut))
        }
    }
}
