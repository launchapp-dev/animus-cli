use std::sync::Arc;

use animus_queue_protocol::{self as queue_proto, QueueCompletionRequest, QueueListRequest};
use orchestrator_core::{project_task_terminal_workflow_status, services::ServiceHub, WorkflowStatus};
use orchestrator_daemon_runtime::remove_terminal_dispatch_queue_entry_non_fatal;

use crate::services::plugin_clients;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn project_terminal_workflow_result(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    subject_id: &str,
    task_id: Option<&str>,
    workflow_ref: Option<&str>,
    workflow_id: Option<&str>,
    workflow_status: WorkflowStatus,
    failure_reason: Option<&str>,
) {
    if !matches!(
        workflow_status,
        WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Escalated | WorkflowStatus::Cancelled
    ) {
        return;
    }

    remove_terminal_dispatch_queue_entry_non_fatal(project_root, subject_id, workflow_ref, workflow_id);

    // Codex R7 [P1]: when a v0.5 queue plugin owns the queue, also call
    // `queue/completion` so the entry leaves `assigned`. We don't have
    // the `entry_id` here (the projection happens at terminal-result
    // time, far from the original dispatch), so look it up by
    // subject_id via `queue/list { status: ["assigned"] }`. If multiple
    // entries match (shouldn't happen in v0.5 single-tenant), complete
    // them all — terminal state for a subject means terminal for every
    // assigned entry referencing it.
    let plugin_status = match workflow_status {
        WorkflowStatus::Completed => animus_queue_protocol::completion_status::COMPLETED,
        WorkflowStatus::Failed | WorkflowStatus::Escalated => animus_queue_protocol::completion_status::FAILED,
        WorkflowStatus::Cancelled => animus_queue_protocol::completion_status::CANCELLED,
        _ => animus_queue_protocol::completion_status::COMPLETED,
    };
    let project_root_path = std::path::Path::new(project_root);
    let list_req =
        QueueListRequest { status: vec![queue_proto::status::ASSIGNED.to_string()], limit: None, offset: None };
    match plugin_clients::call_queue_list(project_root_path, &list_req).await {
        Ok(Some(list_response)) => {
            for entry in list_response.entries {
                if entry.subject_id != subject_id {
                    continue;
                }
                // Wave 3 follow-up (issue #240): atomic queue/lease has
                // the plugin synthesize workflow_ids at claim time, so
                // strict workflow_id matching would skip every
                // queue-plugin entry. Match instead on subject_id +
                // subject_dispatch.workflow_ref so we don't terminate
                // sibling entries for the same subject queued under a
                // different workflow_ref (e.g. the same task queued
                // for `standard` and `ops`).
                if let Some(wanted_ref) = workflow_ref {
                    if entry.subject_dispatch.workflow_ref != wanted_ref {
                        continue;
                    }
                }
                let req = QueueCompletionRequest {
                    entry_id: entry.entry_id,
                    status: plugin_status.to_string(),
                    workflow_ref: workflow_ref.map(str::to_string),
                    workflow_id: workflow_id.map(str::to_string),
                };
                if let Err(error) = plugin_clients::call_queue_completion(project_root_path, &req).await {
                    tracing::warn!(
                        subject_id = subject_id,
                        error = %error,
                        "queue plugin queue/completion failed; entry may remain assigned"
                    );
                }
            }
        }
        Ok(None) => {
            // No queue plugin installed — terminal cleanup already
            // handled by the in-tree `remove_terminal_dispatch_queue_entry_non_fatal`
            // call above.
        }
        Err(error) => {
            tracing::warn!(
                subject_id = subject_id,
                error = %error,
                "queue plugin queue/list lookup for completion failed; entry may remain assigned"
            );
        }
    }

    let Some(task_id) = task_id.filter(|task_id| !task_id.trim().is_empty()) else {
        return;
    };

    project_task_terminal_workflow_status(hub, task_id, workflow_status, failure_reason.map(ToOwned::to_owned)).await;
}
