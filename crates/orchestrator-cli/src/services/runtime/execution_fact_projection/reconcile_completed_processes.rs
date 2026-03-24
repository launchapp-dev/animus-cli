use std::sync::Arc;

use orchestrator_core::{project_execution_fact, project_schedule_execution_fact, services::ServiceHub};
use orchestrator_daemon_runtime::{
    build_completion_reconciliation_plan, remove_terminal_dispatch_queue_entry_non_fatal, CompletedProcess,
};
use tracing::{debug, info};

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
