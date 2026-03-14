use super::*;
#[cfg(test)]
use crate::services::runtime::workflow_mutation_surface::daemon_workflow_assignment;
pub use orchestrator_daemon_runtime::{
    active_workflow_subject_ids, active_workflow_task_ids, execute_dispatch_plan_via_runner,
    is_terminally_completed_workflow, load_dispatch_queue_state,
    mark_dispatch_queue_entry_assigned, plan_ready_dispatch, DispatchCandidate, DispatchNotice,
    DispatchNoticeSink, DispatchQueueEntryStatus, DispatchQueueState, DispatchSelectionSource,
    DispatchWorkflowStart, DispatchWorkflowStartSummary, PlannedDispatchStart, SubjectDispatch,
};
#[cfg(test)]
pub use orchestrator_daemon_runtime::{
    dispatch_queue_state_path, save_dispatch_queue_state, DispatchQueueEntry,
};

#[cfg(test)]
pub fn daemon_agent_assignee_for_workflow_start(
    project_root: &str,
    workflow: &orchestrator_core::OrchestratorWorkflow,
    task: &orchestrator_core::OrchestratorTask,
) -> (String, Option<String>) {
    daemon_workflow_assignment(project_root, workflow, task)
}

pub fn dispatch_queued_entries_via_runner(
    root: &str,
    process_manager: &mut ProcessManager,
    limit: usize,
) -> anyhow::Result<DispatchWorkflowStartSummary> {
    let active_subject_ids = process_manager.active_subject_ids();
    let queue_state = match load_dispatch_queue_state(root) {
        Ok(state) => state,
        Err(error) => {
            eprintln!(
                "{}: failed to load dispatch queue state: {}",
                protocol::ACTOR_DAEMON,
                error
            );
            return Ok(DispatchWorkflowStartSummary::default());
        }
    };

    let Some(queue_state) = queue_state else {
        return Ok(DispatchWorkflowStartSummary::default());
    };

    let mut planned_starts = Vec::new();
    for entry in &queue_state.entries {
        if planned_starts.len() >= limit {
            break;
        }
        if entry.status != DispatchQueueEntryStatus::Pending {
            continue;
        }
        let Some(dispatch) = &entry.dispatch else {
            continue;
        };
        if active_subject_ids.contains(dispatch.subject_id()) {
            continue;
        }

        planned_starts.push(PlannedDispatchStart {
            dispatch: dispatch.clone(),
            selection_source: DispatchSelectionSource::DispatchQueue,
        });
    }

    let mut notice_sink = CliDispatchNoticeSink;
    Ok(execute_dispatch_plan_via_runner(
        root,
        process_manager,
        &planned_starts,
        limit,
        &mut notice_sink,
    ))
}

struct CliDispatchNoticeSink;

impl DispatchNoticeSink for CliDispatchNoticeSink {
    fn notice(&mut self, notice: DispatchNotice) {
        match notice {
            DispatchNotice::QueueAssignmentFailed { dispatch, error } => {
                eprintln!(
                    "{}: failed to mark dispatch queue entry assigned for subject {}: {}",
                    protocol::ACTOR_DAEMON,
                    dispatch.subject_id(),
                    error
                );
            }
            DispatchNotice::Failed { dispatch, error } => {
                eprintln!(
                    "{}: failed to start workflow runner for subject {}: {}",
                    protocol::ACTOR_DAEMON,
                    dispatch.subject_id(),
                    error
                );
            }
            _ => {}
        }
    }
}
