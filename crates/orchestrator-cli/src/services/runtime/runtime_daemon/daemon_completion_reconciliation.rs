use std::sync::Arc;

use orchestrator_core::{services::ServiceHub, TaskStatus};

use super::{
    completion_reason, remove_terminal_em_work_queue_entry_non_fatal,
    schedule_dispatch::ScheduleDispatch, set_task_blocked_with_reason,
};
use crate::services::runtime::runtime_daemon::daemon_process_manager::CompletedProcess;

pub(super) struct CompletionReconciler;

impl CompletionReconciler {
    pub(super) async fn reconcile(
        hub: Arc<dyn ServiceHub>,
        root: &str,
        completed_processes: Vec<CompletedProcess>,
    ) -> (usize, usize) {
        let mut executed_workflow_phases = 0usize;
        let mut failed_workflow_phases = 0usize;

        for completed in completed_processes {
            for event in &completed.events {
                eprintln!(
                    "{}: runner event: {} subject={} pipeline={:?} exit={:?}",
                    protocol::ACTOR_DAEMON,
                    event.event,
                    completed.subject_id,
                    event.pipeline,
                    event.exit_code,
                );
            }

            if let Some(ref task_id) = completed.task_id {
                if completed.success {
                    remove_terminal_em_work_queue_entry_non_fatal(root, task_id, None);
                    let _ = hub
                        .tasks()
                        .set_status(task_id, TaskStatus::Done, false)
                        .await;
                } else {
                    let reason = completion_reason(&completed);
                    if let Ok(task) = hub.tasks().get(task_id).await {
                        let _ = set_task_blocked_with_reason(
                            hub.clone(),
                            &task,
                            format!("workflow runner failed: {reason}"),
                            None,
                        )
                        .await;
                    } else {
                        let _ = hub
                            .tasks()
                            .set_status(task_id, TaskStatus::Blocked, false)
                            .await;
                    }
                }
            } else {
                eprintln!(
                    "{}: workflow runner {} for subject '{}' (exit={:?})",
                    protocol::ACTOR_DAEMON,
                    if completed.success {
                        "succeeded"
                    } else {
                        "failed"
                    },
                    completed.subject_id,
                    completed.exit_code,
                );
            }

            if let Some(ref sched_id) = completed.schedule_id {
                let status = if completed.success {
                    "completed"
                } else {
                    "failed"
                };
                ScheduleDispatch::update_completion_state(root, sched_id, status);
            }

            if completed.success {
                executed_workflow_phases = executed_workflow_phases.saturating_add(1);
            } else {
                failed_workflow_phases = failed_workflow_phases.saturating_add(1);
            }
        }

        (executed_workflow_phases, failed_workflow_phases)
    }
}
