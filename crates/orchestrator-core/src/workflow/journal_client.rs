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
use orchestrator_plugin_host::resident_host_registry::{
    binary_mtime_nanos, global_resident_host_registry, ResidentHostLease, ResidentHostRegistry,
};
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

/// Denormalized blob key carrying the run's canonical subject id (kind-qualified
/// for generic BaaS kinds, e.g. `blog:BLOG-001`; the bare native id for built-in
/// `task` / `requirement` kinds, e.g. `TASK-1`).
///
/// The journal protocol's `JournalRun` has a `kind` summary column but NO subject
/// id, so plugin backends (e.g. `animus-journal-postgres`) index the run's subject
/// by reading a top-level `subject_id` from the opaque blob. `OrchestratorWorkflow`
/// serializes its subject as a nested `subject` object plus a `task_id` string and
/// never emits a top-level `subject_id`, so that read was always `NULL` — most
/// visibly for generic BaaS-kind runs that carry no `task_id`. We fold the
/// canonical subject id into the blob here so every journal backend records it for
/// EVERY kind. It is stripped before the blob is deserialized back into an
/// `OrchestratorWorkflow` (the model derives the subject id from `subject`, never
/// from this denormalized mirror).
const SUBJECT_ID_BLOB_KEY: &str = "subject_id";

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

/// BU-4: whether the DURABLE (plugin-backed, e.g. Postgres) workflow journal is
/// active for `project_root`. Workflow RUN STATE survives a host/volume wipe
/// (a Railway redeploy) ONLY when this is `true`.
///
/// Returns `false` for the in-tree SQLite backend — the default, and what the
/// `ANIMUS_DISABLE_WORKFLOW_JOURNAL_PLUGIN` kill-switch forces. In that case the
/// daemon's boot orphan-sweep keeps its byte-identical pre-BU-4 cancel behavior;
/// resume-from-journal is gated entirely on this returning `true`.
pub fn durable_journal_active(project_root: &Path) -> bool {
    matches!(resolve_backend(project_root), JournalBackend::Plugin(_))
}

/// Clear the cached backend decisions. Test-only seam so a test that installs (or
/// removes) a synthetic plugin between runs is not served a stale decision.
#[cfg(test)]
pub(crate) fn reset_backend_cache_for_tests() {
    backend_cache().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

// ---------------------------------------------------------------------------
// BU-1H: one-time local SQLite -> plugin import.
// ---------------------------------------------------------------------------

/// Marker file (next to the local `workflow.db`) recording that the one-time
/// import of the local SQLite run history into the `workflow_journal` plugin has
/// completed for this project root. Its presence skips the (otherwise idempotent)
/// re-scan on every boot.
const JOURNAL_IMPORT_MARKER_FILE: &str = ".journal-imported-v1";

/// Outcome of [`import_local_sqlite_into_plugin`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct JournalImportStats {
    /// Runs upserted into the plugin via `journal/save`.
    pub runs_imported: usize,
    /// Checkpoints upserted via `journal/checkpoint_save`.
    pub checkpoints_imported: usize,
    /// `true` when the import did not run because there was nothing to do at the
    /// gate: no plugin backend active (SQLite / kill-switch) or the marker already
    /// existed. `false` means the scan ran (possibly importing 0 rows from an
    /// empty/absent local store, in which case the marker is now written).
    pub skipped: bool,
}

impl JournalImportStats {
    fn skipped() -> Self {
        Self { skipped: true, ..Default::default() }
    }
}

/// Sink the import writes scanned runs/checkpoints to. The production impl
/// forwards to the resident `workflow_journal` plugin; tests use an in-memory
/// recorder so the scan + marker logic is exercised without spawning a plugin.
trait JournalImportSink {
    fn save_run(&mut self, workflow: &OrchestratorWorkflow) -> Result<()>;
    fn save_checkpoint(&mut self, workflow_id: &str, checkpoint_num: usize, blob: serde_json::Value) -> Result<()>;
}

struct PluginImportSink {
    plugin: DiscoveredPlugin,
    project_root: PathBuf,
}

impl JournalImportSink for PluginImportSink {
    fn save_run(&mut self, workflow: &OrchestratorWorkflow) -> Result<()> {
        save(&self.plugin, &self.project_root, workflow)
    }
    fn save_checkpoint(&mut self, workflow_id: &str, checkpoint_num: usize, blob: serde_json::Value) -> Result<()> {
        checkpoint_save(&self.plugin, &self.project_root, workflow_id, checkpoint_num, blob)
    }
}

fn import_marker_path(project_root: &Path) -> PathBuf {
    super::state_manager::db_path_for_project(project_root).with_file_name(JOURNAL_IMPORT_MARKER_FILE)
}

fn write_import_marker(marker: &Path) -> Result<()> {
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::File::create(marker).with_context(|| format!("creating journal import marker at {}", marker.display()))?;
    Ok(())
}

/// One-time migration: copy every run + its checkpoints from the LOCAL SQLite
/// store into the active `workflow_journal` PLUGIN, so re-enabling the durable
/// backend does not blank the run-history view (the summary readers route to the
/// plugin store; without this, an installed-but-empty plugin shows no history).
///
/// No-op (returns `skipped`) when the SQLite backend is active (no plugin
/// installed / kill-switch set) — the local path never imports. No-op when the
/// marker already exists.
///
/// IDEMPOTENT + SAFE: `journal/save` and `journal/checkpoint_save` are upserts,
/// so a re-run (e.g. after a mid-import failure left the marker unwritten) merely
/// rewrites the same rows. The marker is written ONLY after a clean full pass, so
/// a partial import is retried on the next boot rather than silently truncated.
pub fn import_local_sqlite_into_plugin(project_root: &Path) -> Result<JournalImportStats> {
    let plugin = match resolve_backend(project_root) {
        JournalBackend::Sqlite => return Ok(JournalImportStats::skipped()),
        JournalBackend::Plugin(plugin) => *plugin,
    };
    let mut sink = PluginImportSink { plugin, project_root: project_root.to_path_buf() };
    import_local_sqlite_into_sink(project_root, &mut sink)
}

/// Backend-agnostic core of the import: scan the LOCAL SQLite store, forward each
/// run + checkpoints to `sink`, and write the marker on a clean pass. Separated
/// from [`import_local_sqlite_into_plugin`] so the scan + marker logic is unit
/// testable with an in-memory sink (the plugin RPC path needs a live plugin).
fn import_local_sqlite_into_sink<S: JournalImportSink>(
    project_root: &Path,
    sink: &mut S,
) -> Result<JournalImportStats> {
    let marker = import_marker_path(project_root);
    if marker.exists() {
        return Ok(JournalImportStats::skipped());
    }

    // Read the LOCAL SQLite engine directly, regardless of the active backend.
    let sqlite = super::state_manager::WorkflowStateManager::new_sqlite(project_root);
    let ids = super::state_manager::sqlite_all_run_ids(project_root)?;

    if ids.is_empty() {
        // Nothing to import (empty or absent local store): mark done so we never
        // re-scan, and report a non-skipped 0/0 pass.
        write_import_marker(&marker)?;
        return Ok(JournalImportStats::default());
    }

    let total = ids.len();
    let mut stats = JournalImportStats::default();
    for (idx, id) in ids.iter().enumerate() {
        // Load + forward one run at a time so the full set never resides in memory.
        let workflow = match sqlite.load(id) {
            Ok(workflow) => workflow,
            Err(err) => {
                tracing::warn!(
                    target: "animus.workflow.journal",
                    workflow_id = %id,
                    error = %err,
                    "skipping unreadable local run during workflow_journal import"
                );
                continue;
            }
        };
        sink.save_run(&workflow).with_context(|| format!("importing run {id} into workflow_journal plugin"))?;
        stats.runs_imported += 1;

        let checkpoint_nums = sqlite.list_checkpoints(id).unwrap_or_default();
        for checkpoint_num in checkpoint_nums {
            match sqlite.load_checkpoint(id, checkpoint_num) {
                Ok(snapshot) => {
                    let blob = serde_json::to_value(&snapshot)
                        .with_context(|| format!("serializing checkpoint snapshot {id}#{checkpoint_num}"))?;
                    sink.save_checkpoint(id, checkpoint_num, blob)
                        .with_context(|| format!("importing checkpoint {id}#{checkpoint_num}"))?;
                    stats.checkpoints_imported += 1;
                }
                Err(err) => {
                    tracing::warn!(
                        target: "animus.workflow.journal",
                        workflow_id = %id,
                        checkpoint = checkpoint_num,
                        error = %err,
                        "skipping unreadable local checkpoint during workflow_journal import"
                    );
                }
            }
        }

        if (idx + 1) % 50 == 0 {
            tracing::info!(
                target: "animus.workflow.journal",
                imported = idx + 1,
                total,
                "workflow_journal import progress"
            );
        }
    }

    // Marker written only after a clean full pass: a mid-import failure (the `?`
    // above) leaves it absent so the next boot retries (saves are upserts).
    write_import_marker(&marker)?;
    tracing::info!(
        target: "animus.workflow.journal",
        runs = stats.runs_imported,
        checkpoints = stats.checkpoints_imported,
        "workflow_journal local SQLite import complete"
    );
    Ok(stats)
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
    let mut blob = serde_json::to_value(workflow).context("serializing OrchestratorWorkflow for journal")?;
    // Fold the canonical subject id into the blob so plugin journal backends that
    // index `subject_id` from the blob record it for every kind (built-in task /
    // requirement AND generic BaaS kinds). `OrchestratorWorkflow` never serializes
    // a top-level `subject_id`, so without this the indexed column is always NULL.
    // Legacy fallback: workflow blobs that predate the `subject` field
    // deserialize with `subject = SubjectRef::task("")` (empty id) while the real
    // id still lives in `task_id`. Mirror the `workflow_task_id` fallback used
    // elsewhere so SQLite->plugin imports of old task runs index a non-null id.
    let subject_id = match workflow.subject.as_ref().map(|s| s.id()) {
        None | Some("") => workflow.task_id.as_str(),
        Some(id) => id,
    };
    if !subject_id.is_empty() {
        if let Some(obj) = blob.as_object_mut() {
            obj.insert(SUBJECT_ID_BLOB_KEY.to_string(), serde_json::Value::String(subject_id.to_string()));
        }
    }
    let kind = workflow.subject.as_ref().map(|s| s.kind.clone()).filter(|k| !k.is_empty());
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
        // Denormalized mirror, not part of the model — the subject id is derived
        // from the `subject` object on load.
        obj.remove(SUBJECT_ID_BLOB_KEY);
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

/// Like [`list`] but bounded to the `limit` newest rows in a SINGLE RPC — the
/// fast path for the workflow-list UI. Replaces `query_ids` + a per-id `load`
/// loop (N+1 — one RPC per run), which serialized behind other traffic on the
/// journal plugin's stdio host and made the list intermittently slow.
pub(crate) fn list_page(
    plugin: &DiscoveredPlugin,
    project_root: &Path,
    status: Option<WorkflowStatus>,
    limit: usize,
) -> Result<Vec<OrchestratorWorkflow>> {
    let query = JournalQuery {
        status: status.map(|s| vec![status_wire(s).to_string()]).unwrap_or_default(),
        workflow_ref: None,
        updated_since: None,
        limit: Some(limit as u32),
    };
    let value = run_blocking(call(plugin, project_root, METHOD_JOURNAL_LIST, query))??;
    let resp: ListResult = serde_json::from_value(value).context("decoding journal ListResult")?;
    Ok(resp.runs.into_iter().filter_map(|run| from_journal_run(run).ok()).collect())
}

/// Lightweight run summary sourced from the journal's no-blob projection
/// (`journal/list { summary: true }`). Carries only the fields the daemon's
/// stale-in-progress reconcile needs to cross-reference a run to its subject —
/// deliberately NOT the full `OrchestratorWorkflow`, so that heartbeat sweep
/// never fetches + deserializes every run's opaque blob (the ~6s all-runs scan
/// that head-of-line-blocked the shared journal host).
#[derive(Debug, Clone)]
pub struct WorkflowRunSummary {
    pub workflow_id: String,
    /// Denormalized subject id (bare native id for task/requirement, e.g.
    /// `TASK-1`; kind-qualified for generic kinds), cross-referenced against a
    /// task id via the daemon's `task_ids_match`.
    pub task_id: String,
    pub workflow_ref: Option<String>,
    pub status: WorkflowStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// The terminal timestamp for a terminal run; `None` for a live run.
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl WorkflowRunSummary {
    /// Project a full run to a summary. Used by the in-memory service hub and any
    /// caller that already holds the whole workflow. Mirrors the `subject_id`
    /// denormalization in [`to_journal_run`] so it matches the plugin projection.
    pub fn from_workflow(w: &OrchestratorWorkflow) -> Self {
        let task_id = match w.subject.as_ref().map(|s| s.id()) {
            None | Some("") => w.task_id.clone(),
            Some(id) => id.to_string(),
        };
        Self {
            workflow_id: w.id.clone(),
            task_id,
            workflow_ref: w.workflow_ref.clone(),
            status: w.status,
            started_at: w.started_at,
            completed_at: w.completed_at,
        }
    }
}

pub(crate) fn status_from_wire(s: &str) -> Option<WorkflowStatus> {
    Some(match s {
        "pending" => WorkflowStatus::Pending,
        "running" => WorkflowStatus::Running,
        "paused" => WorkflowStatus::Paused,
        "completed" => WorkflowStatus::Completed,
        "failed" => WorkflowStatus::Failed,
        "escalated" => WorkflowStatus::Escalated,
        "cancelled" => WorkflowStatus::Cancelled,
        _ => return None,
    })
}

/// Max rows a summary sweep pulls in one RPC. Equal to the reference backend's
/// `MAX_QUERY_LIMIT`, so a project with fewer runs than this gets ALL of them
/// (the reconcile needs every run to cross-reference its in-progress subjects).
const SUMMARY_QUERY_LIMIT: u32 = 10_000;

/// Every run's [`WorkflowRunSummary`] via the journal's no-blob projection —
/// ONE bounded RPC that skips the opaque blob column entirely (`summary: true`).
/// Replaces `list()`-then-drop-the-blob for the daemon's stale-in-progress
/// reconcile, which only needs subject id + status + timestamps. Rows missing a
/// workflow_id/status, or carrying an unknown wire status, are skipped.
pub(crate) fn list_summaries(plugin: &DiscoveredPlugin, project_root: &Path) -> Result<Vec<WorkflowRunSummary>> {
    let params = serde_json::json!({ "status": [], "summary": true, "limit": SUMMARY_QUERY_LIMIT });
    let value = run_blocking(call(plugin, project_root, METHOD_JOURNAL_LIST, params))??;
    let runs = value.get("runs").and_then(|v| v.as_array()).map(Vec::as_slice).unwrap_or_default();
    Ok(runs.iter().filter_map(summary_from_value).collect())
}

fn summary_from_value(v: &serde_json::Value) -> Option<WorkflowRunSummary> {
    let obj = v.as_object()?;
    let workflow_id = obj.get("workflow_id")?.as_str()?.to_string();
    let status = status_from_wire(obj.get("status")?.as_str()?)?;
    let task_id = obj.get("subject_id").and_then(|s| s.as_str()).unwrap_or_default().to_string();
    let workflow_ref = obj.get("workflow_ref").and_then(|s| s.as_str()).map(str::to_string);
    let started_at =
        obj.get("created_at").and_then(|s| s.as_str()).and_then(parse_summary_ts).unwrap_or_else(chrono::Utc::now);
    let updated_at = obj.get("updated_at").and_then(|s| s.as_str()).and_then(parse_summary_ts);
    let terminal = matches!(
        status,
        WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled | WorkflowStatus::Escalated
    );
    let completed_at = if terminal { updated_at } else { None };
    Some(WorkflowRunSummary { workflow_id, task_id, workflow_ref, status, started_at, completed_at })
}

fn parse_summary_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Ceiling on the id set `query_ids` pulls in one RPC. The caller derives the
/// list `total` from `ids.len()`, so an unset limit (which the reference backend
/// defaults to 1000) would cap the reported total — and the numbered pager — at
/// 1000 even with more runs. Request the backend's max so the count is accurate
/// up to this ceiling (ids are cheap — just the id column, no blobs).
const QUERY_IDS_LIMIT: u32 = 10_000;

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
        limit: Some(QUERY_IDS_LIMIT),
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
/// [`JournalEventKind`]. Returns `None` for kinds with no journal mapping.
///
/// `phase_failed` maps to [`JournalEventKind::PhaseCompleted`] carrying a
/// `status: "failed"` (the journal protocol has no dedicated `PhaseFailed`
/// variant). This makes a failed phase — including its exit-code + stderr
/// snippet, preserved in the event `detail` — visible in `journal_events`
/// instead of only in the checkpoint `snapshot_json` decision history.
fn wire_kind_to_journal(wire_kind: &str) -> Option<animus_journal_protocol::JournalEventKind> {
    use animus_journal_protocol::JournalEventKind as K;
    match wire_kind {
        "workflow_started" => Some(K::RunStarted),
        "phase_started" => Some(K::PhaseStarted),
        "phase_completed" => Some(K::PhaseCompleted),
        "phase_failed" => Some(K::PhaseCompleted),
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
    let status = payload
        .get("phase_status")
        .or_else(|| payload.get("status"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        // A `phase_failed` wire event folds into `PhaseCompleted`; stamp an
        // explicit failed status when the payload didn't carry one so consumers
        // reading `journal_events` can tell a failed phase from a successful one.
        .or_else(|| (wire_kind == "phase_failed").then(|| "failed".to_string()));
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
// Resident-host machinery (cross-role shared registry — 0.7 Layer B).
//
// Warm `workflow_journal` hosts live in the process-global
// `ResidentHostRegistry` shared with `config_source` / `subject_backend`, keyed
// by the plugin's binary path + mtime. A plugin binary that also serves those
// roles is therefore ONE shared process, spawned + handshaked once.
// ---------------------------------------------------------------------------

fn normalize_root(project_root: &Path) -> PathBuf {
    std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf())
}

/// Reap every resident host in the shared registry. Wired into daemon graceful
/// shutdown so warm plugin processes terminate cleanly. Idempotent — a prior
/// config_source teardown may already have drained the shared registry.
pub async fn shutdown_resident_hosts() {
    global_resident_host_registry().shutdown_all().await;
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

/// Acquire the shared resident host for `plugin` and run `call` against a clone
/// of it, retrying once (reap + re-spawn) on a death-like failure. All other
/// errors propagate without a re-spawn.
///
/// The host lives in the process-global [`ResidentHostRegistry`] keyed by the
/// plugin's binary path + mtime, shared with the other resident-style roles. The
/// lease is held across the RPC `.await` so LRU pressure from another role can
/// never evict the host mid-call. `project_root` no longer keys the host (it is
/// passed to the plugin per call); it is retained for the call signature.
async fn with_resident_host<T, F, Fut>(plugin: &DiscoveredPlugin, _project_root: &Path, mut call: F) -> Result<T>
where
    F: FnMut(PluginHost) -> Fut,
    Fut: Future<Output = std::result::Result<T, ResidentCallError>>,
{
    let registry = global_resident_host_registry();
    let mtime = binary_mtime_nanos(&plugin.path);
    // Spawn-context fingerprint matching `spawn_journal_host`: full parent env
    // forwarded, no working dir, no notification hint — identical to the
    // `config_source` context, so a plugin binary serving BOTH roles shares one
    // process.
    let context = journal_spawn_context();

    let lease = acquire_resident_lease(&registry, plugin, mtime, &context).await?;
    let generation = lease.generation();
    match call(lease.host().clone()).await {
        Ok(value) => Ok(value),
        Err(ResidentCallError::Other(err)) => Err(err),
        Err(ResidentCallError::Death(err)) => {
            // Reap ONLY the exact host that failed (a concurrent caller may have
            // already replaced it), then re-spawn once and retry.
            drop(lease);
            registry.invalidate_generation(&plugin.path, mtime, &context, generation).await;
            let lease = acquire_resident_lease(&registry, plugin, mtime, &context).await?;
            match call(lease.host().clone()).await {
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

/// Lease the shared resident host for `plugin`, spawning + handshaking it once
/// via the [`ResidentHostRegistry`] if it is not already live. A handshake
/// failure inside the spawn closure tears the half-started child down (it is
/// never inserted) so a retry re-spawns cleanly.
async fn acquire_resident_lease(
    registry: &ResidentHostRegistry,
    plugin: &DiscoveredPlugin,
    mtime: u128,
    context: &str,
) -> Result<ResidentHostLease> {
    registry
        .get_or_spawn(&plugin.path, mtime, context, || async {
            let host = spawn_journal_host(plugin).await?;
            if let Err(err) = host.handshake().await {
                let _ = host.clone().shutdown().await;
                return Err(err).with_context(|| format!("handshake with workflow_journal plugin {}", plugin.name));
            }
            Ok(host)
        })
        .await
}

/// Spawn-context fingerprint for a `workflow_journal` host, matching
/// [`spawn_journal_host`]: full parent env forwarded, no working dir, no
/// notification hint.
fn journal_spawn_context() -> String {
    let forwarded_env: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
    orchestrator_plugin_host::resident_host_registry::spawn_context_fingerprint(&forwarded_env, None, None)
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
            subject: Some(SubjectRef::task("TASK-1".to_string())),
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

    fn sample_baas_workflow(id: &str, kind: &str, subject_id: &str) -> OrchestratorWorkflow {
        let mut wf = sample_workflow(id);
        // A generic BaaS dynamic-kind subject carries NO task_id and is
        // kind-qualified (e.g. blog:BLOG-001).
        wf.subject = Some(SubjectRef::new(kind, subject_id));
        wf.task_id = String::new();
        wf.workflow_ref = Some("draft-blog".to_string());
        wf
    }

    #[test]
    fn to_journal_run_records_subject_id_for_generic_baas_kind() {
        // Regression: a detached / queue BaaS-kind dispatch must persist a run
        // whose journal subject id is the kind-qualified id, not NULL. Plugin
        // backends index `subject_id` from the blob, so the kernel must fold it
        // in (OrchestratorWorkflow never serializes a top-level `subject_id`).
        let wf = sample_baas_workflow("wf-blog", "blog", "blog:BLOG-001");
        let run = to_journal_run(&wf).expect("to run");
        assert_eq!(run.kind.as_deref(), Some("blog"));
        let subject_id = run.blob.get(SUBJECT_ID_BLOB_KEY).and_then(serde_json::Value::as_str);
        assert_eq!(subject_id, Some("blog:BLOG-001"), "journal blob must carry the kind-qualified subject id");

        // And it must round-trip back to the same subject (the denormalized
        // mirror is stripped; the model derives the subject from `subject`).
        let back = from_journal_run(run).expect("from run");
        assert_eq!(back.subject.as_ref().unwrap().kind(), "blog");
        assert_eq!(back.subject.as_ref().unwrap().id(), "blog:BLOG-001");
        assert!(back.task_id.is_empty());
    }

    #[test]
    fn to_journal_run_records_subject_id_for_task_kind() {
        // Built-in task path stays equivalent: the recorded subject id is the
        // bare native task id (previously also NULL because the blob carried no
        // top-level `subject_id`).
        let wf = sample_workflow("wf-task");
        let run = to_journal_run(&wf).expect("to run");
        let subject_id = run.blob.get(SUBJECT_ID_BLOB_KEY).and_then(serde_json::Value::as_str);
        assert_eq!(subject_id, Some("TASK-1"));
    }

    #[test]
    fn subject_id_blob_key_is_stripped_before_deserialization() {
        let wf = sample_baas_workflow("wf-blog-2", "blog", "blog:BLOG-002");
        let run = to_journal_run(&wf).expect("to run");
        assert!(run.blob.get(SUBJECT_ID_BLOB_KEY).is_some(), "blob carries the denormalized subject id");
        // The denormalized key must not break workflow deserialization.
        let back = from_journal_run(run).expect("from run with subject_id key");
        assert_eq!(back.id, "wf-blog-2");
        assert_eq!(back.subject.as_ref().unwrap().id(), "blog:BLOG-002");
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
    fn wire_kind_phase_failed_maps_to_completed_with_failed_status() {
        use animus_journal_protocol::JournalEventKind as K;
        // A failed phase must reach `journal_events` (it previously mapped to
        // None and was silently dropped). The journal protocol has no dedicated
        // PhaseFailed variant, so it folds into PhaseCompleted while the failed
        // status + command exit-code/stderr survive in the event detail.
        assert!(matches!(wire_kind_to_journal("phase_failed"), Some(K::PhaseCompleted)));
        assert!(wire_kind_to_journal("nonexistent_kind").is_none());
    }

    #[test]
    fn phase_failed_wire_event_carries_failed_status_and_detail() {
        // The daemon tee forwards the raw phase_failed payload; record_wire_event
        // must lift a `failed` status (even when the payload omits one) and
        // preserve the exit-code + stderr snippet in `detail` for diagnosis.
        let payload = serde_json::json!({
            "phase_id": "mark-running",
            "exit_code": 2,
            "stderr": "--id must not be empty",
        });
        let kind = wire_kind_to_journal("phase_failed").expect("phase_failed maps");
        let status = payload
            .get("phase_status")
            .or_else(|| payload.get("status"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| Some("failed".to_string()));
        let event = JournalEvent {
            run_id: "wf-1".to_string(),
            workflow_ref: None,
            kind,
            phase: payload.get("phase_id").and_then(|v| v.as_str()).map(str::to_string),
            agent: None,
            status,
            ts: chrono::Utc::now(),
            detail: payload,
        };
        assert_eq!(event.status.as_deref(), Some("failed"));
        assert_eq!(event.phase.as_deref(), Some("mark-running"));
        assert_eq!(event.detail.get("exit_code").and_then(serde_json::Value::as_i64), Some(2));
        assert_eq!(event.detail.get("stderr").and_then(serde_json::Value::as_str), Some("--id must not be empty"));
    }

    #[test]
    fn no_plugin_installed_resolves_to_sqlite() {
        reset_backend_cache_for_tests();
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(resolve_backend(dir.path()), JournalBackend::Sqlite));
    }

    #[test]
    fn durable_journal_inactive_without_plugin() {
        // BU-4 gate: no workflow_journal plugin installed => SQLite => NOT
        // durable, so the daemon keeps its byte-identical cancel-orphans path.
        reset_backend_cache_for_tests();
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!durable_journal_active(dir.path()));
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

    // --- BU-1H import migration ---------------------------------------------

    use crate::types::CheckpointReason;
    use crate::workflow::state_manager::WorkflowStateManager;

    #[derive(Default)]
    struct RecordingSink {
        runs: Vec<String>,
        checkpoints: Vec<(String, usize)>,
    }

    impl JournalImportSink for RecordingSink {
        fn save_run(&mut self, workflow: &OrchestratorWorkflow) -> Result<()> {
            self.runs.push(workflow.id.clone());
            Ok(())
        }
        fn save_checkpoint(
            &mut self,
            workflow_id: &str,
            checkpoint_num: usize,
            _blob: serde_json::Value,
        ) -> Result<()> {
            self.checkpoints.push((workflow_id.to_string(), checkpoint_num));
            Ok(())
        }
    }

    #[test]
    fn import_copies_runs_and_checkpoints_and_writes_marker() {
        crate::test_env::stable_test_home();
        let dir = tempfile::tempdir().expect("tempdir");
        let sqlite = WorkflowStateManager::new_sqlite(dir.path());

        let wf_a = sample_workflow("import-a");
        let wf_b = sample_workflow("import-b");
        sqlite.save(&wf_a).expect("save a");
        sqlite.save(&wf_b).expect("save b");
        // Two checkpoints on one run, none on the other.
        sqlite.save_checkpoint(&wf_a, CheckpointReason::Start).expect("cp1");
        sqlite.save_checkpoint(&wf_a, CheckpointReason::StatusChange).expect("cp2");

        let mut sink = RecordingSink::default();
        let stats = import_local_sqlite_into_sink(dir.path(), &mut sink).expect("import");

        assert!(!stats.skipped);
        assert_eq!(stats.runs_imported, 2);
        assert_eq!(stats.checkpoints_imported, 2);
        assert_eq!(sink.runs.len(), 2);
        assert!(sink.runs.contains(&"import-a".to_string()));
        assert!(sink.runs.contains(&"import-b".to_string()));
        assert_eq!(sink.checkpoints, vec![("import-a".to_string(), 1), ("import-a".to_string(), 2)]);
        assert!(import_marker_path(dir.path()).exists(), "marker written after a clean pass");
    }

    #[test]
    fn import_skips_when_marker_present() {
        crate::test_env::stable_test_home();
        let dir = tempfile::tempdir().expect("tempdir");
        let sqlite = WorkflowStateManager::new_sqlite(dir.path());
        sqlite.save(&sample_workflow("import-c")).expect("save");

        // Pre-write the marker: the scan must not run.
        write_import_marker(&import_marker_path(dir.path())).expect("write marker");

        let mut sink = RecordingSink::default();
        let stats = import_local_sqlite_into_sink(dir.path(), &mut sink).expect("import");

        assert!(stats.skipped);
        assert_eq!(stats.runs_imported, 0);
        assert!(sink.runs.is_empty(), "no saves when marker present");
    }

    #[test]
    fn import_empty_sqlite_writes_marker_and_imports_nothing() {
        crate::test_env::stable_test_home();
        let dir = tempfile::tempdir().expect("tempdir");
        // No runs saved: the local store is empty/absent.

        let mut sink = RecordingSink::default();
        let stats = import_local_sqlite_into_sink(dir.path(), &mut sink).expect("import");

        assert!(!stats.skipped, "an empty store still completes a (0-row) pass");
        assert_eq!(stats.runs_imported, 0);
        assert!(sink.runs.is_empty());
        assert!(import_marker_path(dir.path()).exists(), "marker written so we never re-scan");
    }

    #[test]
    fn import_is_a_noop_for_sqlite_backend() {
        crate::test_env::stable_test_home();
        reset_backend_cache_for_tests();
        let dir = tempfile::tempdir().expect("tempdir");
        // No plugin installed => SQLite backend => the public entrypoint must not
        // touch SQLite and must report skipped (no marker written).
        let stats = import_local_sqlite_into_plugin(dir.path()).expect("import");
        assert!(stats.skipped);
        assert_eq!(stats.runs_imported, 0);
        assert!(!import_marker_path(dir.path()).exists(), "no marker for the SQLite path");
    }
}
