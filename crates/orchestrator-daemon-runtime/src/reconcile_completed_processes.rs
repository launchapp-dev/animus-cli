use std::sync::Arc;

use orchestrator_core::services::ServiceHub;

use crate::project_schedule_execution_fact::project_schedule_execution_fact;
use crate::project_task_execution_fact::project_task_execution_fact;
use crate::{build_completion_reconciliation_plan, CompletedProcess};

pub async fn reconcile_completed_processes(
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
