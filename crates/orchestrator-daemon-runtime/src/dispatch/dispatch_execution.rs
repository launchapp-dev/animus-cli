use crate::dispatch::process_manager::WorkflowConcurrencyCapReached;
use crate::{
    mark_dispatch_queue_entry_assigned, DispatchNotice, DispatchNoticeSink, DispatchSelectionSource,
    DispatchWorkflowStart, DispatchWorkflowStartSummary, PlannedDispatchStart, ProcessManager,
};

pub fn execute_dispatch_plan_via_runner<S>(
    project_root: &str,
    process_manager: &mut ProcessManager,
    starts: &[PlannedDispatchStart],
    limit: usize,
    notice_sink: &mut S,
) -> DispatchWorkflowStartSummary
where
    S: DispatchNoticeSink,
{
    if limit == 0 {
        return DispatchWorkflowStartSummary::default();
    }

    let mut started_workflows = Vec::new();
    for planned_start in starts {
        if started_workflows.len() >= limit {
            break;
        }

        match process_manager.spawn_workflow_runner(&planned_start.dispatch, project_root) {
            Ok(()) => {
                if planned_start.selection_source == DispatchSelectionSource::DispatchQueue {
                    if let Err(error) = mark_dispatch_queue_entry_assigned(project_root, &planned_start.dispatch, None)
                    {
                        notice_sink.notice(DispatchNotice::QueueAssignmentFailed {
                            dispatch: planned_start.dispatch.clone(),
                            error: error.to_string(),
                        });
                    }
                }
                notice_sink.notice(DispatchNotice::Started {
                    dispatch: planned_start.dispatch.clone(),
                    selection_source: planned_start.selection_source,
                });
                started_workflows.push(DispatchWorkflowStart {
                    dispatch: planned_start.dispatch.clone(),
                    workflow_id: None,
                    selection_source: planned_start.selection_source,
                });
            }
            Err(error) => {
                if error.downcast_ref::<WorkflowConcurrencyCapReached>().is_some() {
                    notice_sink.notice(DispatchNotice::Deferred {
                        dispatch: planned_start.dispatch.clone(),
                        reason: error.to_string(),
                    });
                } else {
                    notice_sink.notice(DispatchNotice::Failed {
                        dispatch: planned_start.dispatch.clone(),
                        error: error.to_string(),
                    });
                }
            }
        }
    }

    DispatchWorkflowStartSummary { started: started_workflows.len(), started_workflows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{SubjectDispatch, SubjectDispatchExt};

    struct RecordingSink {
        notices: Vec<DispatchNotice>,
    }

    impl DispatchNoticeSink for RecordingSink {
        fn notice(&mut self, notice: DispatchNotice) {
            self.notices.push(notice);
        }
    }

    #[tokio::test]
    async fn concurrency_cap_rejection_emits_deferred_not_failed() {
        let _lock = crate::dispatch::test_env::lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let mut manager = ProcessManager::new().with_workflow_concurrency_max(Some(0));
        let starts = vec![PlannedDispatchStart {
            dispatch: SubjectDispatch::for_task("TASK-DEFER", "standard"),
            selection_source: DispatchSelectionSource::DispatchQueue,
        }];
        let mut sink = RecordingSink { notices: Vec::new() };

        let summary = execute_dispatch_plan_via_runner(
            temp_dir.path().to_string_lossy().as_ref(),
            &mut manager,
            &starts,
            5,
            &mut sink,
        );

        assert_eq!(summary.started, 0);
        assert_eq!(sink.notices.len(), 1);
        assert!(
            matches!(&sink.notices[0], DispatchNotice::Deferred { .. }),
            "cap rejection must surface as Deferred, not Failed; got {:?}",
            sink.notices[0]
        );
    }

    #[tokio::test]
    async fn hard_spawn_failure_emits_failed() {
        let _lock = crate::dispatch::test_env::lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let missing_runner = temp_dir.path().join("does-not-exist");
        std::env::set_var("ANIMUS_WORKFLOW_RUNNER_BIN", &missing_runner);

        let mut manager = ProcessManager::new().with_workflow_concurrency_max(None);
        let starts = vec![PlannedDispatchStart {
            dispatch: SubjectDispatch::for_task("TASK-FAIL", "standard"),
            selection_source: DispatchSelectionSource::DispatchQueue,
        }];
        let mut sink = RecordingSink { notices: Vec::new() };

        let summary = execute_dispatch_plan_via_runner(
            temp_dir.path().to_string_lossy().as_ref(),
            &mut manager,
            &starts,
            5,
            &mut sink,
        );
        std::env::remove_var("ANIMUS_WORKFLOW_RUNNER_BIN");

        assert_eq!(summary.started, 0);
        assert_eq!(sink.notices.len(), 1);
        assert!(
            matches!(&sink.notices[0], DispatchNotice::Failed { .. }),
            "missing runner binary must surface as Failed; got {:?}",
            sink.notices[0]
        );
    }
}
