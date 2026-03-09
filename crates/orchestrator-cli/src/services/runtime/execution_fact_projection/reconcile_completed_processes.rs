use std::sync::Arc;

use orchestrator_core::{
    project_schedule_execution_fact, project_task_execution_fact, services::ServiceHub,
};
use orchestrator_daemon_runtime::{
    build_completion_reconciliation_plan, remove_terminal_dispatch_queue_entry_non_fatal,
    CompletedProcess,
};

pub(crate) async fn reconcile_completed_processes(
    hub: Arc<dyn ServiceHub>,
    root: &str,
    completed_processes: Vec<CompletedProcess>,
) -> (usize, usize) {
    let plan = build_completion_reconciliation_plan(completed_processes);

    for fact in plan.execution_facts {
        for event in &fact.runner_events {
            eprintln!(
                "{}: runner event: {} subject={} workflow_ref={:?} exit={:?}",
                protocol::ACTOR_DAEMON,
                event.event,
                fact.subject_id,
                event.workflow_ref,
                event.exit_code,
            );
        }

        remove_terminal_dispatch_queue_entry_non_fatal(
            root,
            &fact.subject_id,
            fact.workflow_ref.as_deref(),
            None,
        );

        if fact.task_id.is_some() {
            project_task_execution_fact(hub.clone(), root, &fact).await;
        } else {
            eprintln!(
                "{}: workflow runner {} for subject '{}' (exit={:?})",
                protocol::ACTOR_DAEMON,
                if fact.success { "succeeded" } else { "failed" },
                fact.subject_id,
                fact.exit_code,
            );
        }

        project_schedule_execution_fact(root, &fact);
    }

    (plan.executed_workflow_phases, plan.failed_workflow_phases)
}
