use crate::dispatch::process_manager::WorkflowConcurrencyCapReached;
use crate::{
    DispatchNotice, DispatchNoticeSink, DispatchWorkflowStart, DispatchWorkflowStartSummary, PlannedDispatchStart,
    ProcessManager,
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

        let spawn = match planned_start.workflow_id.as_deref() {
            Some(workflow_id) => {
                process_manager.spawn_workflow_runner_with_id(&planned_start.dispatch, project_root, workflow_id)
            }
            None => process_manager.spawn_workflow_runner(&planned_start.dispatch, project_root),
        };
        match spawn {
            Ok(()) => {
                notice_sink.notice(DispatchNotice::Started {
                    dispatch: planned_start.dispatch.clone(),
                    selection_source: planned_start.selection_source,
                });
                started_workflows.push(DispatchWorkflowStart {
                    dispatch: planned_start.dispatch.clone(),
                    workflow_id: planned_start.workflow_id.clone(),
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
    use crate::DispatchSelectionSource;
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
            workflow_id: Some("wf-defer".to_string()),
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
        let _runner_guard = protocol::test_utils::EnvVarGuard::set(
            "ANIMUS_WORKFLOW_RUNNER_BIN",
            Some(missing_runner.to_string_lossy().as_ref()),
        );

        let mut manager = ProcessManager::new().with_workflow_concurrency_max(None);
        let starts = vec![PlannedDispatchStart {
            dispatch: SubjectDispatch::for_task("TASK-FAIL", "standard"),
            workflow_id: Some("wf-fail".to_string()),
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
            matches!(&sink.notices[0], DispatchNotice::Failed { .. }),
            "missing runner binary must surface as Failed; got {:?}",
            sink.notices[0]
        );
    }
}
