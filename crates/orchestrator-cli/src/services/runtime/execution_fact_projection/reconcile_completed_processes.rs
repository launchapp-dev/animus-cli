use std::sync::Arc;

use animus_queue_protocol::{self as queue_proto, QueueCompletionRequest, QueueListRequest};
use orchestrator_core::{project_execution_fact, project_schedule_execution_fact, services::ServiceHub};
use orchestrator_daemon_runtime::{
    build_completion_reconciliation_plan, remove_terminal_dispatch_queue_entry_non_fatal, CompletedProcess,
};
use tracing::{debug, info, warn};

use crate::services::plugin_clients;

pub(crate) async fn reconcile_completed_processes(
    hub: Arc<dyn ServiceHub>,
    root: &str,
    completed_processes: Vec<CompletedProcess>,
) -> (usize, usize) {
    let plan = build_completion_reconciliation_plan(completed_processes);

    for fact in plan.execution_facts {
        for event in &fact.runner_events {
            debug!(
                actor = protocol::ACTOR_DAEMON,
                subject_id = %fact.subject_id,
                event_type = %event.event,
                workflow_ref = ?event.workflow_ref,
                exit_code = ?event.exit_code,
                "runner event"
            );
        }

        remove_terminal_dispatch_queue_entry_non_fatal(
            root,
            &fact.subject_id,
            fact.workflow_ref.as_deref(),
            fact.workflow_id.as_deref(),
        );

        // Codex R9 [P1]: also drain the v0.5 queue plugin (when
        // installed). The `fact.completion_status()` already maps
        // onto the plugin's `completion_status` vocabulary
        // (`completed`/`failed`/`cancelled`).
        finalize_plugin_queue_entry(root, &fact).await;

        if !project_execution_fact(hub.clone(), root, &fact).await {
            info!(
                actor = protocol::ACTOR_DAEMON,
                subject_id = %fact.subject_id,
                status = %fact.completion_status(),
                exit_code = ?fact.exit_code,
                "workflow runner completed"
            );
        }

        project_schedule_execution_fact(root, &fact);
    }

    (plan.executed_workflow_phases, plan.failed_workflow_phases)
}

async fn finalize_plugin_queue_entry(root: &str, fact: &protocol::SubjectExecutionFact) {
    let project_root_path = std::path::Path::new(root);
    let list_req =
        QueueListRequest { status: vec![queue_proto::status::ASSIGNED.to_string()], limit: None, offset: None };
    let list_response = match plugin_clients::call_queue_list(project_root_path, &list_req).await {
        Ok(Some(r)) => r,
        Ok(None) => return, // No queue plugin installed.
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                subject_id = %fact.subject_id,
                error = %error,
                "queue plugin queue/list for completion lookup failed"
            );
            return;
        }
    };
    let mapped = match fact.completion_status() {
        "completed" => queue_proto::completion_status::COMPLETED,
        "cancelled" => queue_proto::completion_status::CANCELLED,
        _ => queue_proto::completion_status::FAILED,
    };
    for entry in list_response.entries {
        if entry.subject_id != fact.subject_id {
            continue;
        }
        // Wave 3 follow-up (issue #240): the v0.5 queue/lease dispatch
        // path lets the plugin synthesize workflow_ids when it
        // transitions Pending → Assigned, so strict workflow_id
        // matching would skip every queue-plugin entry. Match instead
        // on subject_id + subject_dispatch.workflow_ref so we don't
        // terminate sibling entries for the same subject queued under
        // a different workflow_ref (e.g. the same task queued for
        // `standard` and `ops`).
        if let Some(wanted_ref) = fact.workflow_ref.as_deref() {
            if entry.subject_dispatch.workflow_ref != wanted_ref {
                continue;
            }
        }
        let req = QueueCompletionRequest {
            entry_id: entry.entry_id.clone(),
            status: mapped.to_string(),
            workflow_ref: fact.workflow_ref.clone(),
            workflow_id: fact.workflow_id.clone(),
        };
        if let Err(error) = plugin_clients::call_queue_completion(project_root_path, &req).await {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                subject_id = %fact.subject_id,
                entry_id = %entry.entry_id,
                error = %error,
                "queue plugin queue/completion call failed; entry may remain assigned"
            );
        }
    }
}
