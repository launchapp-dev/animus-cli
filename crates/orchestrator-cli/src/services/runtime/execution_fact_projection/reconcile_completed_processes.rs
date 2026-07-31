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

    // BU-4 / TASK-1174: when durable journal resume is active, terminalize the
    // persisted workflow row for any run that exited before the runner could
    // write its terminal state (e.g. environment preparation, plugin startup,
    // argument parsing, or a resume failure). Preserve the runner's terminal
    // vocabulary: an execution failure is `Failed` with the exact bounded
    // reason, while only an explicit cancellation is `Cancelled`. The old
    // blanket `cancel()` projection hid infrastructure/provider failures from
    // the journal and Portal, and made a phase that never ran look like an
    // operator cancellation.
    //
    // Gated on `journal_resume_enabled` so the no-journal path is byte-identical
    // (its stuck runs are still handled by the orphan-sweep path).
    if crate::services::runtime::runtime_daemon::daemon_reconciliation::journal_resume_enabled(root) {
        for fact in &plan.execution_facts {
            let Some(workflow_id) = fact.workflow_id.as_deref() else {
                continue;
            };
            if !matches!(fact.completion_status(), "failed" | "cancelled") {
                continue;
            }
            match project_runner_terminal_state(
                hub.clone(),
                workflow_id,
                fact.completion_status(),
                fact.failure_reason.as_deref(),
                fact.exit_code,
            )
            .await
            {
                Err(error) => warn!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow_id,
                    error = %error,
                    "failed to project runner terminal state; it may be reconciled next tick"
                ),
                Ok(Some(updated)) if orchestrator_core::is_terminal_workflow_run_status(updated.status) => info!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow_id,
                    status = %fact.completion_status(),
                    "projected runner terminal state after exit"
                ),
                Ok(Some(updated)) => warn!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow_id,
                    status = ?updated.status,
                    expected = %fact.completion_status(),
                    "runner terminal projection remained non-terminal; preserving row for investigation"
                ),
                Ok(None) => {}
            }
        }
    }

    CompletedProcessReconciliation {
        executed_workflow_phases: plan.executed_workflow_phases,
        failed_workflow_phases: plan.failed_workflow_phases,
        workflow_failures: plan.workflow_failures,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalProjection {
    Failed,
    Cancelled,
}

fn terminal_projection(completion_status: &str) -> Option<TerminalProjection> {
    match completion_status {
        "failed" => Some(TerminalProjection::Failed),
        "cancelled" => Some(TerminalProjection::Cancelled),
        _ => None,
    }
}

async fn project_runner_terminal_state(
    hub: Arc<dyn ServiceHub>,
    workflow_id: &str,
    completion_status: &str,
    failure_reason: Option<&str>,
    exit_code: Option<i32>,
) -> anyhow::Result<Option<protocol::orchestrator::OrchestratorWorkflow>> {
    let workflow = hub.workflows().get(workflow_id).await?;
    if orchestrator_core::is_terminal_workflow_run_status(workflow.status) {
        return Ok(None);
    }

    let updated = match terminal_projection(completion_status) {
        Some(TerminalProjection::Failed) => {
            let reason = failure_reason
                .map(str::to_string)
                .unwrap_or_else(|| format!("workflow runner exited with status {exit_code:?}"));
            hub.workflows().fail_external_execution(workflow_id, reason).await?
        }
        Some(TerminalProjection::Cancelled) => hub.workflows().cancel(workflow_id).await?,
        None => return Ok(None),
    };
    Ok(Some(updated))
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

#[cfg(test)]
mod tests {
    use super::*;

    use orchestrator_core::{
        services::FileServiceHub, workflow::WorkflowStateManager, WorkflowRunInput, WorkflowStatus,
    };

    #[test]
    fn failed_runner_exit_is_not_projected_as_operator_cancellation() {
        assert_eq!(terminal_projection("failed"), Some(TerminalProjection::Failed));
        assert_eq!(terminal_projection("cancelled"), Some(TerminalProjection::Cancelled));
        assert_eq!(terminal_projection("completed"), None);
        assert_eq!(terminal_projection("running"), None);
    }

    #[tokio::test]
    async fn runner_failure_projection_preserves_reason_and_cancel_remains_distinct() {
        let _env_lock = crate::shared::test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // This test exercises terminal projection semantics, not journal-plugin
        // discovery. Pin both WorkflowStateManager instances to SQLite while
        // holding the process-wide environment guard. Without this boundary,
        // parallel runtime tests can temporarily redirect HOME/plugin state
        // between FileServiceHub construction and the fixture's direct manager
        // load, producing a nondeterministic "workflow not found" failure.
        let _journal_backend =
            protocol::test_utils::EnvVarGuard::set("ANIMUS_DISABLE_WORKFLOW_JOURNAL_PLUGIN", Some("1"));
        let root = tempfile::tempdir().expect("project root");
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(root.path()).expect("file service hub"));
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(root.path());

        let failed = hub
            .workflows()
            .run(WorkflowRunInput::for_task("TASK-failed".to_string(), Some("standard-workflow".to_string())), None)
            .await
            .expect("failed workflow fixture");
        let manager = WorkflowStateManager::new(root.path());
        let mut before = manager.load(&failed.id).expect("load workflow fixture");
        before.phases[before.current_phase_index].attempt = 2;
        before.rework_counts.insert("implementation".to_string(), 2);
        before.total_reworks = 2;
        manager.save(&before).expect("persist retry/rework fixture");
        let attempts_before: Vec<_> = before.phases.iter().map(|phase| phase.attempt).collect();
        let rework_counts_before = before.rework_counts.clone();
        let total_reworks_before = before.total_reworks;
        let reason = "environment/prepare failed: Railway service cap reached";
        let projected = project_runner_terminal_state(hub.clone(), &failed.id, "failed", Some(reason), Some(1))
            .await
            .expect("failure projection")
            .expect("non-terminal workflow should be projected");
        assert_eq!(projected.status, WorkflowStatus::Failed);
        assert_eq!(projected.failure_reason.as_deref(), Some(reason));
        assert_eq!(projected.phases.iter().map(|phase| phase.attempt).collect::<Vec<_>>(), attempts_before);
        assert_eq!(projected.rework_counts, rework_counts_before);
        assert_eq!(projected.total_reworks, total_reworks_before);

        let cancelled = hub
            .workflows()
            .run(WorkflowRunInput::for_task("TASK-cancelled".to_string(), Some("standard-workflow".to_string())), None)
            .await
            .expect("cancelled workflow fixture");
        let projected = project_runner_terminal_state(
            hub.clone(),
            &cancelled.id,
            "cancelled",
            Some("must not become a failure reason"),
            None,
        )
        .await
        .expect("cancellation projection")
        .expect("non-terminal workflow should be projected");
        assert_eq!(projected.status, WorkflowStatus::Cancelled);
        assert_eq!(projected.failure_reason, None);
    }
}
