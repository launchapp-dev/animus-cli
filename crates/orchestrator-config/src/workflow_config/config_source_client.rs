//! v0.6: resolve an installed `config_source` plugin and load the base
//! `WorkflowConfig` from it. When no such plugin is installed this returns
//! `Ok(None)` and the caller falls back to the in-tree YAML acquisition (the
//! default), so existing `.animus/workflows/*.yaml` projects are unaffected.
//!
//! This is the seam that lets a deployment source its workflow/agent config
//! from anywhere (Postgres, an API) instead of YAML files. The kernel still
//! owns the compiler (pack overlays + validate + cache + state-machine
//! derivation) — the plugin only produces the canonical base.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use animus_actor::Actor;
use anyhow::{anyhow, Context, Result};
use orchestrator_plugin_host::session::plugin_supervisor::{classify, RetryDecision};
use orchestrator_plugin_host::{discover_by_kind, DiscoveredPlugin, HostError, PluginHost, PluginSpawnOptions};

use super::types::{LoadedWorkflowConfig, WorkflowConfig};

const CONFIG_SOURCE_KIND: &str = "config_source";
const CONFIG_LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const CONFIG_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// One resident `config_source` plugin host, kept warm across `config/load`
/// and `config/write` calls for a single project root.
///
/// v0.6.6 replaces the v0.6.3 spawn+reap-per-call model (one fork on every
/// scheduler-loop config load, ~50-60/min) with EXACTLY ONE host per root,
/// reused for the life of the process. The host is reaped only when it dies
/// (re-spawn) or on explicit teardown via [`shutdown_resident_hosts`].
struct ResidentHost {
    host: PluginHost,
    /// Path the host was spawned from. A re-spawn re-discovers, so this lets a
    /// caller confirm the cached host still matches the installed plugin.
    plugin_path: PathBuf,
    /// Binary mtime (nanos) at spawn time. If the plugin is upgraded/replaced
    /// in place (same path, new bytes) while the daemon runs, the mtime changes
    /// and the cached host is dropped + re-spawned so loads/writes never keep
    /// using the stale binary/capabilities until a daemon restart.
    binary_mtime_nanos: u128,
    /// Monotonic id assigned at insert. Lets a death-like retry reap ONLY the
    /// exact host that failed: a concurrent caller may have already replaced the
    /// dead host with a fresh one, and we must not shut down that healthy
    /// replacement.
    generation: u64,
}

/// Source of monotonic [`ResidentHost::generation`] ids.
fn next_generation() -> u64 {
    static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Best-effort binary mtime (nanos since epoch); `0` when unavailable. Used only
/// as a resident-host invalidation signal (an unreadable mtime collides to `0`,
/// and a later successful read differs and forces a re-spawn).
fn binary_mtime_nanos(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Process-global resident-host cache keyed by project root. The daemon is one
/// root; the CLI may target several, so a map (not a single slot) is required.
fn resident_hosts() -> &'static Mutex<HashMap<PathBuf, ResidentHost>> {
    static HOSTS: OnceLock<Mutex<HashMap<PathBuf, ResidentHost>>> = OnceLock::new();
    HOSTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Compiled-config cache map: `(normalized project root, actor partition)` =>
/// `(CacheToken version, compiled config)`. The actor partition (see
/// [`actor_cache_key`]) keeps one user's compiled config from being served to
/// another.
type CompiledCacheMap = HashMap<(PathBuf, String), (String, LoadedWorkflowConfig)>;

/// Last compiled config served for a `(root, actor)` pair, keyed by the plugin's
/// CacheToken version. A `config/load` whose token matches `version` can skip the
/// whole pack-overlay merge + validate compile and reuse `compiled`. Invalidated
/// on `config/write` and on host re-spawn so a real config change always
/// recompiles. Lives here (not in `loading.rs`) so write + load share one
/// invalidation point.
///
/// SECURITY: the key includes an [`actor_cache_key`] partition so one user's
/// compiled config can never be served to another. A `config_source` plugin may
/// return per-user config (the actor reaches it via the `config/load` params),
/// so a root-only key would leak user A's private config to user B on a cache
/// hit. `None` (no authenticated actor) maps to the shared `__global__`
/// partition.
fn compiled_cache() -> &'static Mutex<CompiledCacheMap> {
    static CACHE: OnceLock<Mutex<CompiledCacheMap>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Stable cache-partition key derived from the actor identity. Two actors with
/// the same `user_id`, claim set, and tenant collide (correct — they see the
/// same scoped config); any difference partitions them. Claims are sorted so the
/// key is order-independent. `None` => the shared `__global__` partition.
///
/// SECURITY: the encoding MUST be unambiguous — a delimiter-joined string (e.g.
/// `user_id|claims|tenant`) lets distinct identities collide when a field
/// contains the delimiter (`user_id="a|b"`+claim `c` vs `user_id="a"`+claim
/// `b|c`), which would serve one actor another's cached config. JSON of the
/// sorted fields is self-delimiting (strings are escaped), so no field value can
/// forge another identity's key. `__global__` cannot collide with a JSON object
/// (which always starts with `{`).
pub(crate) fn actor_cache_key(actor: Option<&Actor>) -> String {
    match actor {
        None => "__global__".to_string(),
        Some(actor) => {
            let mut claims = actor.claims.clone();
            claims.sort();
            // serde_json::to_string on owned strings/Vec<String>/Option<String>
            // is infallible in practice; fall back to a Debug encoding (still
            // unambiguous) on the impossible error rather than panicking.
            serde_json::to_string(&serde_json::json!({
                "u": actor.user_id,
                "c": claims,
                "t": actor.tenant_id,
            }))
            .unwrap_or_else(|_| format!("{:?}", (&actor.user_id, &claims, &actor.tenant_id)))
        }
    }
}

/// Return the cached compiled config for `(project_root, actor)` iff its stored
/// CacheToken `version` matches `cache_version`. The kernel's `loading.rs` calls
/// this to short-circuit recompilation when the source is unchanged for THIS
/// actor — a different actor never hits another actor's entry.
pub(crate) fn cached_compiled(
    project_root: &Path,
    actor: Option<&Actor>,
    cache_version: &str,
) -> Option<LoadedWorkflowConfig> {
    let key = (normalize_root(project_root), actor_cache_key(actor));
    let guard = compiled_cache().lock().unwrap_or_else(|p| p.into_inner());
    match guard.get(&key) {
        Some((version, loaded)) if version == cache_version => Some(loaded.clone()),
        _ => None,
    }
}

/// Store the compiled config for `(project_root, actor)` under its CacheToken
/// `version`.
pub(crate) fn store_compiled(
    project_root: &Path,
    actor: Option<&Actor>,
    cache_version: String,
    loaded: LoadedWorkflowConfig,
) {
    let key = (normalize_root(project_root), actor_cache_key(actor));
    compiled_cache().lock().unwrap_or_else(|p| p.into_inner()).insert(key, (cache_version, loaded));
}

/// Drop the cached compiled config for `project_root` so the next load
/// recompiles. Called after a `config/write` (the source changed under us) and
/// on host re-spawn. Clears EVERY actor partition for the root: a write (or a
/// source/host change) can alter what any user sees, so a single global event
/// must invalidate all per-actor entries, not just the writer's.
fn invalidate_compiled(project_root: &Path) {
    let root = normalize_root(project_root);
    compiled_cache().lock().unwrap_or_else(|p| p.into_inner()).retain(|(cached_root, _actor), _| cached_root != &root);
}

/// Normalize a project root the same way the test seam does, so the resident
/// host / compiled cache keys line up regardless of symlink canonicalization
/// (e.g. /var -> /private/var on macOS).
fn normalize_root(project_root: &Path) -> PathBuf {
    std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf())
}

/// Reap every resident `config_source` host and clear the compiled cache.
/// Wired into the daemon's graceful-shutdown teardown so the warm plugin
/// processes are terminated cleanly (and the CLI's short-lived processes do
/// not leak a host on exit). Idempotent.
pub async fn shutdown_resident_hosts() {
    let hosts: Vec<ResidentHost> = {
        let mut guard = resident_hosts().lock().unwrap_or_else(|p| p.into_inner());
        guard.drain().map(|(_, v)| v).collect()
    };
    for resident in hosts {
        let _ = resident.host.shutdown().await;
    }
    compiled_cache().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

/// Drop + reap the resident host for `project_root` ONLY if the cached entry is
/// still the generation `failed_gen` that failed. A concurrent caller may have
/// already reaped that dead host and installed a fresh replacement; reaping by
/// generation guarantees we never shut down that healthy replacement out from
/// under the other caller.
async fn drop_resident_host_if_current(project_root: &Path, failed_gen: u64) {
    let key = normalize_root(project_root);
    let resident = {
        let mut guard = resident_hosts().lock().unwrap_or_else(|p| p.into_inner());
        match guard.get(&key) {
            Some(existing) if existing.generation == failed_gen => guard.remove(&key),
            _ => None,
        }
    };
    if let Some(resident) = resident {
        let _ = resident.host.shutdown().await;
    }
}

/// True if a `config_source` plugin is installed (cheap discovery, no spawn).
/// Used by callers that early-return when there's nothing to compile.
pub fn config_source_installed(project_root: &Path) -> bool {
    discover_by_kind(project_root.to_path_buf(), CONFIG_SOURCE_KIND).map(|plugins| !plugins.is_empty()).unwrap_or(false)
}

/// Resolve a `config_source` plugin (if installed) and load the base config.
/// `Ok(None)` => no plugin installed and (in tests) no injected base; the
/// caller surfaces an actionable "no config_source plugin installed" error.
/// Returns `(base WorkflowConfig, cache_token_version)`.
pub fn resolve_plugin_base(project_root: &Path, actor: Option<&Actor>) -> Result<Option<(WorkflowConfig, String)>> {
    // Test-only seam: a synthetic base config injected via
    // `set_test_plugin_base` stands in for an installed config_source plugin so
    // unit tests can exercise the kernel's pack-merge + validate pipeline (which
    // it still owns) without spawning a real plugin process.
    #[cfg(any(test, feature = "test-utils"))]
    if let Some(base) = test_seam::base_for(project_root) {
        // Derive the CacheToken from the base content so the compiled-config
        // short-circuit in `loading.rs` recompiles when a test reinstalls a
        // DIFFERENT base for the same root. A constant token would serve a
        // stale compile across a real change.
        let token = super::loading::workflow_config_hash(&base);
        return Ok(Some((base, token)));
    }

    let plugins = discover_by_kind(project_root.to_path_buf(), CONFIG_SOURCE_KIND)
        .with_context(|| format!("discovering config_source plugins for {}", project_root.display()))?;
    let Some(plugin) = plugins.into_iter().next() else {
        return Ok(None);
    };
    let loaded = run_blocking(load_via_resident(plugin, project_root.to_path_buf(), actor.cloned()))?;
    Ok(Some(loaded?))
}

/// A failure running an RPC against a resident host. Distinguishes a death-like
/// host failure (presume the process is dead → reap + re-spawn + retry once)
/// from any other error (a structured plugin RPC error, or a decode/validation
/// failure on a live host) which must propagate without burning a re-spawn.
enum ResidentCallError {
    /// The host's process is presumed dead; the call may succeed on a fresh
    /// spawn. Carries the original error for the message if re-spawn also fails.
    Death(anyhow::Error),
    /// A live-host error (structured plugin error or response decode failure);
    /// re-spawning would not help.
    Other(anyhow::Error),
}

impl ResidentCallError {
    /// Classify a `HostError` returned by an RPC into death-like vs other.
    fn from_host_error(err: HostError) -> Self {
        match classify(&err) {
            RetryDecision::DeathLike => ResidentCallError::Death(anyhow!(err)),
            RetryDecision::StructuredError => ResidentCallError::Other(anyhow!(err)),
        }
    }
}

/// Acquire the resident host for `project_root` (spawning it once if absent),
/// then run `call` against a clone of it. On a death-like failure (the warm
/// host's process is presumed dead), reap it, re-spawn exactly once, and retry
/// the call. All other errors propagate without a re-spawn.
///
/// The map lock is never held across the RPC `.await`: we take a clone of the
/// `PluginHost` (cheap — it is `Arc`-backed) while holding the lock, then drop
/// the lock before calling. Distinct roots therefore never serialize on each
/// other's RPCs.
async fn with_resident_host<T, F, Fut>(plugin: DiscoveredPlugin, project_root: &Path, mut call: F) -> Result<T>
where
    F: FnMut(PluginHost) -> Fut,
    Fut: Future<Output = std::result::Result<T, ResidentCallError>>,
{
    let (host, generation) = acquire_resident_host(&plugin, project_root).await?;
    match call(host).await {
        Ok(value) => Ok(value),
        Err(ResidentCallError::Other(err)) => Err(err),
        Err(ResidentCallError::Death(err)) => {
            // The warm host is presumed dead: reap it (only if it's still the
            // generation that failed — a concurrent caller may have already
            // replaced it), re-spawn once, retry.
            drop_resident_host_if_current(project_root, generation).await;
            let (host, _gen) = acquire_resident_host(&plugin, project_root).await?;
            match call(host).await {
                Ok(value) => Ok(value),
                Err(ResidentCallError::Other(retry_err)) => Err(retry_err),
                Err(ResidentCallError::Death(retry_err)) => Err(retry_err.context(format!(
                    "config_source plugin {} still failing after one re-spawn (first error: {err})",
                    plugin.name
                ))),
            }
        }
    }
}

/// Return a clone of the resident host for `project_root` plus its generation
/// id, spawning + handshaking it if none is cached (or the cached one was
/// spawned from a different path / a changed binary).
async fn acquire_resident_host(plugin: &DiscoveredPlugin, project_root: &Path) -> Result<(PluginHost, u64)> {
    let key = normalize_root(project_root);
    // A cached host is only reused when it was spawned from the SAME path AND
    // the binary's mtime is unchanged — so an in-place plugin upgrade/replace
    // (new bytes at the same path) drops the stale host and re-spawns.
    let current_mtime = binary_mtime_nanos(&plugin.path);
    {
        let guard = resident_hosts().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(resident) = guard.get(&key) {
            if resident.plugin_path == plugin.path && resident.binary_mtime_nanos == current_mtime {
                return Ok((resident.host.clone(), resident.generation));
            }
        }
    }

    // Spawn + handshake OUTSIDE the map lock so a slow spawn for one root never
    // blocks another root's cache lookups.
    let host = spawn_config_source_host(plugin).await?;
    host.handshake().await.with_context(|| format!("handshake with config_source plugin {}", plugin.name))?;

    // Decide the outcome WITHOUT holding the std Mutex across an `.await`: take
    // any host that needs reaping out under the lock, drop the guard, then await
    // the shutdown. (`acquire_resident_host` is the only place that mutates the
    // map besides shutdown/drop, so the brief window between unlock and reap is
    // benign — the loser host is owned solely by this stack frame.)
    enum Outcome {
        /// A concurrent caller already inserted a matching host: keep theirs,
        /// reap ours.
        UseExisting { winner: PluginHost, generation: u64, reap_ours: PluginHost },
        /// We installed ours; reap the (stale-path) host it replaced, if any.
        Installed { ours: PluginHost, generation: u64, reap_old: Option<PluginHost> },
    }
    let outcome = {
        let mut guard = resident_hosts().lock().unwrap_or_else(|p| p.into_inner());
        match guard.get(&key) {
            Some(existing) if existing.plugin_path == plugin.path && existing.binary_mtime_nanos == current_mtime => {
                Outcome::UseExisting { winner: existing.host.clone(), generation: existing.generation, reap_ours: host }
            }
            _ => {
                let ours = host.clone();
                let generation = next_generation();
                let replaced = guard.insert(
                    key,
                    ResidentHost {
                        host,
                        plugin_path: plugin.path.clone(),
                        binary_mtime_nanos: current_mtime,
                        generation,
                    },
                );
                Outcome::Installed { ours, generation, reap_old: replaced.map(|r| r.host) }
            }
        }
    };

    match outcome {
        Outcome::UseExisting { winner, generation, reap_ours } => {
            let _ = reap_ours.shutdown().await;
            Ok((winner, generation))
        }
        Outcome::Installed { ours, generation, reap_old } => {
            if let Some(old) = reap_old {
                let _ = old.shutdown().await;
            }
            Ok((ours, generation))
        }
    }
}

/// Spawn a `config_source` plugin host with the full parent env forwarded.
///
/// Unlike other plugin roles, a config_source plugin REPLACES the kernel's
/// in-process YAML parsing, which read the daemon's full process environment
/// for non-secret `${VAR}` interpolation (team ids, URLs, feature flags). We
/// forward the full parent env plus the manifest-declared secret env (e.g.
/// DATABASE_URL); secret-backed `${secret.*}` still resolves via the plugin's
/// own keychain resolver, repo-scope aware.
async fn spawn_config_source_host(plugin: &DiscoveredPlugin) -> Result<PluginHost> {
    let forwarded_env: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
    let options =
        PluginSpawnOptions::for_manifest(plugin.name.clone(), &plugin.manifest.env_required, forwarded_env, None);
    PluginHost::spawn_with_options(&plugin.path, &[], options)
        .await
        .with_context(|| format!("spawning config_source plugin {}", plugin.name))
}

/// Reuse the resident host to run `config/load`. The host is NOT reaped after
/// the call (the v0.6.6 resident model) — only on death-like failure or at
/// shutdown via [`shutdown_resident_hosts`].
async fn load_via_resident(
    plugin: DiscoveredPlugin,
    project_root: PathBuf,
    actor: Option<Actor>,
) -> Result<(WorkflowConfig, String)> {
    let name = plugin.name.clone();
    let call_root = project_root.clone();
    with_resident_host(plugin, &project_root, move |host| {
        let project_root = call_root.clone();
        let name = name.clone();
        let actor = actor.clone();
        async move { config_load_call(&host, &name, &project_root, actor.as_ref()).await }
    })
    .await
}

/// Persist `config` through the installed `config_source` plugin's
/// `config/write`. The kernel is the validator: callers MUST have already run
/// `validate_workflow_config_with_project_root` against `config` before calling
/// this — the plugin trusts the model and only enforces its own storage
/// constraints.
///
/// Errors (each surfaces an actionable message, never panics or corrupts):
/// - no `config_source` plugin installed;
/// - the installed source does not advertise [`CAPABILITY_CONFIG_WRITE`]
///   (e.g. the read-only YAML source) — refused up front, no RPC issued;
/// - the plugin's `config/write` RPC fails.
///
/// On success the resident host is REUSED (not reaped — v0.6.6 resident model)
/// and the compiled-config cache for this root is invalidated so the next load
/// recompiles from the freshly-written source. The caller is responsible for
/// triggering a reload so the daemon's in-memory snapshot refreshes.
pub fn write_plugin_config(project_root: &Path, config: &WorkflowConfig) -> Result<()> {
    let plugins = discover_by_kind(project_root.to_path_buf(), CONFIG_SOURCE_KIND)
        .with_context(|| format!("discovering config_source plugins for {}", project_root.display()))?;
    let Some(plugin) = plugins.into_iter().next() else {
        return Err(anyhow!(
            "no config_source plugin is installed, so there is nothing to write the config to; install one with `animus plugin install-defaults` (the default `animus-config-yaml` is read-only — a writable source such as `animus-config-postgres` is required to manage config through animus)"
        ));
    };

    if !plugin.manifest.capabilities.iter().any(|c| c == animus_config_protocol::CAPABILITY_CONFIG_WRITE) {
        return Err(anyhow!(
            "the installed config_source plugin '{}' does not support writes (it does not advertise the '{}' capability); config managed by this source must be edited at its origin (e.g. the `.animus/workflows.yaml` files for the YAML source), or install a writable source such as `animus-config-postgres`",
            plugin.name,
            animus_config_protocol::CAPABILITY_CONFIG_WRITE,
        ));
    }

    let model = config_to_model(config)?;
    run_blocking(write_via_resident(plugin, project_root.to_path_buf(), model))?
}

/// Serialize a [`WorkflowConfig`] into the wire [`ConfigModel`] envelope,
/// tagged with the current schema id / version.
fn config_to_model(config: &WorkflowConfig) -> Result<animus_config_protocol::ConfigModel> {
    let value = serde_json::to_value(config).context("serializing WorkflowConfig for config/write")?;
    Ok(animus_config_protocol::ConfigModel::new(value))
}

/// Reuse the resident host to run `config/write`, then invalidate the
/// compiled-config cache so the next load recompiles from the new source.
async fn write_via_resident(
    plugin: DiscoveredPlugin,
    project_root: PathBuf,
    model: animus_config_protocol::ConfigModel,
) -> Result<()> {
    let name = plugin.name.clone();
    let call_root = project_root.clone();
    let result = with_resident_host(plugin, &project_root, move |host| {
        let project_root = call_root.clone();
        let name = name.clone();
        let model = model.clone();
        async move { config_write_call(&host, &name, &project_root, model).await }
    })
    .await;
    if result.is_ok() {
        // The source changed under us; force the next load to recompile.
        invalidate_compiled(&project_root);
    }
    result
}

/// `config/write` against a resident host clone. Returns a [`ResidentCallError`]
/// so the caller can reap + re-spawn on a death-like failure.
async fn config_write_call(
    host: &PluginHost,
    plugin_name: &str,
    project_root: &Path,
    model: animus_config_protocol::ConfigModel,
) -> std::result::Result<(), ResidentCallError> {
    let request = animus_config_protocol::ConfigWriteRequest {
        project_root: project_root.display().to_string(),
        repo_scope: Some(protocol::repository_scope_for_path(project_root)),
        config: model,
    };
    let params = match serde_json::to_value(&request).context("serializing ConfigWriteRequest") {
        Ok(params) => params,
        Err(err) => return Err(ResidentCallError::Other(err)),
    };
    let value = host
        .request_typed_with_timeout("config/write", Some(params), CONFIG_WRITE_TIMEOUT)
        .await
        .map_err(ResidentCallError::from_host_error)?;

    // Decode for validation/forward-compat, even though the current kernel only
    // needs to know the call succeeded (it re-issues config/load on reload).
    let _resp: animus_config_protocol::ConfigWriteResponse = serde_json::from_value(value)
        .context(format!("decoding ConfigWriteResponse from config_source plugin {plugin_name}"))
        .map_err(ResidentCallError::Other)?;
    Ok(())
}

/// `config/load` against a resident host clone. Returns a [`ResidentCallError`]
/// so the caller can reap + re-spawn on a death-like failure.
async fn config_load_call(
    host: &PluginHost,
    plugin_name: &str,
    project_root: &Path,
    actor: Option<&Actor>,
) -> std::result::Result<(WorkflowConfig, String), ResidentCallError> {
    // Compute the repo-scope id from the project root so config_source plugins
    // that select config rows / repo-scoped secrets by scope (e.g. postgres)
    // get a real scope, not null.
    //
    // TRUST BOUNDARY: `actor` is the transport-asserted caller identity relayed
    // verbatim from the authenticated control request. It is serialized into the
    // `config/load` params (`null` when absent) so a per-user `config_source`
    // plugin can scope its result. The kernel never authenticates or interprets
    // it — and it is NEVER sourced from workflow YAML, agent output, or subject
    // content.
    let params = serde_json::json!({
        "project_root": project_root,
        "repo_scope": protocol::repository_scope_for_path(project_root),
        "actor": actor,
    });
    let value = host
        .request_typed_with_timeout("config/load", Some(params), CONFIG_LOAD_TIMEOUT)
        .await
        .map_err(ResidentCallError::from_host_error)?;

    let resp: animus_config_protocol::ConfigLoadResponse = match serde_json::from_value(value)
        .context(format!("decoding ConfigLoadResponse from config_source plugin {plugin_name}"))
    {
        Ok(resp) => resp,
        Err(err) => return Err(ResidentCallError::Other(err)),
    };
    // Reject incompatible models: wrong schema OR a newer version this kernel
    // can't safely interpret (`ConfigModel::is_compatible` = schema match AND
    // version <= CONFIG_MODEL_VERSION).
    if !resp.config.is_compatible() {
        return Err(ResidentCallError::Other(anyhow!(
            "config_source plugin {} returned an incompatible config model (schema '{}', version {}); this kernel supports schema '{}' up to version {}",
            plugin_name,
            resp.config.schema,
            resp.config.version,
            animus_config_protocol::CONFIG_MODEL_SCHEMA_ID,
            animus_config_protocol::CONFIG_MODEL_VERSION,
        )));
    }
    match serde_json::from_value(resp.config.config)
        .with_context(|| format!("deserializing {plugin_name}'s config into WorkflowConfig"))
    {
        Ok(config) => Ok((config, resp.cache_token.version)),
        Err(err) => Err(ResidentCallError::Other(err)),
    }
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
    let _ = super::yaml_scaffold::scaffold_default_workflows_for_tests(project_root);
    let base = super::compile_yaml_workflow_files(project_root)
        .expect("compile project yaml base")
        .unwrap_or_else(super::builtin_workflow_config);
    test_seam::install(project_root, base)
}

#[cfg(test)]
mod resident_cache_tests {
    use super::*;
    use animus_config_protocol::builtins::builtin_workflow_config;

    fn loaded(root: &Path) -> LoadedWorkflowConfig {
        loaded_marked(root, "")
    }

    /// Build a `LoadedWorkflowConfig` tagged with `marker` in `default_workflow_ref`
    /// so a test can prove two cached entries are distinct (no cross-actor leak).
    fn loaded_marked(root: &Path, marker: &str) -> LoadedWorkflowConfig {
        let mut config = builtin_workflow_config();
        config.default_workflow_ref = marker.to_string();
        LoadedWorkflowConfig {
            config,
            metadata: super::super::types::WorkflowConfigMetadata {
                schema: String::new(),
                version: 0,
                hash: String::new(),
                source: super::super::types::WorkflowConfigSource::Yaml,
            },
            path: root.to_path_buf(),
        }
    }

    fn actor(user_id: &str) -> Actor {
        Actor { user_id: user_id.to_string(), claims: Vec::new(), tenant_id: None }
    }

    #[test]
    fn compiled_cache_hits_on_matching_token_and_misses_otherwise() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        // Cold: nothing cached.
        assert!(cached_compiled(root, None, "tok-1").is_none());

        store_compiled(root, None, "tok-1".to_string(), loaded(root));
        // Same token: hit.
        assert!(cached_compiled(root, None, "tok-1").is_some());
        // Different token (source changed): miss => caller recompiles.
        assert!(cached_compiled(root, None, "tok-2").is_none());
    }

    #[test]
    fn invalidate_compiled_forces_recompile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        store_compiled(root, None, "tok".to_string(), loaded(root));
        assert!(cached_compiled(root, None, "tok").is_some());
        invalidate_compiled(root);
        assert!(cached_compiled(root, None, "tok").is_none(), "write must invalidate the compiled cache");
    }

    #[test]
    fn compiled_cache_never_leaks_across_actors() {
        // (a) SAME project_root + token, two DIFFERENT actors => two distinct
        // compiled configs. Actor B must never receive actor A's entry.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let alice = actor("alice");
        let bob = actor("bob");

        store_compiled(root, Some(&alice), "tok".to_string(), loaded_marked(root, "alice-wf"));
        store_compiled(root, Some(&bob), "tok".to_string(), loaded_marked(root, "bob-wf"));

        let a = cached_compiled(root, Some(&alice), "tok").expect("alice cached");
        let b = cached_compiled(root, Some(&bob), "tok").expect("bob cached");
        assert_eq!(a.config.default_workflow_ref, "alice-wf");
        assert_eq!(b.config.default_workflow_ref, "bob-wf", "bob must not be served alice's compiled config");

        // The global (actor=None) partition is independent of both users.
        assert!(
            cached_compiled(root, None, "tok").is_none(),
            "per-actor stores must not populate the global partition"
        );
    }

    #[test]
    fn actor_cache_key_is_claim_order_independent_and_partitions_identity() {
        // (b) actor=None maps to the shared global partition, unchanged from
        // today's behavior.
        assert_eq!(actor_cache_key(None), "__global__");

        // Claim order does not matter (sorted), so the same identity always
        // shares one partition.
        let unsorted = Actor { user_id: "u".into(), claims: vec!["b".into(), "a".into()], tenant_id: Some("t".into()) };
        let sorted = Actor { user_id: "u".into(), claims: vec!["a".into(), "b".into()], tenant_id: Some("t".into()) };
        assert_eq!(actor_cache_key(Some(&unsorted)), actor_cache_key(Some(&sorted)));

        // Different user / tenant / claims partition.
        assert_ne!(actor_cache_key(Some(&actor("alice"))), actor_cache_key(Some(&actor("bob"))));
        let no_tenant = Actor { user_id: "u".into(), claims: vec!["a".into()], tenant_id: None };
        assert_ne!(actor_cache_key(Some(&sorted)), actor_cache_key(Some(&no_tenant)));

        // SECURITY: delimiter-bearing fields must NOT collide. With a naive
        // `user_id|claims.join(",")|tenant` encoding, (`a|b`, [`c`]) and
        // (`a`, [`b|c`]) would both render `a|b|c|` and share a cache partition.
        // The unambiguous (JSON) encoding must keep them distinct.
        let pipe_user = Actor { user_id: "a|b".into(), claims: vec!["c".into()], tenant_id: None };
        let pipe_claim = Actor { user_id: "a".into(), claims: vec!["b|c".into()], tenant_id: None };
        assert_ne!(
            actor_cache_key(Some(&pipe_user)),
            actor_cache_key(Some(&pipe_claim)),
            "delimiter-bearing identities must not collide into one cache partition"
        );
    }

    #[test]
    fn invalidate_clears_every_actor_partition_for_the_root() {
        // (c) a global/source change (config/write) must invalidate EVERY actor's
        // entry for the root, not just one partition.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let alice = actor("alice");
        let bob = actor("bob");

        store_compiled(root, Some(&alice), "tok".to_string(), loaded_marked(root, "alice-wf"));
        store_compiled(root, Some(&bob), "tok".to_string(), loaded_marked(root, "bob-wf"));
        store_compiled(root, None, "tok".to_string(), loaded(root));

        invalidate_compiled(root);

        assert!(cached_compiled(root, Some(&alice), "tok").is_none(), "alice entry must be invalidated");
        assert!(cached_compiled(root, Some(&bob), "tok").is_none(), "bob entry must be invalidated");
        assert!(cached_compiled(root, None, "tok").is_none(), "global entry must be invalidated");
    }

    #[test]
    fn death_like_host_errors_trigger_respawn_decision() {
        // ConnectionLost / Timeout / ProcessExited are death-like => the
        // resident cache reaps + re-spawns + retries once.
        assert!(matches!(ResidentCallError::from_host_error(HostError::ConnectionLost), ResidentCallError::Death(_)));
        assert!(matches!(
            ResidentCallError::from_host_error(HostError::Timeout(Duration::from_secs(1))),
            ResidentCallError::Death(_)
        ));
    }

    #[test]
    fn incompatible_protocol_is_death_like_not_silently_dropped() {
        // Pre-request handshake failures are conservatively death-like (the
        // dispatcher burns a restart slot rather than dropping the failure).
        assert!(matches!(
            ResidentCallError::from_host_error(HostError::IncompatibleProtocol("x".into())),
            ResidentCallError::Death(_)
        ));
    }
}
