use super::*;
use orchestrator_daemon_runtime::{
    execute_dispatch_plan_via_runner, CodingLease, CodingRunResources, CodingScheduler, DispatchNoticeSink,
    DispatchSelectionSource, PlannedDispatchStart, ReservationOutcome, TaskGeneration,
};
pub use orchestrator_daemon_runtime::{DispatchNotice, DispatchWorkflowStartSummary};
use tracing::warn;

use crate::services::plugin_clients;
use animus_queue_protocol::{self as queue_proto, QueueCompletionRequest, QueueLeaseRequest};
use animus_subject_protocol_wire::SubjectId as QueueSubjectId;

pub async fn dispatch_queued_entries_via_runner(
    root: &str,
    process_manager: &mut ProcessManager,
    coding_scheduler: &CodingScheduler,
    limit: usize,
) -> anyhow::Result<DispatchWorkflowStartSummary> {
    let active_subject_ids = process_manager.active_subject_ids();

    let mut planned_starts: Vec<PlannedDispatchStart> = Vec::new();
    let mut plugin_owned_subject_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut leased_entry_ids: Vec<String> = Vec::new();
    let mut coding_leases: Vec<CodingLease> = Vec::new();
    let mut undecodable_entry_ids: Vec<String> = Vec::new();
    let project_root_path = std::path::Path::new(root);

    let exclude_subjects: Vec<QueueSubjectId> = active_subject_ids.iter().cloned().map(QueueSubjectId::new).collect();
    // TASK-1072: mint the durable ids BEFORE the atomic lease. The queue stores
    // these exact ids and the runner creates/loads the journal rows with them,
    // eliminating the previous queue-UUID / runner-UUID split.
    let workflow_ids: Vec<String> = (0..limit).map(|_| uuid::Uuid::new_v4().to_string()).collect();
    let lease_req = QueueLeaseRequest {
        max: limit,
        workflow_ids: Some(workflow_ids),
        exclude_subjects: if exclude_subjects.is_empty() { None } else { Some(exclude_subjects) },
    };
    match plugin_clients::call_queue_lease(project_root_path, &lease_req).await {
        Ok(Some(response)) => {
            for entry in response.leased {
                let dispatch_value = match serde_json::to_value(&entry.subject_dispatch) {
                    Ok(v) => v,
                    Err(error) => {
                        warn!(actor = protocol::ACTOR_DAEMON, error = %error, "queue/lease returned undecodable subject_dispatch; closing entry as failed");
                        undecodable_entry_ids.push(entry.entry_id.clone());
                        continue;
                    }
                };
                let dispatch: protocol::SubjectDispatch = match serde_json::from_value(dispatch_value) {
                    Ok(d) => d,
                    Err(error) => {
                        warn!(actor = protocol::ACTOR_DAEMON, error = %error, "queue/lease subject_dispatch shape drift vs protocol::SubjectDispatch; closing entry as failed");
                        undecodable_entry_ids.push(entry.entry_id.clone());
                        continue;
                    }
                };
                let Some(workflow_id) = entry.workflow_id.clone() else {
                    warn!(
                        actor = protocol::ACTOR_DAEMON,
                        entry_id = %entry.entry_id,
                        "queue/lease returned an assigned entry without workflow_id; closing it as failed"
                    );
                    undecodable_entry_ids.push(entry.entry_id.clone());
                    continue;
                };
                let task_id = dispatch
                    .task_id()
                    .or_else(|| dispatch.subject_id())
                    .unwrap_or(&entry.entry_id)
                    .to_string();
                // The queue row is the authoritative task-generation identity.
                // Optional repo resources are recorded only when the producer
                // supplied them; workspace/environment/branch allocation happens
                // later and must be attached with the scheduler's fenced bind.
                let generation =
                    dispatch.vars.get("task_generation").cloned().unwrap_or_else(|| entry.entry_id.clone());
                let branch = dispatch.vars.get("branch").cloned().or_else(|| {
                    dispatch
                        .task_id()
                        .map(|task_id| format!("animus/{}", protocol::sanitize_identifier(task_id, "task")))
                });
                let git_ref = dispatch.vars.get("git_ref").cloned().or_else(|| branch.clone());
                let pull_request = ["pull_request", "pull_request_url", "pr_url", "pr_number"]
                    .iter()
                    .find_map(|key| dispatch.vars.get(*key).cloned())
                    .filter(|value| !value.trim().is_empty());
                let resources = CodingRunResources {
                    repository: std::fs::canonicalize(project_root_path)
                        .unwrap_or_else(|_| project_root_path.to_path_buf())
                        .to_string_lossy()
                        .into_owned(),
                    git_ref,
                    queue_item_id: entry.entry_id.clone(),
                    workflow_id: workflow_id.clone(),
                    workspace_id: None,
                    environment_id: None,
                    branch,
                    pull_request,
                };
                let lease = match coding_scheduler.reserve(TaskGeneration { task_id, generation }, resources)? {
                    ReservationOutcome::Reserved { lease } => lease,
                    ReservationOutcome::Rejected { reason } => {
                        warn!(
                            actor = protocol::ACTOR_DAEMON,
                            entry_id = %entry.entry_id,
                            collision = ?reason,
                            "coding scheduler rejected queue admission; releasing entry to pending"
                        );
                        release_leased_entry_to_pending(project_root_path, &entry.entry_id, "coding-scheduler-collision")
                            .await;
                        continue;
                    }
                };
                // Within-batch dedupe: the queue's `exclude_subjects` filter
                // honors the snapshot we sent at lease time, but multiple
                // pending entries for the same subject (different
                // workflow_refs) can still be returned in one batch. Release
                // the duplicate back to Pending so it runs on a later tick
                // after the first entry's workflow finishes. A subjectless
                // dispatch has no subject_key and is never deduped — each
                // subjectless run is its own entry.
                if let Some(subject_key) = dispatch.subject_key() {
                    if plugin_owned_subject_keys.contains(&subject_key) {
                        warn!(
                            actor = protocol::ACTOR_DAEMON,
                            subject_key = %subject_key,
                            entry_id = %entry.entry_id,
                            "queue/lease returned duplicate subject within batch; releasing extra entry back to pending"
                        );
                        release_leased_entry_to_pending(
                            project_root_path,
                            &entry.entry_id,
                            "within-batch-duplicate-subject",
                        )
                        .await;
                        let _ = coding_scheduler.release(&lease.task, lease.lease_generation, &lease.owner);
                        continue;
                    }
                    plugin_owned_subject_keys.insert(subject_key);
                }
                leased_entry_ids.push(entry.entry_id.clone());
                coding_leases.push(lease);
                planned_starts.push(PlannedDispatchStart {
                    dispatch,
                    workflow_id: Some(workflow_id),
                    selection_source: DispatchSelectionSource::DispatchQueue,
                });
            }
        }
        Ok(None) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                "queue plugin not installed; deferring dispatch (install with `animus plugin install-defaults`)"
            );
            return Ok(DispatchWorkflowStartSummary::default());
        }
        Err(error) => {
            warn!(actor = protocol::ACTOR_DAEMON, error = %error, "queue plugin queue/lease failed; deferring dispatch to next tick to avoid stranding claimed entries");
            return Ok(DispatchWorkflowStartSummary::default());
        }
    }

    for entry_id in &undecodable_entry_ids {
        let req = QueueCompletionRequest {
            entry_id: entry_id.clone(),
            status: queue_proto::completion_status::FAILED.to_string(),
            workflow_ref: None,
            workflow_id: None,
        };
        if let Err(error) = plugin_clients::call_queue_completion(project_root_path, &req).await {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                entry_id = %entry_id,
                error = %error,
                "queue plugin queue/completion (undecodable entry) failed"
            );
        }
    }

    let mut notice_sink = CliDispatchNoticeSink { outcomes: Vec::new() };
    let summary = execute_dispatch_plan_via_runner(root, process_manager, &planned_starts, limit, &mut notice_sink);

    // Reconcile each leased queue entry against its spawn outcome. Outcomes are
    // recorded in dispatch order (one per processed entry), so they align by
    // INDEX with `leased_entry_ids` / `planned_starts`. Correlating by position
    // rather than subject id is what lets subjectless dispatches (which have no
    // subject_key to key on) reconcile correctly.
    for (idx, entry_id) in leased_entry_ids.iter().enumerate() {
        match notice_sink.outcomes.get(idx) {
            // Runner spawned: the entry is now Assigned to a live workflow.
            Some(DispatchEntryOutcome::Started) => {
                let lease = &coding_leases[idx];
                let _ = coding_scheduler.renew(&lease.task, lease.lease_generation, &lease.owner);
            }
            // Recoverable defer (workflow concurrency cap) or never attempted
            // (dispatch limit reached mid-batch): back to Pending for the next
            // tick — closing them would permanently drop legitimate queued work.
            Some(DispatchEntryOutcome::Deferred) | None => {
                let lease = &coding_leases[idx];
                let _ = coding_scheduler.release(&lease.task, lease.lease_generation, &lease.owner);
                release_leased_entry_to_pending(project_root_path, entry_id, "spawn-deferred").await;
            }
            // Hard spawn failure: close the entry as FAILED.
            Some(DispatchEntryOutcome::Failed) => {
                let lease = &coding_leases[idx];
                let _ = coding_scheduler.release(&lease.task, lease.lease_generation, &lease.owner);
                let req = QueueCompletionRequest {
                    entry_id: entry_id.clone(),
                    status: queue_proto::completion_status::FAILED.to_string(),
                    workflow_ref: None,
                    workflow_id: None,
                };
                if let Err(error) = plugin_clients::call_queue_completion(project_root_path, &req).await {
                    warn!(
                        actor = protocol::ACTOR_DAEMON,
                        entry_id = %entry_id,
                        error = %error,
                        "queue plugin queue/completion (spawn-failed entry) failed"
                    );
                }
            }
        }
    }
    Ok(summary)
}

/// Release a leased queue entry back to Pending so a later tick can retry it.
///
/// Older queue plugins (pre-v0.2.0 of animus-queue-default) don't implement
/// queue/release_pending — they'd return method-not-found and we'd strand the
/// entry as Assigned forever. Fall back to completion(CANCELLED) so old
/// plugins at least move the entry to a terminal state. This trades silently
/// dropping legitimate queued work (the codex-v1 P1 we just fixed) for not
/// stranding the queue on old plugins; preflight already requires queue
/// v0.2.0+ for v0.5 so production should rarely hit this path.
async fn release_leased_entry_to_pending(project_root_path: &std::path::Path, entry_id: &str, reason: &str) {
    if let Err(error) = plugin_clients::call_queue_release_pending(project_root_path, entry_id, reason).await {
        warn!(
            actor = protocol::ACTOR_DAEMON,
            entry_id = %entry_id,
            error = %error,
            "queue plugin queue/release_pending failed; falling back to completion(cancelled)"
        );
        let req = QueueCompletionRequest {
            entry_id: entry_id.to_string(),
            status: queue_proto::completion_status::CANCELLED.to_string(),
            workflow_ref: None,
            workflow_id: None,
        };
        if let Err(completion_error) = plugin_clients::call_queue_completion(project_root_path, &req).await {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                entry_id = %entry_id,
                error = %completion_error,
                "queue plugin queue/completion fallback ({reason}) also failed; entry may be stranded as Assigned"
            );
        }
    }
}

/// Spawn outcome for a single dispatched queue entry, recorded in dispatch
/// order so the caller can reconcile leased entries by position (subjectless
/// dispatches have no subject_key to correlate on).
enum DispatchEntryOutcome {
    Started,
    Deferred,
    Failed,
}

struct CliDispatchNoticeSink {
    outcomes: Vec<DispatchEntryOutcome>,
}

impl DispatchNoticeSink for CliDispatchNoticeSink {
    fn notice(&mut self, notice: DispatchNotice) {
        match notice {
            DispatchNotice::Started { .. } => self.outcomes.push(DispatchEntryOutcome::Started),
            DispatchNotice::Failed { dispatch, error } => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    subject_id = %dispatch.subject_id().unwrap_or_default(),
                    error = %error,
                    "failed to start workflow runner"
                );
                self.outcomes.push(DispatchEntryOutcome::Failed);
            }
            DispatchNotice::Deferred { dispatch, reason } => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    subject_id = %dispatch.subject_id().unwrap_or_default(),
                    reason = %reason,
                    "workflow runner spawn deferred; entry returns to pending for next tick"
                );
                self.outcomes.push(DispatchEntryOutcome::Deferred);
            }
            _ => {}
        }
    }
}
