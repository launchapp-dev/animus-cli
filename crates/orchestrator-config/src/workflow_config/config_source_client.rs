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
    discover_by_kind(project_root.to_path_buf(), CONFIG_SOURCE_KIND).map(|plugins| !plugins.is_empty()).unwrap_or(false)
}

/// Resolve a `config_source` plugin (if installed) and load the base config.
/// `Ok(None)` => no plugin installed and (in tests) no injected base; the
/// caller surfaces an actionable "no config_source plugin installed" error.
/// Returns `(base WorkflowConfig, cache_token_version)`.
pub fn resolve_plugin_base(project_root: &Path) -> Result<Option<(WorkflowConfig, String)>> {
    // Test-only seam: a synthetic base config injected via
    // `set_test_plugin_base` stands in for an installed config_source plugin so
    // unit tests can exercise the kernel's pack-merge + validate pipeline (which
    // it still owns) without spawning a real plugin process.
    #[cfg(any(test, feature = "test-utils"))]
    if let Some(base) = test_seam::base_for(project_root) {
        return Ok(Some((base, "test-seam".to_string())));
    }

    let plugins = discover_by_kind(project_root.to_path_buf(), CONFIG_SOURCE_KIND)
        .with_context(|| format!("discovering config_source plugins for {}", project_root.display()))?;
    let Some(plugin) = plugins.into_iter().next() else {
        return Ok(None);
    };
    let loaded = run_blocking(load_from_plugin(plugin, project_root.to_path_buf()))?;
    Ok(Some(loaded?))
}

async fn load_from_plugin(plugin: DiscoveredPlugin, project_root: PathBuf) -> Result<(WorkflowConfig, String)> {
    // The host spawns plugins with a CLEAN env. Unlike other plugin roles, a
    // config_source plugin REPLACES the kernel's in-process YAML parsing, which
    // read the daemon's full process environment for non-secret `${VAR}`
    // interpolation (team ids, URLs, feature flags). To preserve that behavior
    // we forward the full parent env to the config_source plugin, in addition
    // to the manifest-declared secret env (e.g. DATABASE_URL). config_source is
    // a trusted, required-role plugin in the config pipeline, so this matches
    // the capability the kernel itself had — secret-backed `${secret.*}` still
    // resolves via the plugin's own keychain resolver, repo-scope aware.
    let forwarded_env: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
    let options =
        PluginSpawnOptions::for_manifest(plugin.name.clone(), &plugin.manifest.env_required, forwarded_env, None);
    let host = PluginHost::spawn_with_options(&plugin.path, &[], options)
        .await
        .with_context(|| format!("spawning config_source plugin {}", plugin.name))?;
    host.handshake().await.with_context(|| format!("handshake with config_source plugin {}", plugin.name))?;

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

/// Test-only seam that lets unit tests inject a synthetic base
/// [`WorkflowConfig`], standing in for an installed `config_source` plugin so
/// the kernel's pack-merge + validate pipeline (which it still owns) can be
/// exercised without spawning a plugin process. Scoped per-thread; the returned
/// guard clears the override on drop so tests stay isolated.
#[cfg(any(test, feature = "test-utils"))]
pub mod test_seam {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    use super::WorkflowConfig;

    // Process-global registry keyed by project root, so the seam is safe under
    // cargo's parallel test execution (daemon/runtime tests load config on
    // worker threads, and many tests run concurrently against distinct temp
    // project roots). Keying by root means a test only ever sees the base it
    // installed for its own project.
    fn registry() -> &'static Mutex<HashMap<PathBuf, WorkflowConfig>> {
        static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, WorkflowConfig>>> = OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
    }

    // Normalize the key so a test that installs against `tempdir().path()` is
    // still found when the loader resolves the project root via git-common-root
    // (which canonicalizes, e.g. /var -> /private/var on macOS). Falls back to
    // the raw path when canonicalization fails (path may not exist yet).
    fn normalize(project_root: &Path) -> PathBuf {
        std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf())
    }

    /// Install a synthetic base config for `project_root`. The override is
    /// active until the returned guard is dropped.
    #[must_use]
    pub fn install(project_root: &Path, base: WorkflowConfig) -> TestBaseGuard {
        let key = normalize(project_root);
        registry().lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone(), base);
        TestBaseGuard { key }
    }

    /// Clone the synthetic base installed for `project_root`, if any.
    pub fn base_for(project_root: &Path) -> Option<WorkflowConfig> {
        registry().lock().unwrap_or_else(|p| p.into_inner()).get(&normalize(project_root)).cloned()
    }

    /// RAII guard that removes the installed base for its project root on drop.
    pub struct TestBaseGuard {
        key: PathBuf,
    }

    impl Drop for TestBaseGuard {
        fn drop(&mut self) {
            registry().lock().unwrap_or_else(|p| p.into_inner()).remove(&self.key);
        }
    }
}

/// v0.6 cross-crate test seam: stand in for an installed `config_source` plugin
/// by compiling the project's `.animus/*.yaml` into a base [`WorkflowConfig`]
/// (the same `compile_yaml_workflow_files` library call the `animus-config-yaml`
/// plugin makes) and injecting it as the plugin-sourced base. When the project
/// has no YAML sources, the builtin base is injected instead — a real
/// config_source plugin would still produce *some* canonical base. The kernel's
/// `load_workflow_config*` / `load_agent_runtime_config*` paths then merge pack
/// overlays onto it and validate, exactly as with a real plugin installed.
///
/// Returns a guard that clears the injected base on drop; hold it for the
/// duration of the load call. Gated behind the `test-utils` feature so it is
/// available to dependent crates' test builds but never compiled into release.
#[cfg(any(test, feature = "test-utils"))]
pub fn install_yaml_config_source_base(project_root: &Path) -> test_seam::TestBaseGuard {
    // Mirror what `animus-config-yaml` does on first contact with a project:
    // ensure the `.animus/workflows.yaml` scaffold exists, then compile it. This
    // matches the historical kernel behavior where `FileServiceHub::new` wrote
    // the scaffold and the in-tree loader compiled it, so tests that create a
    // hub (which still scaffolds) and rely on the standard workflow keep working.
    let _ = super::ensure_workflow_yaml_scaffold(project_root);
    let base = super::compile_yaml_workflow_files(project_root)
        .expect("compile project yaml base")
        .unwrap_or_else(super::builtin_workflow_config);
    test_seam::install(project_root, base)
}
