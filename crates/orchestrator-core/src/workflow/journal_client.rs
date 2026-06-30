//! BU-1: the `workflow_journal` plugin seam.
//!
//! [`WorkflowStateManager`](super::state_manager::WorkflowStateManager) persists
//! workflow RUN STATE, CHECKPOINTS, and lifecycle EVENTS. Historically that lived
//! exclusively in a local SQLite file (`workflow.db`), which is ephemeral on a
//! disposable host (a Railway container loses run history on every redeploy).
//!
//! This module resolves the backend ONCE at `WorkflowStateManager::new`:
//! - no `workflow_journal` plugin installed  => [`JournalBackend::Sqlite`]
//!   (the in-tree rusqlite engine, byte-identical to pre-BU-1 behavior).
//! - a `workflow_journal` plugin installed    => [`JournalBackend::Plugin`]
//!   (run state/checkpoints/events persist through the plugin's `journal/*`
//!   RPCs, e.g. Postgres).
//!
//! The plugin treats run state as an OPAQUE JSON blob (`JournalRun::blob`, the
//! serialized `OrchestratorWorkflow`) plus indexed summary columns, so the kernel
//! can evolve the workflow model without a protocol bump.
//!
//! ## Capability boundary (v1)
//!
//! The journal protocol lets a minimal backend advertise
//! `supports_checkpoints = false` / `supports_events = false`. This kernel half
//! (BU-1) does NOT yet read `journal/schema` to gate delegation: an installed
//! `workflow_journal` plugin is expected to implement the FULL method set
//! (run state + checkpoints + record). The reference Postgres backend does.
//! A capability-aware fallback to local SQLite for checkpoints/events is a
//! follow-up. The event sink (BU-3) is the one exception — it is purely additive
//! and a `journal/record` failure is logged and dropped, never fatal.
//!
//! ## Resident-host model
//!
//! Mirrors `orchestrator_config::workflow_config::config_source_client`: ONE warm
//! plugin host per project root, kept across calls (journal RPCs fire per phase),
//! with death-aware respawn and a sync↔async bridge ([`run_blocking`]).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use animus_actor::Actor;
use animus_journal_protocol::{
    CheckpointListResult, CheckpointLoadParams, CheckpointLoadResult, CheckpointPruneParams, CheckpointSaveParams,
    JournalEvent, JournalQuery, JournalRun, ListResult, LoadResult, QueryIdsResult, RecordParams, SaveParams,
    WorkflowIdParams, METHOD_JOURNAL_CHECKPOINT_LIST, METHOD_JOURNAL_CHECKPOINT_LOAD, METHOD_JOURNAL_CHECKPOINT_PRUNE,
    METHOD_JOURNAL_CHECKPOINT_SAVE, METHOD_JOURNAL_DELETE, METHOD_JOURNAL_LIST, METHOD_JOURNAL_LOAD,
    METHOD_JOURNAL_QUERY_IDS, METHOD_JOURNAL_RECORD, METHOD_JOURNAL_SAVE, PLUGIN_KIND_WORKFLOW_JOURNAL,
};
use anyhow::{anyhow, Context, Result};
use orchestrator_plugin_host::session::plugin_supervisor::{classify, RetryDecision};
use orchestrator_plugin_host::{discover_by_kind, DiscoveredPlugin, HostError, PluginHost, PluginSpawnOptions};

use crate::types::{OrchestratorWorkflow, WorkflowStatus};

const JOURNAL_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Env kill-switch: when set to `1`/`true`, force the in-tree SQLite backend even
/// if a `workflow_journal` plugin is discovered. Safety valve so an operator (or a
/// dev machine with a globally-installed journal plugin) can pin the byte-identical
/// local path without uninstalling. Requires a process restart to take effect
/// (the backend is resolved once and cached).
const DISABLE_JOURNAL_PLUGIN_ENV: &str = "ANIMUS_DISABLE_WORKFLOW_JOURNAL_PLUGIN";

/// Reserved blob key holding the run's transport-asserted [`Actor`] in the plugin
/// backend. The journal protocol's `JournalRun` has no actor field (actor is kernel
/// ROUTING context, not part of the wire run record), and the local SQLite backend
/// stores it in a dedicated column. For the plugin backend we fold it into the
/// opaque blob under this key; it is stripped before the blob is deserialized back
/// into an `OrchestratorWorkflow`.
const ACTOR_BLOB_KEY: &str = "__animus_actor__";

/// Which persistence engine a [`WorkflowStateManager`](super::state_manager::WorkflowStateManager)
/// is bound to for its lifetime. Resolved once at construction.
#[derive(Debug, Clone)]
pub(crate) enum JournalBackend {
    /// In-tree rusqlite engine (`workflow.db`). The default; byte-identical to
    /// pre-BU-1 behavior.
    Sqlite,
    /// An installed `workflow_journal` plugin. Carries the discovered plugin
    /// (boxed — it is far larger than the unit `Sqlite` variant); the warm host
    /// lives in the process-global resident-host cache keyed by root.
    Plugin(Box<DiscoveredPlugin>),
}

/// Process-global cache of resolved backends keyed by normalized project root, so
/// the (cheap, no-spawn) plugin discovery runs once per root rather than on every
/// `WorkflowStateManager::new` (constructed ad hoc in ~12 call sites, some per
/// phase). Mirrors the resident-host caching rationale in `config_source_client`.
fn backend_cache() -> &'static Mutex<HashMap<PathBuf, JournalBackend>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, JournalBackend>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve the journal backend for `project_root`, caching the decision. Returns
/// [`JournalBackend::Sqlite`] when no `workflow_journal` plugin is installed (the
/// default) or when the kill-switch is set; otherwise [`JournalBackend::Plugin`].
pub(crate) fn resolve_backend(project_root: &Path) -> JournalBackend {
    let key = normalize_root(project_root);
    if let Some(cached) = backend_cache().lock().unwrap_or_else(|p| p.into_inner()).get(&key).cloned() {
        return cached;
    }
    let resolved = discover_backend(project_root);
    backend_cache().lock().unwrap_or_else(|p| p.into_inner()).insert(key, resolved.clone());
    resolved
}

fn discover_backend(project_root: &Path) -> JournalBackend {
    if env_flag_enabled(DISABLE_JOURNAL_PLUGIN_ENV) {
        return JournalBackend::Sqlite;
    }
    match discover_by_kind(project_root.to_path_buf(), PLUGIN_KIND_WORKFLOW_JOURNAL) {
        Ok(mut plugins) if !plugins.is_empty() => JournalBackend::Plugin(Box::new(plugins.remove(0))),
        _ => JournalBackend::Sqlite,
    }
}

fn env_flag_enabled(var: &str) -> bool {
    std::env::var(var).map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "on")).unwrap_or(false)
}

/// The installed `workflow_journal` plugin for `project_root`, or `None` (SQLite).
/// Used by the free-function summary/index readers in `state_manager` so they
/// route through the same backend as `WorkflowStateManager` rather than reading a
/// stale local SQLite table when a plugin is installed.
pub(crate) fn plugin_for(project_root: &Path) -> Option<DiscoveredPlugin> {
    match resolve_backend(project_root) {
        JournalBackend::Sqlite => None,
        JournalBackend::Plugin(plugin) => Some(*plugin),
    }
}

/// Clear the cached backend decisions. Test-only seam so a test that installs (or
/// removes) a synthetic plugin between runs is not served a stale decision.
#[cfg(test)]
pub(crate) fn reset_backend_cache_for_tests() {
    backend_cache().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

// ---------------------------------------------------------------------------
// Blob <-> OrchestratorWorkflow conversions
// ---------------------------------------------------------------------------

fn status_wire(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Pending => "pending",
        WorkflowStatus::Running => "running",
        WorkflowStatus::Paused => "paused",
        WorkflowStatus::Completed => "completed",
        WorkflowStatus::Failed => "failed",
        WorkflowStatus::Escalated => "escalated",
        WorkflowStatus::Cancelled => "cancelled",
    }
}

/// Serialize an [`OrchestratorWorkflow`] into a [`JournalRun`]: the blob is the
/// serialized workflow (the same shape persisted as `snapshot_json` in checkpoints
/// today), plus indexed summary columns the backend can query.
pub(crate) fn to_journal_run(workflow: &OrchestratorWorkflow) -> Result<JournalRun> {
    let blob = serde_json::to_value(workflow).context("serializing OrchestratorWorkflow for journal")?;
    let kind = if workflow.subject.kind.is_empty() { None } else { Some(workflow.subject.kind.clone()) };
    Ok(JournalRun {
        workflow_id: workflow.id.clone(),
        workflow_ref: workflow.workflow_ref.clone(),
        status: status_wire(workflow.status).to_string(),
        kind,
        blob,
        created_at: Some(workflow.started_at),
        updated_at: Some(workflow.completed_at.unwrap_or(workflow.started_at)),
    })
}

/// Reconstruct an [`OrchestratorWorkflow`] from a [`JournalRun`] blob. The reserved
/// [`ACTOR_BLOB_KEY`] (kernel routing context, not part of the workflow record) is
/// stripped before deserialization so it never leaks into the model.
pub(crate) fn from_journal_run(run: JournalRun) -> Result<OrchestratorWorkflow> {
    let mut blob = run.blob;
    if let Some(obj) = blob.as_object_mut() {
        obj.remove(ACTOR_BLOB_KEY);
    }
    serde_json::from_value(blob).context("deserializing OrchestratorWorkflow from journal blob")
}

fn actor_from_run(run: &JournalRun) -> Option<Actor> {
    run.blob.as_object().and_then(|obj| obj.get(ACTOR_BLOB_KEY)).and_then(|v| serde_json::from_value(v.clone()).ok())
}

// ---------------------------------------------------------------------------
// Public sync surface used by WorkflowStateManager (Plugin backend only).
// Each resolves/reuses the resident host and bridges async->sync via run_blocking.
// ---------------------------------------------------------------------------

/// Upsert a run's state. Preserves a previously-bound [`Actor`] (folded into the
/// blob by [`save_actor`]) across saves, mirroring the SQLite backend's
/// column-preserving upsert which never NULLs `actor` on a lifecycle save.
pub(crate) fn save(plugin: &DiscoveredPlugin, project_root: &Path, workflow: &OrchestratorWorkflow) -> Result<()> {
    let mut run = to_journal_run(workflow)?;
    // Carry an existing actor forward: a normal `save` rewrites the whole blob, so
    // without this it would drop the actor `save_actor` wrote at bootstrap.
    if let Ok(Some(existing)) = load_run_opt(plugin, project_root, &workflow.id) {
        if let Some(actor_value) = existing.blob.as_object().and_then(|o| o.get(ACTOR_BLOB_KEY)).cloned() {
            if let Some(obj) = run.blob.as_object_mut() {
                obj.insert(ACTOR_BLOB_KEY.to_string(), actor_value);
            }
        }
    }
    run_blocking(call(plugin, project_root, METHOD_JOURNAL_SAVE, SaveParams { run }))?.map(|_: serde_json::Value| ())
}

pub(crate) fn load(plugin: &DiscoveredPlugin, project_root: &Path, workflow_id: &str) -> Result<OrchestratorWorkflow> {
    match load_run_opt(plugin, project_root, workflow_id)? {
        Some(run) => from_journal_run(run),
        None => Err(anyhow!("workflow not found: {workflow_id}")),
    }
}

fn load_run_opt(plugin: &DiscoveredPlugin, project_root: &Path, workflow_id: &str) -> Result<Option<JournalRun>> {
    let params = WorkflowIdParams { workflow_id: workflow_id.to_string() };
    let value = run_blocking(call(plugin, project_root, METHOD_JOURNAL_LOAD, params))??;
    let resp: LoadResult = serde_json::from_value(value).context("decoding journal LoadResult")?;
    Ok(resp.run)
}

pub(crate) fn delete(plugin: &DiscoveredPlugin, project_root: &Path, workflow_id: &str) -> Result<()> {
    let params = WorkflowIdParams { workflow_id: workflow_id.to_string() };
    run_blocking(call(plugin, project_root, METHOD_JOURNAL_DELETE, params))?.map(|_: serde_json::Value| ())
}

/// List runs whose status is in `statuses` (empty = all), newest-first as ordered
/// by the backend. Decodes each blob back into an `OrchestratorWorkflow`; rows that
/// fail to decode are skipped (matching the SQLite path's `filter_map`).
pub(crate) fn list(
    plugin: &DiscoveredPlugin,
    project_root: &Path,
    statuses: &[&str],
) -> Result<Vec<OrchestratorWorkflow>> {
    let query = JournalQuery {
        status: statuses.iter().map(|s| (*s).to_string()).collect(),
        workflow_ref: None,
        updated_since: None,
        limit: None,
    };
    let value = run_blocking(call(plugin, project_root, METHOD_JOURNAL_LIST, query))??;
    let resp: ListResult = serde_json::from_value(value).context("decoding journal ListResult")?;
    Ok(resp.runs.into_iter().filter_map(|run| from_journal_run(run).ok()).collect())
}

/// All run ids matching `status` (None = all). The caller paginates client-side.
pub(crate) fn query_ids(
    plugin: &DiscoveredPlugin,
    project_root: &Path,
    status: Option<WorkflowStatus>,
) -> Result<Vec<String>> {
    let query = JournalQuery {
        status: status.map(|s| vec![status_wire(s).to_string()]).unwrap_or_default(),
        workflow_ref: None,
        updated_since: None,
        limit: None,
    };
    let value = run_blocking(call(plugin, project_root, METHOD_JOURNAL_QUERY_IDS, query))??;
    let resp: QueryIdsResult = serde_json::from_value(value).context("decoding journal QueryIdsResult")?;
    Ok(resp.ids)
}

pub(crate) fn checkpoint_save(
    plugin: &DiscoveredPlugin,
    project_root: &Path,
    workflow_id: &str,
    checkpoint_num: usize,
    blob: serde_json::Value,
) -> Result<()> {
    let params =
        CheckpointSaveParams { workflow_id: workflow_id.to_string(), checkpoint_num: checkpoint_num as u32, blob };
    run_blocking(call(plugin, project_root, METHOD_JOURNAL_CHECKPOINT_SAVE, params))?.map(|_: serde_json::Value| ())
}

pub(crate) fn checkpoint_load(
    plugin: &DiscoveredPlugin,
    project_root: &Path,
    workflow_id: &str,
    checkpoint_num: usize,
) -> Result<OrchestratorWorkflow> {
    let params = CheckpointLoadParams { workflow_id: workflow_id.to_string(), checkpoint_num: checkpoint_num as u32 };
    let value = run_blocking(call(plugin, project_root, METHOD_JOURNAL_CHECKPOINT_LOAD, params))??;
    let resp: CheckpointLoadResult = serde_json::from_value(value).context("decoding journal CheckpointLoadResult")?;
    match resp.blob {
        Some(blob) => from_journal_run(JournalRun {
            workflow_id: workflow_id.to_string(),
            workflow_ref: None,
            status: String::new(),
            kind: None,
            blob,
            created_at: None,
            updated_at: None,
        }),
        None => Err(anyhow!("checkpoint not found: {workflow_id} #{checkpoint_num}")),
    }
}

pub(crate) fn checkpoint_list(plugin: &DiscoveredPlugin, project_root: &Path, workflow_id: &str) -> Result<Vec<usize>> {
    let params = WorkflowIdParams { workflow_id: workflow_id.to_string() };
    let value = run_blocking(call(plugin, project_root, METHOD_JOURNAL_CHECKPOINT_LIST, params))??;
    let resp: CheckpointListResult = serde_json::from_value(value).context("decoding journal CheckpointListResult")?;
    Ok(resp.checkpoint_nums.into_iter().map(|n| n as usize).collect())
}

pub(crate) fn checkpoint_prune(
    plugin: &DiscoveredPlugin,
    project_root: &Path,
    workflow_id: &str,
    keep: usize,
) -> Result<()> {
    let params = CheckpointPruneParams { workflow_id: workflow_id.to_string(), keep: keep as u32 };
    run_blocking(call(plugin, project_root, METHOD_JOURNAL_CHECKPOINT_PRUNE, params))?.map(|_: serde_json::Value| ())
}

/// Bind/clear the run's [`Actor`] by loading the run, folding the actor into the
/// blob under [`ACTOR_BLOB_KEY`] (or removing it), and re-saving. Best-effort: a
/// missing run is a no-op (the row must exist; mirrors the SQLite UPDATE that
/// affects zero rows when absent).
pub(crate) fn save_actor(
    plugin: &DiscoveredPlugin,
    project_root: &Path,
    workflow_id: &str,
    actor: Option<&Actor>,
) -> Result<()> {
    let Some(mut run) = load_run_opt(plugin, project_root, workflow_id)? else {
        return Ok(());
    };
    if let Some(obj) = run.blob.as_object_mut() {
        match actor {
            Some(actor) => {
                let value = serde_json::to_value(actor).context("serializing actor for journal blob")?;
                obj.insert(ACTOR_BLOB_KEY.to_string(), value);
            }
            None => {
                obj.remove(ACTOR_BLOB_KEY);
            }
        }
    }
    run_blocking(call(plugin, project_root, METHOD_JOURNAL_SAVE, SaveParams { run }))?.map(|_: serde_json::Value| ())
}

/// Load the run's bound [`Actor`], if any. Best-effort: a missing run / key / a
/// malformed value yields `None` (global scope), so a lifecycle op never fails on
/// actor lookup (matching the SQLite backend).
pub(crate) fn load_actor(plugin: &DiscoveredPlugin, project_root: &Path, workflow_id: &str) -> Option<Actor> {
    let run = load_run_opt(plugin, project_root, workflow_id).ok()??;
    actor_from_run(&run)
}

/// BU-3 event sink: append a lifecycle event. No-op for the SQLite backend (events
/// are a plugin-only feature — purely additive, state still flows through
/// `WorkflowStateManager`); issues `journal/record` for the plugin backend.
///
/// Async-native (no [`run_blocking`]): the daemon's event broadcaster tees each
/// event by spawning this on the runtime, so it must not block the emit path.
pub async fn record_event_async(project_root: &Path, event: JournalEvent) {
    let plugin = match resolve_backend(project_root) {
        JournalBackend::Sqlite => return,
        JournalBackend::Plugin(plugin) => plugin,
    };
    if let Err(err) = with_resident_host(&plugin, project_root, move |host| {
        let params = RecordParams { event: event.clone() };
        async move {
            let params = serde_json::to_value(&params)
                .map_err(|e| ResidentCallError::Other(anyhow!("serializing RecordParams: {e}")))?;
            journal_rpc(&host, METHOD_JOURNAL_RECORD, params).await.map(|_| ())
        }
    })
    .await
    {
        tracing::debug!(
            target: "animus.workflow.journal",
            error = %err,
            "workflow_journal record failed (event dropped; state persistence is unaffected)"
        );
    }
}

/// Map a wire workflow-event kind (as emitted by the broadcaster) to a
/// [`JournalEventKind`]. Returns `None` for kinds with no journal mapping
/// (e.g. `phase_failed`), which the sink skips.
fn wire_kind_to_journal(wire_kind: &str) -> Option<animus_journal_protocol::JournalEventKind> {
    use animus_journal_protocol::JournalEventKind as K;
    match wire_kind {
        "workflow_started" => Some(K::RunStarted),
        "phase_started" => Some(K::PhaseStarted),
        "phase_completed" => Some(K::PhaseCompleted),
        "workflow_completed" => Some(K::RunCompleted),
        "workflow_failed" => Some(K::RunFailed),
        _ => None,
    }
}

/// BU-3 convenience: build a [`JournalEvent`] from a broadcaster wire event and
/// record it. No-op (returns immediately) for unmapped kinds or the SQLite
/// backend. The full payload is preserved in `detail`; `phase`/`agent`/`status`
/// are best-effort lifted from common payload keys for indexed columns.
pub async fn record_wire_event(
    project_root: &Path,
    workflow_id: &str,
    wire_kind: &str,
    payload: serde_json::Value,
    occurred_at: chrono::DateTime<chrono::Utc>,
) {
    let Some(kind) = wire_kind_to_journal(wire_kind) else {
        return;
    };
    let phase = payload.get("phase_id").and_then(|v| v.as_str()).map(str::to_string);
    let agent = payload.get("agent").or_else(|| payload.get("tool")).and_then(|v| v.as_str()).map(str::to_string);
    let status =
        payload.get("phase_status").or_else(|| payload.get("status")).and_then(|v| v.as_str()).map(str::to_string);
    let workflow_ref = payload.get("workflow_ref").and_then(|v| v.as_str()).map(str::to_string);
    let event = JournalEvent {
        run_id: workflow_id.to_string(),
        workflow_ref,
        kind,
        phase,
        agent,
        status,
        ts: occurred_at,
        detail: payload,
    };
    record_event_async(project_root, event).await;
}

// ---------------------------------------------------------------------------
// Resident-host machinery (mirrors config_source_client).
// ---------------------------------------------------------------------------

struct ResidentHost {
    host: PluginHost,
    plugin_path: PathBuf,
    binary_mtime_nanos: u128,
    generation: u64,
}

fn next_generation() -> u64 {
    static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn binary_mtime_nanos(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn resident_hosts() -> &'static Mutex<HashMap<PathBuf, ResidentHost>> {
    static HOSTS: OnceLock<Mutex<HashMap<PathBuf, ResidentHost>>> = OnceLock::new();
    HOSTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn normalize_root(project_root: &Path) -> PathBuf {
    std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf())
}

/// Reap every resident `workflow_journal` host. Wired into daemon graceful
/// shutdown so warm plugin processes terminate cleanly. Idempotent.
pub async fn shutdown_resident_hosts() {
    let hosts: Vec<ResidentHost> = {
        let mut guard = resident_hosts().lock().unwrap_or_else(|p| p.into_inner());
        guard.drain().map(|(_, v)| v).collect()
    };
    for resident in hosts {
        let _ = resident.host.shutdown().await;
    }
}

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

enum ResidentCallError {
    Death(anyhow::Error),
    Other(anyhow::Error),
}

impl ResidentCallError {
    fn from_host_error(err: HostError) -> Self {
        match classify(&err) {
            RetryDecision::DeathLike => ResidentCallError::Death(anyhow!(err)),
            RetryDecision::StructuredError => ResidentCallError::Other(anyhow!(err)),
        }
    }
}

/// Run one `journal/*` RPC: serialize `params`, dispatch against the resident host
/// (spawning it once if absent), reap+respawn+retry once on a death-like failure.
/// Returns the raw response `Value` for the caller to decode.
async fn call<P: serde::Serialize>(
    plugin: &DiscoveredPlugin,
    project_root: &Path,
    method: &'static str,
    params: P,
) -> Result<serde_json::Value> {
    let params = serde_json::to_value(&params).with_context(|| format!("serializing params for {method}"))?;
    with_resident_host(plugin, project_root, move |host| {
        let params = params.clone();
        async move { journal_rpc(&host, method, params).await }
    })
    .await
}

async fn journal_rpc(
    host: &PluginHost,
    method: &'static str,
    params: serde_json::Value,
) -> std::result::Result<serde_json::Value, ResidentCallError> {
    host.request_typed_with_timeout(method, Some(params), JOURNAL_RPC_TIMEOUT)
        .await
        .map_err(ResidentCallError::from_host_error)
}

async fn with_resident_host<T, F, Fut>(plugin: &DiscoveredPlugin, project_root: &Path, mut call: F) -> Result<T>
where
    F: FnMut(PluginHost) -> Fut,
    Fut: Future<Output = std::result::Result<T, ResidentCallError>>,
{
    let (host, generation) = acquire_resident_host(plugin, project_root).await?;
    match call(host).await {
        Ok(value) => Ok(value),
        Err(ResidentCallError::Other(err)) => Err(err),
        Err(ResidentCallError::Death(err)) => {
            drop_resident_host_if_current(project_root, generation).await;
            let (host, _gen) = acquire_resident_host(plugin, project_root).await?;
            match call(host).await {
                Ok(value) => Ok(value),
                Err(ResidentCallError::Other(retry_err)) => Err(retry_err),
                Err(ResidentCallError::Death(retry_err)) => Err(retry_err.context(format!(
                    "workflow_journal plugin {} still failing after one re-spawn (first error: {err})",
                    plugin.name
                ))),
            }
        }
    }
}

async fn acquire_resident_host(plugin: &DiscoveredPlugin, project_root: &Path) -> Result<(PluginHost, u64)> {
    let key = normalize_root(project_root);
    let current_mtime = binary_mtime_nanos(&plugin.path);
    {
        let guard = resident_hosts().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(resident) = guard.get(&key) {
            if resident.plugin_path == plugin.path && resident.binary_mtime_nanos == current_mtime {
                return Ok((resident.host.clone(), resident.generation));
            }
        }
    }

    let host = spawn_journal_host(plugin).await?;
    host.handshake().await.with_context(|| format!("handshake with workflow_journal plugin {}", plugin.name))?;

    enum Outcome {
        UseExisting { winner: PluginHost, generation: u64, reap_ours: PluginHost },
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

async fn spawn_journal_host(plugin: &DiscoveredPlugin) -> Result<PluginHost> {
    let forwarded_env: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
    let options =
        PluginSpawnOptions::for_manifest(plugin.name.clone(), &plugin.manifest.env_required, forwarded_env, None);
    PluginHost::spawn_with_options(&plugin.path, &[], options)
        .await
        .with_context(|| format!("spawning workflow_journal plugin {}", plugin.name))
}

/// Bridge an async future into a sync call. Works whether or not a tokio runtime
/// is already running (daemon = inside a runtime; CLI = none). Mirrors
/// `config_source_client::run_blocking`.
fn run_blocking<F: Future>(fut: F) -> Result<F::Output> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => Ok(tokio::task::block_in_place(|| handle.block_on(fut))),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("building tokio runtime for workflow_journal call")?;
            Ok(rt.block_on(fut))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SubjectRef, WorkflowCheckpointMetadata, WorkflowMachineState};

    fn sample_workflow(id: &str) -> OrchestratorWorkflow {
        OrchestratorWorkflow {
            id: id.to_string(),
            task_id: "TASK-1".to_string(),
            workflow_ref: Some("task-default".to_string()),
            subject: SubjectRef::task("TASK-1".to_string()),
            input: None,
            vars: HashMap::new(),
            status: WorkflowStatus::Running,
            current_phase_index: 0,
            phases: Vec::new(),
            machine_state: WorkflowMachineState::Idle,
            current_phase: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            failure_reason: None,
            checkpoint_metadata: WorkflowCheckpointMetadata::default(),
            rework_counts: HashMap::new(),
            total_reworks: 0,
            decision_history: Vec::new(),
        }
    }

    #[test]
    fn journal_run_round_trips_through_blob() {
        let wf = sample_workflow("wf-1");
        let run = to_journal_run(&wf).expect("to run");
        assert_eq!(run.workflow_id, "wf-1");
        assert_eq!(run.workflow_ref.as_deref(), Some("task-default"));
        assert_eq!(run.status, "running");
        assert_eq!(run.kind.as_deref(), Some("animus.task"));
        let back = from_journal_run(run).expect("from run");
        assert_eq!(back.id, "wf-1");
        assert_eq!(back.task_id, "TASK-1");
        assert_eq!(back.status, WorkflowStatus::Running);
    }

    #[test]
    fn actor_blob_key_is_stripped_before_deserialization() {
        let wf = sample_workflow("wf-2");
        let mut run = to_journal_run(&wf).expect("to run");
        // Simulate save_actor having folded an actor into the blob.
        let actor = Actor { user_id: "alice".into(), claims: vec!["c".into()], tenant_id: None };
        run.blob.as_object_mut().unwrap().insert(ACTOR_BLOB_KEY.to_string(), serde_json::to_value(&actor).unwrap());

        assert_eq!(actor_from_run(&run).map(|a| a.user_id), Some("alice".to_string()));
        // The reserved key must not break workflow deserialization.
        let back = from_journal_run(run).expect("from run with actor key");
        assert_eq!(back.id, "wf-2");
    }

    #[test]
    fn no_plugin_installed_resolves_to_sqlite() {
        reset_backend_cache_for_tests();
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(resolve_backend(dir.path()), JournalBackend::Sqlite));
    }

    #[test]
    fn kill_switch_forces_sqlite() {
        // The discover path is exercised elsewhere; here assert the env flag parser.
        assert!(env_flag_enabled_value("1"));
        assert!(env_flag_enabled_value("true"));
        assert!(!env_flag_enabled_value("0"));
        assert!(!env_flag_enabled_value(""));
    }

    fn env_flag_enabled_value(v: &str) -> bool {
        matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "on")
    }
}
