use super::*;
use orchestrator_core::{
    project_schedule_execution_fact, project_task_execution_fact, project_task_status,
    services::ServiceHub, TaskStatus, WorkflowMachineState, WorkflowStatus,
};
use orchestrator_daemon_runtime::{
    build_completion_reconciliation_plan, remove_terminal_dispatch_queue_entry_non_fatal,
    CompletedProcess,
};
use protocol::SubjectExecutionFact;
use std::collections::HashSet;

pub async fn project_terminal_workflow_status(
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
        WorkflowStatus::Completed
            | WorkflowStatus::Failed
            | WorkflowStatus::Escalated
            | WorkflowStatus::Cancelled
    ) {
        return;
    }

    remove_terminal_dispatch_queue_entry_non_fatal(
        project_root,
        subject_id,
        workflow_ref,
        workflow_id,
    );

    let Some(task_id) = task_id.filter(|task_id| !task_id.trim().is_empty()) else {
        return;
    };

    match workflow_status {
        WorkflowStatus::Completed => {
            project_task_execution_fact(
                hub,
                project_root,
                &SubjectExecutionFact {
                    subject_id: subject_id.to_string(),
                    task_id: Some(task_id.to_string()),
                    workflow_ref: workflow_ref.map(ToOwned::to_owned),
                    schedule_id: None,
                    exit_code: Some(0),
                    success: true,
                    failure_reason: None,
                    runner_events: Vec::new(),
                },
            )
            .await;
        }
        WorkflowStatus::Failed | WorkflowStatus::Escalated => {
            let failure_reason = failure_reason.map(ToOwned::to_owned).unwrap_or_else(|| {
                format!(
                    "workflow ended with status {}",
                    format!("{workflow_status:?}").to_ascii_lowercase()
                )
            });

            project_task_execution_fact(
                hub,
                project_root,
                &SubjectExecutionFact {
                    subject_id: subject_id.to_string(),
                    task_id: Some(task_id.to_string()),
                    workflow_ref: workflow_ref.map(ToOwned::to_owned),
                    schedule_id: None,
                    exit_code: None,
                    success: false,
                    failure_reason: Some(failure_reason),
                    runner_events: Vec::new(),
                },
            )
            .await;
        }
        WorkflowStatus::Cancelled => {
            let _ = project_task_status(hub, task_id, TaskStatus::Cancelled).await;
        }
        WorkflowStatus::Paused | WorkflowStatus::Running | WorkflowStatus::Pending => {}
    }
}

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

pub async fn recover_orphaned_running_workflows(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_subject_ids: &HashSet<String>,
) -> usize {
    let workflows = match hub.workflows().list().await {
        Ok(workflows) => workflows,
        Err(_) => return 0,
    };

    let mut recovered = 0usize;
    for workflow in workflows {
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        if workflow.machine_state == WorkflowMachineState::MergeConflict {
            continue;
        }
        if active_subject_ids.contains(&workflow.id)
            || active_subject_ids.contains(workflow.subject.id())
            || (!workflow.task_id.is_empty() && active_subject_ids.contains(&workflow.task_id))
        {
            continue;
        }

        eprintln!(
            "{}: recovering orphaned running workflow {} subject={} task={}",
            protocol::ACTOR_DAEMON,
            workflow.id,
            workflow.subject.id(),
            workflow.task_id
        );
        if let Ok(_updated) = hub.workflows().cancel(&workflow.id).await {
            project_terminal_workflow_status(
                hub.clone(),
                project_root,
                workflow.subject.id(),
                Some(workflow.task_id.as_str()),
                workflow.workflow_ref.as_deref(),
                Some(workflow.id.as_str()),
                WorkflowStatus::Cancelled,
                workflow.failure_reason.as_deref(),
            )
            .await;
        }
        recovered = recovered.saturating_add(1);
    }

    recovered
}
