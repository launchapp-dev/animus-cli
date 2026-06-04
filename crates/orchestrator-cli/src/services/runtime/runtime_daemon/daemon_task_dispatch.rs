use super::*;
use orchestrator_daemon_runtime::{
    execute_dispatch_plan_via_runner, DispatchNoticeSink, DispatchSelectionSource, PlannedDispatchStart,
};
pub use orchestrator_daemon_runtime::{DispatchNotice, DispatchWorkflowStartSummary};
use tracing::warn;

use crate::services::plugin_clients;
use animus_queue_protocol::{self as queue_proto, QueueCompletionRequest, QueueLeaseRequest};
use animus_subject_protocol_v05::SubjectId as QueueSubjectId;

pub async fn dispatch_queued_entries_via_runner(
    root: &str,
    process_manager: &mut ProcessManager,
    limit: usize,
) -> anyhow::Result<DispatchWorkflowStartSummary> {
    let active_subject_ids = process_manager.active_subject_ids();

    let mut planned_starts: Vec<PlannedDispatchStart> = Vec::new();
    let mut plugin_owned_subject_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut leased_entry_ids: Vec<String> = Vec::new();
    let mut undecodable_entry_ids: Vec<String> = Vec::new();
    let project_root_path = std::path::Path::new(root);

    let exclude_subjects: Vec<QueueSubjectId> = active_subject_ids.iter().cloned().map(QueueSubjectId::new).collect();
    let lease_req = QueueLeaseRequest {
        max: limit,
        workflow_ids: None,
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
                let subject_key = dispatch.subject_key();
                // Within-batch dedupe: the queue's `exclude_subjects` filter
                // honors the snapshot we sent at lease time, but multiple
                // pending entries for the same subject (different
                // workflow_refs) can still be returned in one batch. Release
                // the duplicate back to Pending so it runs on a later tick
                // after the first entry's workflow finishes.
                if plugin_owned_subject_keys.contains(&subject_key) {
                    warn!(
                        actor = protocol::ACTOR_DAEMON,
                        subject_key = %subject_key,
                        entry_id = %entry.entry_id,
                        "queue/lease returned duplicate subject within batch; releasing extra entry back to pending"
                    );
                    if let Err(error) = plugin_clients::call_queue_release_pending(
                        project_root_path,
                        &entry.entry_id,
                        "within-batch-duplicate-subject",
                    )
                    .await
                    {
                        // Older queue plugins (pre-v0.2.0 of animus-queue-default) don't
                        // implement queue/release_pending — they'd return method-not-found
                        // and we'd strand the entry as Assigned forever. Fall back to
                        // completion(CANCELLED) so old plugins at least move the entry to
                        // a terminal state. This trades silently dropping legitimate
                        // queued work (the codex-v1 P1 we just fixed) for not stranding
                        // the queue on old plugins; preflight already requires queue
                        // v0.2.0+ for v0.5 so production should rarely hit this path.
                        warn!(
                            actor = protocol::ACTOR_DAEMON,
                            entry_id = %entry.entry_id,
                            error = %error,
                            "queue plugin queue/release_pending failed; falling back to completion(cancelled)"
                        );
                        let req = QueueCompletionRequest {
                            entry_id: entry.entry_id.clone(),
                            status: queue_proto::completion_status::CANCELLED.to_string(),
                            workflow_ref: None,
                            workflow_id: None,
                        };
                        if let Err(completion_error) =
                            plugin_clients::call_queue_completion(project_root_path, &req).await
                        {
                            warn!(
                                actor = protocol::ACTOR_DAEMON,
                                entry_id = %entry.entry_id,
                                error = %completion_error,
                                "queue plugin queue/completion fallback (within-batch duplicate) also failed; entry may be stranded as Assigned"
                            );
                        }
                    }
                    continue;
                }
                plugin_owned_subject_keys.insert(subject_key);
                leased_entry_ids.push(entry.entry_id.clone());
                planned_starts
                    .push(PlannedDispatchStart { dispatch, selection_source: DispatchSelectionSource::DispatchQueue });
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

    let mut notice_sink = CliDispatchNoticeSink { plugin_owned_subject_keys: plugin_owned_subject_keys.clone() };
    let summary = execute_dispatch_plan_via_runner(root, process_manager, &planned_starts, limit, &mut notice_sink);

    if !leased_entry_ids.is_empty() {
        let started_keys: std::collections::HashSet<String> =
            summary.started_workflows.iter().map(|s| s.dispatch.subject_key()).collect();
        for (idx, planned) in planned_starts.iter().enumerate() {
            let Some(entry_id) = leased_entry_ids.get(idx) else {
                continue;
            };
            let subject_key = planned.dispatch.subject_key();
            if started_keys.contains(&subject_key) {
                continue;
            }
            let req = QueueCompletionRequest {
                entry_id: entry_id.clone(),
                status: queue_proto::completion_status::FAILED.to_string(),
                workflow_ref: None,
                workflow_id: None,
            };
            if let Err(error) = plugin_clients::call_queue_completion(project_root_path, &req).await {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    subject_key = %subject_key,
                    entry_id = %entry_id,
                    error = %error,
                    "queue plugin queue/completion (spawn-failed entry) failed"
                );
            }
        }
    }
    Ok(summary)
}

struct CliDispatchNoticeSink {
    plugin_owned_subject_keys: std::collections::HashSet<String>,
}

impl DispatchNoticeSink for CliDispatchNoticeSink {
    fn notice(&mut self, notice: DispatchNotice) {
        match notice {
            DispatchNotice::QueueAssignmentFailed { dispatch, error } => {
                if self.plugin_owned_subject_keys.contains(&dispatch.subject_key()) {
                    return;
                }
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    subject_id = %dispatch.subject_key(),
                    error = %error,
                    "failed to mark dispatch queue entry assigned"
                );
            }
            DispatchNotice::Failed { dispatch, error } => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    subject_id = %dispatch.subject_key(),
                    error = %error,
                    "failed to start workflow runner"
                );
            }
            _ => {}
        }
    }
}
