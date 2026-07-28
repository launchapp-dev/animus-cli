//! CLI-side `QueueRouting` adapter — forwards each control-protocol
//! queue verb to the installed `queue` plugin via the
//! `animus-queue-protocol` RPC surface.
//!
//! Build one of these at daemon startup via [`build_queue_routing`] and
//! pass the resulting `Arc<dyn QueueRouting>` into
//! [`orchestrator_daemon_runtime::control::InProcessSurfaceBuilder::queue_routing`].
//!
//! ## Shape conversion
//!
//! The control protocol wire types (e.g.
//! [`animus_control_protocol::types::QueueEntry`]) carry a small subset
//! of the plugin's richer per-entry shape
//! ([`animus_queue_protocol::QueueEntry`]). The adapter does the lossy
//! projection here so wire callers (MCP / WebAPI) see a stable wire
//! contract while the local CLI path still has the full envelope.
//!
//! ## Plugin discovery
//!
//! Each call invokes [`crate::services::plugin_clients::call_queue_*`]
//! which discovers the installed queue plugin, spawns it, runs the
//! initialize handshake, issues the RPC, then shuts down. The preflight
//! gate enforces queue plugin presence at daemon startup so a missing
//! plugin should never happen in production — defensively we still
//! return a typed `ControlError::Unavailable` if the discovery comes
//! back empty.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use animus_control_protocol::{
    types::{
        QueueDropRequest as WireDropRequest, QueueEnqueueRequest as WireEnqueueRequest, QueueEntry as WireQueueEntry,
        QueueEntryStatus as WireQueueEntryStatus, QueueHoldRequest as WireHoldRequest,
        QueueListRequest as WireListRequest, QueueListResponse as WireListResponse,
        QueueReleaseRequest as WireReleaseRequest, QueueReorderPosition as WireReorderPosition,
        QueueReorderRequest as WireReorderRequest, QueueStats as WireQueueStats, Unit,
    },
    ControlError,
};
use animus_subject_protocol_wire::SubjectId;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use orchestrator_core::{workflow_ref_for_task, FileServiceHub, ServiceHub};
use orchestrator_daemon_runtime::control::QueueRouting;
use protocol::SubjectDispatch;

use animus_queue_protocol as queue_proto;

use crate::services::plugin_clients;

const FORWARD_LOG_TARGET: &str = "animus::control::queue_routing";

pub fn build_queue_routing(project_root: PathBuf) -> Arc<dyn QueueRouting> {
    Arc::new(QueueRoutingImpl { project_root })
}

struct QueueRoutingImpl {
    project_root: PathBuf,
}

impl QueueRoutingImpl {
    fn project_root_path(&self) -> &Path {
        self.project_root.as_path()
    }

    fn project_root_str(&self) -> String {
        self.project_root.to_string_lossy().to_string()
    }

    async fn hub(&self) -> Result<Arc<dyn ServiceHub>, ControlError> {
        let hub = FileServiceHub::new(&self.project_root_str())
            .map_err(|err| ControlError::Internal(format!("queue routing: hub init: {err:#}")))?;
        Ok(Arc::new(hub))
    }
}

fn plugin_missing(verb: &str) -> ControlError {
    ControlError::Unavailable(format!(
        "queue/{verb} routing requires the `queue` plugin role - run `animus plugin install-defaults` (or install `launchapp-dev/animus-queue-default`) and retry"
    ))
}

fn internal_err(err: anyhow::Error) -> ControlError {
    ControlError::Internal(format!("{err:#}"))
}

/// Build a [`SubjectRef`] from a queue `task_id`, honoring the
/// `<kind>:<native>` qualifier so plugin-backed custom kinds resolve through
/// their own `<kind>/get` method. `task:`/`requirement:` map to the canonical
/// in-tree constructors; any other non-empty prefix becomes a custom-kind ref;
/// a bare id (no prefix) defaults to task for backward compatibility.
fn subject_ref_from_qualified_id(task_id: &str) -> protocol::orchestrator::SubjectRef {
    use protocol::orchestrator::SubjectRef;
    match task_id.split_once(':') {
        Some((kind, _)) if kind.eq_ignore_ascii_case("task") => SubjectRef::task(task_id.to_string()),
        Some((kind, _)) if kind.eq_ignore_ascii_case("requirement") => SubjectRef::requirement(task_id.to_string()),
        Some((kind, _)) if !kind.is_empty() => SubjectRef::new(kind.to_string(), task_id.to_string()),
        _ => SubjectRef::task(task_id.to_string()),
    }
}

fn mutation_to_result(
    verb: &str,
    entry_id: &str,
    resp: queue_proto::QueueMutationResponse,
) -> Result<Unit, ControlError> {
    if resp.not_found {
        return Err(ControlError::NotFound(format!("queue/{verb}: entry `{entry_id}` not found")));
    }
    if !resp.changed {
        return Err(ControlError::InvalidRequest(format!(
            "queue/{verb}: entry `{entry_id}` was not in the expected state"
        )));
    }
    Ok(Unit::default())
}

fn plugin_status_to_wire(status: &str) -> WireQueueEntryStatus {
    match status {
        queue_proto::status::PENDING => WireQueueEntryStatus::Ready,
        queue_proto::status::ASSIGNED => WireQueueEntryStatus::InFlight,
        queue_proto::status::HELD => WireQueueEntryStatus::Held,
        _ => WireQueueEntryStatus::Ready,
    }
}

fn wire_status_to_plugin(status: WireQueueEntryStatus) -> Option<&'static str> {
    match status {
        WireQueueEntryStatus::Ready => Some(queue_proto::status::PENDING),
        WireQueueEntryStatus::InFlight => Some(queue_proto::status::ASSIGNED),
        WireQueueEntryStatus::Held => Some(queue_proto::status::HELD),
        WireQueueEntryStatus::Done | WireQueueEntryStatus::Dropped => None,
    }
}

fn parse_enqueued_at(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw).map(|dt| dt.with_timezone(&Utc)).unwrap_or_else(|_| Utc::now())
}

fn priority_label_to_u8(label: Option<&str>) -> u8 {
    match label.map(|s| s.to_ascii_lowercase()) {
        Some(ref s) if s == "critical" => 4,
        Some(ref s) if s == "high" => 3,
        Some(ref s) if s == "medium" => 2,
        Some(ref s) if s == "low" => 1,
        Some(ref s) if s == "none" => 0,
        _ => 2,
    }
}

fn priority_u8_to_label(value: u8) -> &'static str {
    match value {
        0 => "none",
        1 => "low",
        2 => "medium",
        3 => "high",
        _ => "critical",
    }
}

fn plugin_entry_to_wire(entry: queue_proto::QueueEntry) -> WireQueueEntry {
    let status = plugin_status_to_wire(entry.status.as_str());
    let enqueued_at = parse_enqueued_at(&entry.enqueued_at);
    let priority = priority_label_to_u8(entry.subject_dispatch.priority.as_deref());
    WireQueueEntry {
        id: entry.entry_id,
        subject_id: SubjectId::new(entry.subject_id),
        status,
        priority,
        enqueued_at,
        hold_reason: None,
    }
}

fn plugin_stats_to_wire(stats: queue_proto::QueueStats) -> WireQueueStats {
    WireQueueStats {
        // `pending` includes deferred entries that are not yet leasable, so
        // subtract them: "ready" must mean dispatchable-now. `deferred` is a
        // subset of `pending`, but saturate defensively against bad backends.
        ready: stats.pending.saturating_sub(stats.deferred) as u64,
        held: stats.held as u64,
        in_flight: stats.assigned as u64,
        done_recent: 0,
        dropped_recent: 0,
    }
}

#[async_trait]
impl QueueRouting for QueueRoutingImpl {
    async fn queue_list(&self, request: WireListRequest) -> Result<WireListResponse, ControlError> {
        tracing::info!(target: FORWARD_LOG_TARGET, "forwarding queue/list to queue plugin");
        let mut plugin_status = Vec::new();
        if let Some(status) = request.status {
            if let Some(plugin) = wire_status_to_plugin(status) {
                plugin_status.push(plugin.to_string());
            } else {
                return Ok(WireListResponse { entries: Vec::new(), next_cursor: None });
            }
        }
        let offset = request.cursor.as_deref().and_then(|c| c.parse::<usize>().ok());
        let plugin_request =
            queue_proto::QueueListRequest { status: plugin_status, limit: request.limit.map(|v| v as usize), offset };
        let response = plugin_clients::call_queue_list(self.project_root_path(), &plugin_request)
            .await
            .map_err(internal_err)?
            .ok_or_else(|| plugin_missing("list"))?;
        let entries: Vec<WireQueueEntry> = response.entries.into_iter().map(plugin_entry_to_wire).collect();
        let next_cursor = match (request.limit, offset) {
            (Some(limit), _) if limit > 0 && (entries.len() as u32) >= limit => {
                Some((offset.unwrap_or(0).saturating_add(entries.len())).to_string())
            }
            _ => None,
        };
        Ok(WireListResponse { entries, next_cursor })
    }

    async fn queue_enqueue(&self, request: WireEnqueueRequest) -> Result<WireQueueEntry, ControlError> {
        tracing::info!(target: FORWARD_LOG_TARGET, "forwarding queue/enqueue to queue plugin");
        let hub = self.hub().await?;
        // v0.4.12+: a subject may live in an installed `subject_backend` plugin
        // rather than the in-tree task store. Try the in-tree store first (legacy
        // projects), then route through the subject_resolver so the plugin fallback
        // engages — mirroring the workflow-run path (ops_workflow). Without this,
        // control-routed enqueue of any plugin-backed subject failed with
        // "task not found", silently breaking the queue trigger over the transports.
        let (resolved_id, workflow_ref) = match hub.tasks().get(&request.task_id).await {
            Ok(task) => (task.id.clone(), workflow_ref_for_task(&task)),
            Err(in_tree_err) => {
                // Derive the subject kind from the qualified id prefix
                // (`<kind>:<native>`) so plugin-backed custom kinds (e.g. a
                // `song:SONG-001` subject) resolve via `<kind>/get` instead of
                // being forced through `task/get`, which the owning backend
                // rejects ("is not a task subject"). Bare ids default to task.
                let subject = subject_ref_from_qualified_id(&request.task_id);
                match hub.subject_resolver().resolve_subject_context(&subject, None, None).await {
                    Ok(ctx) => (
                        ctx.subject_id,
                        orchestrator_core::load_workflow_config_or_default(self.project_root_path())
                            .config
                            .default_workflow_ref,
                    ),
                    Err(plugin_err) => {
                        return Err(ControlError::Internal(format!(
                            "queue enqueue: task '{}' not found (in-tree: {in_tree_err}; plugin: {plugin_err})",
                            request.task_id
                        )));
                    }
                }
            }
        };
        // Preserve the subject KIND in the STORED dispatch, not just at
        // resolution time. The v0.5.21 fix derived the kind from the qualified
        // id to RESOLVE the subject at enqueue, then dropped it by storing a
        // `for_task` dispatch — so the daemon re-leased the entry as a task and
        // failed with "failed to resolve subject context" (it called `task/get`
        // for a `song:` subject). `resolved_id` is the qualified id
        // (`build_context_from_plugin` sets `subject_id` to the SubjectRef's
        // full id), so plugin-backed custom kinds lease + resolve via
        // `<kind>/get`; task/requirement keep their canonical kinds.
        let mut dispatch = SubjectDispatch::for_subject_with_metadata(
            subject_ref_from_qualified_id(&resolved_id),
            workflow_ref,
            "manual-queue-enqueue-via-control",
            Utc::now(),
        );
        if let Some(priority) = request.priority {
            dispatch.priority = Some(priority_u8_to_label(priority).to_string());
        }
        let dispatch_value = serde_json::to_value(&dispatch)
            .map_err(|e| ControlError::Internal(format!("queue routing: encode SubjectDispatch failed: {e}")))?;
        let plugin_dispatch = serde_json::from_value(dispatch_value)
            .map_err(|e| ControlError::Internal(format!("queue routing: SubjectDispatch shape drift: {e}")))?;
        // Control-routed enqueues are always immediate; deferral is a
        // CLI/MCP-only surface (`queue enqueue --at`).
        let plugin_request = queue_proto::QueueEnqueueRequest {
            subject_dispatch: plugin_dispatch,
            run_at: None,
            expire_after_secs: None,
        };
        let response = plugin_clients::call_queue_enqueue(self.project_root_path(), &plugin_request)
            .await
            .map_err(internal_err)?
            .ok_or_else(|| plugin_missing("enqueue"))?;

        if !response.enqueued {
            let list_req = queue_proto::QueueListRequest {
                status: vec![
                    queue_proto::status::PENDING.to_string(),
                    queue_proto::status::ASSIGNED.to_string(),
                    queue_proto::status::HELD.to_string(),
                ],
                limit: None,
                offset: None,
            };
            if let Some(list) =
                plugin_clients::call_queue_list(self.project_root_path(), &list_req).await.map_err(internal_err)?
            {
                if let Some(existing) = list.entries.into_iter().find(|e| e.entry_id == response.entry_id) {
                    return Ok(plugin_entry_to_wire(existing));
                }
            }
        }

        let priority = priority_label_to_u8(dispatch.priority.as_deref());
        Ok(WireQueueEntry {
            id: response.entry_id,
            subject_id: SubjectId::new(response.subject_id),
            status: WireQueueEntryStatus::Ready,
            priority,
            enqueued_at: Utc::now(),
            hold_reason: None,
        })
    }

    async fn queue_drop(&self, request: WireDropRequest) -> Result<Unit, ControlError> {
        tracing::info!(target: FORWARD_LOG_TARGET, "forwarding queue/drop to queue plugin");
        let entry_id = request.id.clone();
        let plugin_request = queue_proto::QueueDropRequest { entry_id: request.id };
        let resp = plugin_clients::call_queue_drop(self.project_root_path(), &plugin_request)
            .await
            .map_err(internal_err)?
            .ok_or_else(|| plugin_missing("drop"))?;
        mutation_to_result("drop", &entry_id, resp)
    }

    async fn queue_hold(&self, request: WireHoldRequest) -> Result<Unit, ControlError> {
        tracing::info!(target: FORWARD_LOG_TARGET, "forwarding queue/hold to queue plugin");
        let entry_id = request.id.clone();
        let plugin_request = queue_proto::QueueHoldRequest { entry_id: request.id, reason: request.reason };
        let resp = plugin_clients::call_queue_hold(self.project_root_path(), &plugin_request)
            .await
            .map_err(internal_err)?
            .ok_or_else(|| plugin_missing("hold"))?;
        mutation_to_result("hold", &entry_id, resp)
    }

    async fn queue_release(&self, request: WireReleaseRequest) -> Result<Unit, ControlError> {
        tracing::info!(target: FORWARD_LOG_TARGET, "forwarding queue/release to queue plugin");
        let entry_id = request.id.clone();
        let plugin_request = queue_proto::QueueReleaseRequest { entry_id: request.id };
        let resp = plugin_clients::call_queue_release(self.project_root_path(), &plugin_request)
            .await
            .map_err(internal_err)?
            .ok_or_else(|| plugin_missing("release"))?;
        mutation_to_result("release", &entry_id, resp)
    }

    async fn queue_reorder(&self, request: WireReorderRequest) -> Result<Unit, ControlError> {
        tracing::info!(target: FORWARD_LOG_TARGET, "forwarding queue/reorder to queue plugin");
        let moved_ids: Vec<String> = match (request.id.clone(), request.subject_ids.is_empty()) {
            (Some(_), false) => {
                return Err(ControlError::InvalidRequest(
                    "queue/reorder requires exactly one of `id` or `subject_ids`".to_string(),
                ));
            }
            (None, true) => {
                return Err(ControlError::InvalidRequest("queue/reorder requires `id` or `subject_ids`".to_string()));
            }
            (Some(id), true) => vec![id],
            (None, false) => request.subject_ids.clone(),
        };

        if matches!(request.position, WireReorderPosition::Before | WireReorderPosition::After)
            && request.anchor_id.is_none()
        {
            return Err(ControlError::InvalidRequest(
                "queue/reorder position=before|after requires `anchor_id`".to_string(),
            ));
        }

        let list_req = queue_proto::QueueListRequest {
            status: vec![queue_proto::status::PENDING.to_string(), queue_proto::status::HELD.to_string()],
            limit: None,
            offset: None,
        };
        let list = plugin_clients::call_queue_list(self.project_root_path(), &list_req)
            .await
            .map_err(internal_err)?
            .ok_or_else(|| plugin_missing("reorder"))?;
        let current_ids: Vec<String> = list.entries.into_iter().map(|e| e.entry_id).collect();
        if let Some(missing) = moved_ids.iter().find(|id| !current_ids.iter().any(|c| c == *id)) {
            return Err(ControlError::NotFound(format!("queue/reorder entry `{missing}` not in queue")));
        }
        let remaining: Vec<String> = current_ids.iter().filter(|id| !moved_ids.contains(id)).cloned().collect();

        let resolved_position: Option<bool> = match (request.position, request.anchor_id.as_deref()) {
            (WireReorderPosition::Front, None) => None,
            (WireReorderPosition::Back, None) => Some(true),
            (WireReorderPosition::Before, _) | (WireReorderPosition::Front, Some(_)) => Some(false),
            (WireReorderPosition::After, _) | (WireReorderPosition::Back, Some(_)) => Some(true),
        };

        let mut target: Vec<String> = Vec::with_capacity(current_ids.len());
        match (resolved_position, request.anchor_id.as_deref()) {
            (None, _) => {
                target.extend(moved_ids.iter().cloned());
                target.extend(remaining);
            }
            (Some(true), None) => {
                target.extend(remaining);
                target.extend(moved_ids.iter().cloned());
            }
            (Some(after_anchor), Some(anchor)) => {
                if moved_ids.iter().any(|id| id == anchor) {
                    return Err(ControlError::InvalidRequest(
                        "queue/reorder anchor_id cannot be one of the moved entries".to_string(),
                    ));
                }
                if !remaining.iter().any(|id| id == anchor) {
                    return Err(ControlError::NotFound(format!("queue/reorder anchor `{anchor}` not in queue")));
                }
                for id in &remaining {
                    if id == anchor {
                        if after_anchor {
                            target.push(id.clone());
                            target.extend(moved_ids.iter().cloned());
                        } else {
                            target.extend(moved_ids.iter().cloned());
                            target.push(id.clone());
                        }
                    } else {
                        target.push(id.clone());
                    }
                }
            }
            (Some(false), None) => unreachable!("resolved_position=Some(false) implies anchor_id Some"),
        }

        let plugin_request = queue_proto::QueueReorderRequest { entry_ids: target };
        plugin_clients::call_queue_reorder(self.project_root_path(), &plugin_request)
            .await
            .map_err(internal_err)?
            .ok_or_else(|| plugin_missing("reorder"))?;
        Ok(Unit::default())
    }

    async fn queue_stats(&self) -> Result<WireQueueStats, ControlError> {
        tracing::info!(target: FORWARD_LOG_TARGET, "forwarding queue/stats to queue plugin");
        let stats = plugin_clients::call_queue_stats(self.project_root_path())
            .await
            .map_err(internal_err)?
            .ok_or_else(|| plugin_missing("stats"))?;
        Ok(plugin_stats_to_wire(stats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_queue_protocol::QueueStats as PluginStats;
    use protocol::SubjectDispatchExt;

    #[test]
    fn plugin_stats_map_into_wire_buckets() {
        let stats = PluginStats { total: 9, pending: 5, assigned: 3, held: 1, deferred: 0 };
        let wire = plugin_stats_to_wire(stats);
        assert_eq!(wire.ready, 5);
        assert_eq!(wire.in_flight, 3);
        assert_eq!(wire.held, 1);
        assert_eq!(wire.done_recent, 0);
        assert_eq!(wire.dropped_recent, 0);
    }

    #[test]
    fn deferred_entries_are_excluded_from_ready_stat() {
        // 5 pending, 2 of which are deferred → only 3 are dispatchable now.
        let stats = PluginStats { total: 9, pending: 5, assigned: 3, held: 1, deferred: 2 };
        let wire = plugin_stats_to_wire(stats);
        assert_eq!(wire.ready, 3, "ready must exclude deferred entries");
        assert_eq!(wire.in_flight, 3);
        assert_eq!(wire.held, 1);
    }

    #[test]
    fn wire_status_round_trips_through_plugin_strings() {
        assert_eq!(plugin_status_to_wire(queue_proto::status::PENDING), WireQueueEntryStatus::Ready);
        assert_eq!(plugin_status_to_wire(queue_proto::status::ASSIGNED), WireQueueEntryStatus::InFlight);
        assert_eq!(plugin_status_to_wire(queue_proto::status::HELD), WireQueueEntryStatus::Held);
        assert_eq!(plugin_status_to_wire("unknown"), WireQueueEntryStatus::Ready);
        assert_eq!(wire_status_to_plugin(WireQueueEntryStatus::Ready), Some(queue_proto::status::PENDING));
        assert_eq!(wire_status_to_plugin(WireQueueEntryStatus::InFlight), Some(queue_proto::status::ASSIGNED));
        assert_eq!(wire_status_to_plugin(WireQueueEntryStatus::Held), Some(queue_proto::status::HELD));
        assert!(wire_status_to_plugin(WireQueueEntryStatus::Done).is_none());
    }

    #[test]
    fn subject_ref_kind_derives_from_qualified_id() {
        // Custom plugin kind must resolve via its own `<kind>/get`.
        let song = subject_ref_from_qualified_id("song:SONG-001");
        assert_eq!(song.kind(), "song");
        assert_eq!(song.id(), "song:SONG-001");
        // Canonical task / requirement keep their namespaced kinds.
        assert_eq!(subject_ref_from_qualified_id("task:TASK-1").kind(), protocol::orchestrator::SUBJECT_KIND_TASK);
        assert_eq!(
            subject_ref_from_qualified_id("requirement:REQ-1").kind(),
            protocol::orchestrator::SUBJECT_KIND_REQUIREMENT
        );
        // Bare id (no prefix) defaults to task for backward compatibility.
        assert_eq!(subject_ref_from_qualified_id("TASK-9").kind(), protocol::orchestrator::SUBJECT_KIND_TASK);
    }

    #[test]
    fn enqueue_dispatch_preserves_custom_subject_kind() {
        // Regression: the enqueue must STORE the dispatch with the subject's
        // real kind, not coerce it to `task`. A `song:` subject leased from the
        // queue must re-resolve via `song/get`; before the fix the stored
        // dispatch was always `for_task`, so the daemon re-resolved it via
        // `task/get` and failed with "failed to resolve subject context".
        let song = SubjectDispatch::for_subject_with_metadata(
            subject_ref_from_qualified_id("song:SONG-001"),
            "generate-song".to_string(),
            "manual-queue-enqueue-via-control",
            Utc::now(),
        );
        assert_eq!(song.to_workflow_run_input().subject().unwrap().kind(), "song");

        // Canonical task subjects still round-trip as tasks.
        let task = SubjectDispatch::for_subject_with_metadata(
            subject_ref_from_qualified_id("task:TASK-1"),
            "wf".to_string(),
            "src",
            Utc::now(),
        );
        assert_eq!(task.to_workflow_run_input().subject().unwrap().kind(), protocol::orchestrator::SUBJECT_KIND_TASK);
    }
}
