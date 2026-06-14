// Wave 3 v0.5 plugin-host wrappers: most helpers are wired through one
// strategic call site each in this Wave (see "Wave 3 — In scope" item 1
// in docs/architecture/v0.5-execution-plan.md), while the remaining
// helpers stay available for follow-on call-site migrations in v0.5.x.
#![allow(dead_code)]

//! Outbound RPC clients for v0.5 `workflow_runner` and `queue` plugins.
//!
//! Wave 3 of the v0.5 release routes `workflow/execute`, `workflow/run_phase`,
//! and `queue/*` RPCs through plugin host calls instead of the in-tree
//! `animus_runtime_shared` crate and the `orchestrator_daemon_runtime::queue`
//! module. This module provides thin per-call wrappers that:
//!
//! 1. Discover whether a `workflow_runner` / `queue` plugin is installed.
//! 2. Spawn the plugin process (one process per call — caching is deferred
//!    to v0.5.1 once preflight confirms the plugin-only path is stable).
//! 3. Issue a custom `initialize` request that includes the v0.5
//!    `init_extensions.project_binding.project_root` field so the plugin
//!    binds to the correct project root, then issue the typed method call.
//!
//! Each entry point returns `Ok(None)` when no matching plugin is installed
//! so existing callers fall back to the in-tree code path. The in-tree
//! `animus_runtime_shared` crate and `orchestrator_daemon_runtime::queue` module
//! are intentionally retained in v0.5 — deletion is a v0.5.x follow-up after
//! preflight confirms the plugin-only path is stable. (See
//! `docs/architecture/v0.5-execution-plan.md` "Wave 3 — Out of scope" for the
//! deletion gate.)

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use orchestrator_plugin_host::{
    DiscoveredPlugin, PluginDiscovery, PluginHost, PluginSpawnOptions, PLUGIN_BASE_ENV_ALLOWLIST,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use animus_queue_protocol as queue_proto;
use animus_workflow_runner_protocol as workflow_proto;

/// Plugin-kind constant for `workflow_runner`. The in-tree
/// `animus-plugin-protocol` crate is still on protocol v1.0 and does NOT
/// export this constant; the v0.5 protocol crate (transitively via
/// `animus-workflow-runner-protocol`) defines it as the wire literal.
const PLUGIN_KIND_WORKFLOW_RUNNER: &str = "workflow_runner";

/// Plugin-kind constant for `queue`. Same rationale as
/// [`PLUGIN_KIND_WORKFLOW_RUNNER`].
const PLUGIN_KIND_QUEUE: &str = "queue";

/// Per-call default timeout for plugin RPCs. Workflow execution can be
/// long-running so the workflow-runner timeout is generous; queue ops use
/// the short timeout.
const PLUGIN_CALL_TIMEOUT_SHORT: Duration = Duration::from_secs(30);
const PLUGIN_CALL_TIMEOUT_LONG: Duration = Duration::from_hours(1);

/// Discover a plugin by `plugin_kind` value from its manifest.
fn find_plugin_for_kind(project_root: &Path, plugin_kind: &str) -> Result<Option<DiscoveredPlugin>> {
    let discovered = PluginDiscovery::new()
        .with_project_root(project_root.to_path_buf())
        .discover()
        .context("plugin discovery failed")?;
    Ok(discovered.into_iter().find(|p| p.manifest.plugin_kind == plugin_kind))
}

/// Spawn a plugin process and run the v0.5 initialize handshake with the
/// project_binding extension. Returns the spawned [`PluginHost`].
///
/// The plugin is left running; the caller is responsible for sending
/// follow-up RPCs and either calling [`PluginHost::shutdown`] or dropping
/// the host (which severs stdio). For demo-quality v0.5 we spawn-per-call
/// and shut down at the end of each helper.
async fn spawn_with_project_binding(plugin: &DiscoveredPlugin, project_root: &Path) -> Result<PluginHost> {
    let options = PluginSpawnOptions::for_manifest(
        plugin.name.clone(),
        &plugin.manifest.env_required,
        PLUGIN_BASE_ENV_ALLOWLIST.iter().map(|s| (*s).to_string()),
        None,
    )
    .with_notification_buffer_hint(plugin.manifest.notification_buffer_size)
    .with_working_dir(project_root);

    let host = PluginHost::spawn_with_options(&plugin.path, &[], options)
        .await
        .with_context(|| format!("failed to spawn plugin '{}' at {}", plugin.name, plugin.path.display()))?;

    // Custom initialize that includes init_extensions.project_binding per
    // the v0.5 protocol §"Common conventions" / "Project-scope binding".
    // Codex R6 [P1]: send the kernel-computed `repo_scope` so plugins
    // that validate it or use it for scoped runtime state read/write
    // the right state slot rather than an empty default.
    //
    // `memory_mcp_stdio_command` (v0.5 P2 fold-in for
    // `animus-workflow-runner-default` v0.1.0): supply the daemon's own
    // animus binary path so the plugin can inject memory MCP injection
    // without depending on a sibling-binary search at the plugin's
    // install location. Plugins that don't recognize this extension
    // ignore it.
    let repo_scope = protocol::repository_scope_for_path(project_root);
    let mut init_extensions = serde_json::Map::new();
    init_extensions.insert(
        "project_binding".to_string(),
        json!({
            "project_root": project_root.to_string_lossy(),
            "repo_scope": repo_scope,
        }),
    );
    if let Ok(self_path) = std::env::current_exe() {
        init_extensions
            .insert("memory_mcp_stdio_command".to_string(), json!({ "command": self_path.to_string_lossy() }));
    }
    let init_params = json!({
        "protocol_version": "1.1.0",
        "host_info": { "name": "animus", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "streaming": true, "progress": true, "cancellation": true },
        "init_extensions": init_extensions,
    });

    host.request_typed_with_timeout("initialize", Some(init_params), PLUGIN_CALL_TIMEOUT_SHORT)
        .await
        .with_context(|| format!("plugin '{}' initialize failed", plugin.name))?;

    host.notify("initialized", None)
        .await
        .with_context(|| format!("plugin '{}' initialized notification failed", plugin.name))?;

    Ok(host)
}

async fn shutdown_quiet(host: PluginHost) {
    if let Err(error) = host.shutdown().await {
        tracing::warn!(%error, "plugin shutdown errored");
    }
}

// ----- Workflow runner -----

/// Wrapper around the v0.5 `workflow/execute` RPC.
///
/// Returns `Ok(None)` if no `workflow_runner` plugin is installed; callers
/// fall back to the in-tree `animus_runtime_shared::execute_workflow`.
pub async fn call_workflow_execute(
    project_root: &Path,
    request: &workflow_proto::WorkflowExecuteRequest,
) -> Result<Option<workflow_proto::WorkflowExecuteResult>> {
    let Some(plugin) = find_plugin_for_kind(project_root, PLUGIN_KIND_WORKFLOW_RUNNER)? else {
        return Ok(None);
    };
    let host = spawn_with_project_binding(&plugin, project_root).await?;

    let params = Some(serde_json::to_value(request).context("failed to encode WorkflowExecuteRequest")?);
    let value = host
        .request_typed_with_timeout(workflow_proto::METHOD_WORKFLOW_EXECUTE, params, PLUGIN_CALL_TIMEOUT_LONG)
        .await
        .with_context(|| format!("workflow_runner plugin '{}' workflow/execute failed", plugin.name));

    let result = match value {
        Ok(v) => serde_json::from_value::<workflow_proto::WorkflowExecuteResult>(v)
            .context("failed to decode WorkflowExecuteResult")?,
        Err(error) => {
            shutdown_quiet(host).await;
            return Err(error);
        }
    };

    shutdown_quiet(host).await;
    Ok(Some(result))
}

/// Wrapper around the v0.5 `workflow/run_phase` RPC. See
/// [`call_workflow_execute`] for the fallback contract.
pub async fn call_workflow_run_phase(
    project_root: &Path,
    request: &workflow_proto::WorkflowPhaseRunRequest,
) -> Result<Option<workflow_proto::WorkflowPhaseRunResult>> {
    let Some(plugin) = find_plugin_for_kind(project_root, PLUGIN_KIND_WORKFLOW_RUNNER)? else {
        return Ok(None);
    };
    let host = spawn_with_project_binding(&plugin, project_root).await?;

    let params = Some(serde_json::to_value(request).context("failed to encode WorkflowPhaseRunRequest")?);
    let value = host
        .request_typed_with_timeout(workflow_proto::METHOD_WORKFLOW_RUN_PHASE, params, PLUGIN_CALL_TIMEOUT_LONG)
        .await
        .with_context(|| format!("workflow_runner plugin '{}' workflow/run_phase failed", plugin.name));

    let result = match value {
        Ok(v) => serde_json::from_value::<workflow_proto::WorkflowPhaseRunResult>(v)
            .context("failed to decode WorkflowPhaseRunResult")?,
        Err(error) => {
            shutdown_quiet(host).await;
            return Err(error);
        }
    };

    shutdown_quiet(host).await;
    Ok(Some(result))
}

// ----- Queue -----

async fn queue_call<T: for<'de> Deserialize<'de>>(
    project_root: &Path,
    method: &str,
    params: Option<Value>,
) -> Result<Option<T>> {
    let Some(plugin) = find_plugin_for_kind(project_root, PLUGIN_KIND_QUEUE)? else {
        return Ok(None);
    };
    let host = spawn_with_project_binding(&plugin, project_root).await?;

    let value = host
        .request_typed_with_timeout(method, params, PLUGIN_CALL_TIMEOUT_SHORT)
        .await
        .with_context(|| format!("queue plugin '{}' {} failed", plugin.name, method));

    let decoded = match value {
        Ok(v) => serde_json::from_value::<T>(v).with_context(|| format!("failed to decode {method} response"))?,
        Err(error) => {
            shutdown_quiet(host).await;
            return Err(error);
        }
    };

    shutdown_quiet(host).await;
    Ok(Some(decoded))
}

/// `queue/lease` — atomically claim up to `max` pending entries and transition
/// them to `assigned`. Daemon dispatch hot path per the Brief F handoff state
/// in `docs/architecture/v0.5-execution-plan.md`.
///
/// Per the wire contract: if `workflow_ids` is `Some`, its length MUST equal
/// `max` (otherwise the plugin returns
/// `QUEUE_LEASE_WORKFLOW_ID_COUNT_MISMATCH`).
pub async fn call_queue_lease(
    project_root: &Path,
    request: &queue_proto::QueueLeaseRequest,
) -> Result<Option<queue_proto::QueueLeaseResponse>> {
    let params = Some(serde_json::to_value(request).context("failed to encode QueueLeaseRequest")?);
    queue_call(project_root, queue_proto::METHOD_QUEUE_LEASE, params).await
}

/// `queue/list` — list queue entries, optionally filtered by status.
pub async fn call_queue_list(
    project_root: &Path,
    request: &queue_proto::QueueListRequest,
) -> Result<Option<queue_proto::QueueListResponse>> {
    let params = Some(serde_json::to_value(request).context("failed to encode QueueListRequest")?);
    queue_call(project_root, queue_proto::METHOD_QUEUE_LIST, params).await
}

/// `queue/stats` — pending / assigned / held counts.
pub async fn call_queue_stats(project_root: &Path) -> Result<Option<queue_proto::QueueStats>> {
    queue_call(project_root, queue_proto::METHOD_QUEUE_STATS, Some(json!({}))).await
}

/// `queue/next_deadline` — earliest future `run_at` across pending deferred
/// entries, for the daemon's precise-wake loop. `Ok(None)` when no queue
/// plugin is installed; the inner `next_run_at` is `None` when the queue
/// holds no future-dated entries.
pub async fn call_queue_next_deadline(project_root: &Path) -> Result<Option<queue_proto::QueueNextDeadlineResponse>> {
    queue_call(project_root, queue_proto::METHOD_QUEUE_NEXT_DEADLINE, Some(json!({}))).await
}

/// `queue/enqueue` — append a [`SubjectDispatch`] to the queue.
pub async fn call_queue_enqueue(
    project_root: &Path,
    request: &queue_proto::QueueEnqueueRequest,
) -> Result<Option<queue_proto::QueueEnqueueResponse>> {
    let params = Some(serde_json::to_value(request).context("failed to encode QueueEnqueueRequest")?);
    queue_call(project_root, queue_proto::METHOD_QUEUE_ENQUEUE, params).await
}

/// `queue/hold` — pause a pending entry.
pub async fn call_queue_hold(
    project_root: &Path,
    request: &queue_proto::QueueHoldRequest,
) -> Result<Option<queue_proto::QueueMutationResponse>> {
    let params = Some(serde_json::to_value(request).context("failed to encode QueueHoldRequest")?);
    queue_call(project_root, queue_proto::METHOD_QUEUE_HOLD, params).await
}

/// `queue/release` — release a held entry back to pending.
pub async fn call_queue_release(
    project_root: &Path,
    request: &queue_proto::QueueReleaseRequest,
) -> Result<Option<queue_proto::QueueMutationResponse>> {
    let params = Some(serde_json::to_value(request).context("failed to encode QueueReleaseRequest")?);
    queue_call(project_root, queue_proto::METHOD_QUEUE_RELEASE, params).await
}

/// `queue/drop` — remove an entry from the queue.
pub async fn call_queue_drop(
    project_root: &Path,
    request: &queue_proto::QueueDropRequest,
) -> Result<Option<queue_proto::QueueMutationResponse>> {
    let params = Some(serde_json::to_value(request).context("failed to encode QueueDropRequest")?);
    queue_call(project_root, queue_proto::METHOD_QUEUE_DROP, params).await
}

/// `queue/reorder` — re-rank pending entries.
pub async fn call_queue_reorder(
    project_root: &Path,
    request: &queue_proto::QueueReorderRequest,
) -> Result<Option<queue_proto::QueueReorderResponse>> {
    let params = Some(serde_json::to_value(request).context("failed to encode QueueReorderRequest")?);
    queue_call(project_root, queue_proto::METHOD_QUEUE_REORDER, params).await
}

/// `queue/mark_assigned` — flip a pending entry to assigned without a lease
/// round-trip (used by tests; the production dispatch path uses
/// [`call_queue_lease`]).
pub async fn call_queue_mark_assigned(
    project_root: &Path,
    request: &queue_proto::QueueMarkAssignedRequest,
) -> Result<Option<queue_proto::QueueMutationResponse>> {
    let params = Some(serde_json::to_value(request).context("failed to encode QueueMarkAssignedRequest")?);
    queue_call(project_root, queue_proto::METHOD_QUEUE_MARK_ASSIGNED, params).await
}

/// `queue/completion` — final state transition for an assigned entry.
pub async fn call_queue_completion(
    project_root: &Path,
    request: &queue_proto::QueueCompletionRequest,
) -> Result<Option<queue_proto::QueueMutationResponse>> {
    let params = Some(serde_json::to_value(request).context("failed to encode QueueCompletionRequest")?);
    queue_call(project_root, queue_proto::METHOD_QUEUE_COMPLETION, params).await
}

/// `queue/release_pending` — return an Assigned entry to Pending without
/// canceling it. Used when the daemon discovers the leased subject is
/// already being worked on by another in-flight workflow and the entry
/// should remain queued for a future tick. Available in
/// `animus-queue-protocol` 0.2.0 / `animus-queue-default` v0.2.0+; older
/// plugins return JSON-RPC method-not-found.
pub async fn call_queue_release_pending(
    project_root: &Path,
    entry_id: &str,
    reason: &str,
) -> Result<Option<queue_proto::QueueReleasePendingResponse>> {
    let request = queue_proto::QueueReleasePendingParams { entry_id: entry_id.to_string(), reason: reason.to_string() };
    let params = Some(serde_json::to_value(&request).context("failed to encode QueueReleasePendingParams")?);
    queue_call(project_root, queue_proto::METHOD_QUEUE_RELEASE_PENDING, params).await
}

/// Lightweight check used by `animus daemon health` / `animus status` to
/// detect whether the active flavor's required plugins are installed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePluginRoles {
    pub workflow_runner: bool,
    pub queue: bool,
}

/// Probe the discovery surface and report which v0.5 plugin roles are
/// satisfied. Used by daemon health output. (Not load-bearing in the call
/// path itself; routes still fall back to in-tree.)
pub fn probe_active_plugin_roles(project_root: &Path) -> Result<ActivePluginRoles> {
    let discovered = PluginDiscovery::new()
        .with_project_root(project_root.to_path_buf())
        .discover()
        .context("plugin discovery failed")?;
    let mut workflow_runner = false;
    let mut queue = false;
    for plugin in &discovered {
        match plugin.manifest.plugin_kind.as_str() {
            PLUGIN_KIND_WORKFLOW_RUNNER => workflow_runner = true,
            PLUGIN_KIND_QUEUE => queue = true,
            _ => {}
        }
    }
    Ok(ActivePluginRoles { workflow_runner, queue })
}

pub(crate) fn workflow_runner_kind() -> &'static str {
    PLUGIN_KIND_WORKFLOW_RUNNER
}

pub(crate) fn queue_kind() -> &'static str {
    PLUGIN_KIND_QUEUE
}
