use super::*;
use orchestrator_daemon_runtime::{
    execute_dispatch_plan_via_runner, load_dispatch_queue_state, DispatchNoticeSink, DispatchQueueEntryStatus,
    DispatchSelectionSource, PlannedDispatchStart,
};
pub use orchestrator_daemon_runtime::{DispatchNotice, DispatchWorkflowStartSummary};
use tracing::warn;

pub fn dispatch_queued_entries_via_runner(
    root: &str,
    process_manager: &mut ProcessManager,
    limit: usize,
) -> anyhow::Result<DispatchWorkflowStartSummary> {
    let active_subject_ids = process_manager.active_subject_ids();
    let queue_state = match load_dispatch_queue_state(root) {
        Ok(state) => state,
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                error = %error,
                "failed to load dispatch queue state"
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
        if active_subject_ids.contains(&dispatch.subject_key()) {
            continue;
        }

        planned_starts.push(PlannedDispatchStart {
            dispatch: dispatch.clone(),
            selection_source: DispatchSelectionSource::DispatchQueue,
        });
    }

    let mut notice_sink = CliDispatchNoticeSink;
    Ok(execute_dispatch_plan_via_runner(root, process_manager, &planned_starts, limit, &mut notice_sink))
}

struct CliDispatchNoticeSink;

impl DispatchNoticeSink for CliDispatchNoticeSink {
    fn notice(&mut self, notice: DispatchNotice) {
        match notice {
            DispatchNotice::QueueAssignmentFailed { dispatch, error } => {
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
