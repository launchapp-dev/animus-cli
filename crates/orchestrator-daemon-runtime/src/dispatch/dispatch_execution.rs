use crate::{
    mark_dispatch_queue_entry_assigned, DispatchNotice, DispatchNoticeSink,
    DispatchSelectionSource, DispatchWorkflowStart, DispatchWorkflowStartSummary,
    PlannedDispatchStart, ProcessManager,
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
                    if let Err(error) = mark_dispatch_queue_entry_assigned(
                        project_root,
                        &planned_start.dispatch,
                        None,
                    ) {
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
                notice_sink.notice(DispatchNotice::Failed {
                    dispatch: planned_start.dispatch.clone(),
                    error: error.to_string(),
                });
            }
        }
    }

    DispatchWorkflowStartSummary {
        started: started_workflows.len(),
        started_workflows,
    }
}
