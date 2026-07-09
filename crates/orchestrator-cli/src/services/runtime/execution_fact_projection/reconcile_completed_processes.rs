use std::sync::Arc;

use animus_queue_protocol::{self as queue_proto, QueueCompletionRequest, QueueListRequest};
use orchestrator_core::{project_execution_fact, project_schedule_execution_fact, services::ServiceHub};
use orchestrator_daemon_runtime::{
    build_completion_reconciliation_plan, CompletedProcess, CompletedProcessReconciliation,
};
use tracing::{debug, info, warn};

use crate::services::plugin_clients;

pub(crate) async fn reconcile_completed_processes(
    hub: Arc<dyn ServiceHub>,
    root: &str,
    completed_processes: Vec<CompletedProcess>,
) -> CompletedProcessReconciliation {
    let plan = build_completion_reconciliation_plan(completed_processes);

    // Route task-status projections through the installed subject backend when
    // one owns `task` (portal); the in-tree `hub.tasks()` store is empty there,
    // so projecting a terminal status into it left the plugin-backed task stuck
    // InProgress until the 24h stale sweep.
    let store = orchestrator_daemon_runtime::resolve_task_projection_store(root, hub.clone()).await;

    for fact in &plan.execution_facts {
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

        // Drain the v0.5 queue plugin (when installed). The
        // `fact.completion_status()` already maps
        // onto the plugin's `completion_status` vocabulary
        // (`completed`/`failed`/`cancelled`).
        finalize_plugin_queue_entry(root, fact).await;

        if !project_execution_fact(store.as_ref(), fact).await {
            info!(
                actor = protocol::ACTOR_DAEMON,
                subject_id = %fact.subject_id,
                status = %fact.completion_status(),
                exit_code = ?fact.exit_code,
                "workflow runner completed"
            );
        }

        project_schedule_execution_fact(root, fact);
    }

    // BU-4: when durable journal resume is active, terminalize the persisted
    // workflow row for any run that FAILED before the runner could write a
    // terminal status itself (e.g. a resume re-dispatch that died on a bad
    // `--workflow-id`, plugin/startup failure, or arg parse error). The
    // task/queue projectors above do NOT update the workflow row, so without
    // this the run stays `Running` and `resumable_orphans_for_redispatch`
    // re-dispatches it every tick (livelock). Cancel is idempotent here — a run
    // the runner already terminalized is skipped by the non-terminal guard.
    // Gated on `journal_resume_enabled` so the no-journal path is byte-identical
    // (its stuck runs are still handled by the orphan-sweep cancel path).
    if crate::services::runtime::runtime_daemon::daemon_reconciliation::journal_resume_enabled(root) {
        for fact in &plan.execution_facts {
            let Some(workflow_id) = fact.workflow_id.as_deref() else {
                continue;
            };
            if !matches!(fact.completion_status(), "failed" | "cancelled") {
                continue;
            }
            match hub.workflows().get(workflow_id).await {
                Ok(workflow) if !orchestrator_core::is_terminal_workflow_run_status(workflow.status) => {
                    // Use `cancel` (Running -> Cancelled): it is the only
                    // service transition GUARANTEED to terminalize a Running
                    // run here. `mark_completed_failed` no-ops unless the run is
                    // already Completed, and `fail_current_phase` may apply the
                    // phase RETRY policy and leave the run Running — both would
                    // leave the livelock unbroken (codex P1). Cancelling a run
                    // that died before doing any work is correct cleanup (live
                    // runners / live agent records were already excluded from
                    // re-dispatch), not a wrongful cancel; the operator can
                    // re-enqueue. The original failure reason is preserved in
                    // the execution fact / task projection above.
                    if let Err(error) = hub.workflows().cancel(workflow_id).await {
                        warn!(
                            actor = protocol::ACTOR_DAEMON,
                            workflow_id = %workflow_id,
                            error = %error,
                            "failed to terminalize failed resume target; it may be re-dispatched next tick"
                        );
                    } else {
                        info!(
                            actor = protocol::ACTOR_DAEMON,
                            workflow_id = %workflow_id,
                            status = %fact.completion_status(),
                            "terminalized failed resume target (runner exited before persisting terminal status)"
                        );
                    }
                }
                _ => {}
            }
        }
    }

    CompletedProcessReconciliation {
        executed_workflow_phases: plan.executed_workflow_phases,
        failed_workflow_phases: plan.failed_workflow_phases,
        workflow_failures: plan.workflow_failures,
    }
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
        // Send `workflow_id: None`. The plugin synthesizes its own workflow_id
        // when it transitions an entry Pending → Assigned (see the comment
        // above), so that synthesized id never equals `fact.workflow_id` (the
        // real run id). The plugin's completion handler filters out the entry
        // when a non-matching workflow_id is supplied, which would strand every
        // queue-leased entry as Assigned forever. The entry_id (unique) plus
        // workflow_ref already identify the entry unambiguously.
        let req = QueueCompletionRequest {
            entry_id: entry.entry_id.clone(),
            status: mapped.to_string(),
            workflow_ref: fact.workflow_ref.clone(),
            workflow_id: None,
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
