use protocol::SubjectDispatch;

use crate::DispatchSelectionSource;

#[cfg(test)]
use std::collections::HashSet;

#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DispatchCandidate {
    pub(crate) dispatch: SubjectDispatch,
    pub(crate) selection_source: DispatchSelectionSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedDispatchStart {
    pub dispatch: SubjectDispatch,
    pub selection_source: DispatchSelectionSource,
}

impl PlannedDispatchStart {
    pub fn task_id(&self) -> Option<&str> {
        self.dispatch.task_id()
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ReadyDispatchPlan {
    pub(crate) ordered_starts: Vec<PlannedDispatchStart>,
    pub(crate) completed_subject_ids: Vec<String>,
}

#[cfg(test)]
pub(crate) fn plan_ready_dispatch(
    queued_candidates: &[DispatchCandidate],
    fallback_candidates: &[DispatchCandidate],
    completed_subject_ids: &[String],
) -> ReadyDispatchPlan {
    let mut plan = ReadyDispatchPlan::default();
    let mut seen_subject_ids = HashSet::new();
    let mut seen_completed_ids = HashSet::new();

    for subject_id in completed_subject_ids {
        if seen_completed_ids.insert(subject_id.clone()) {
            plan.completed_subject_ids.push(subject_id.clone());
        }
    }

    for candidate in queued_candidates.iter().chain(fallback_candidates.iter()) {
        let subject_id = candidate.dispatch.subject_key();
        if !seen_subject_ids.insert(subject_id) {
            continue;
        }

        plan.ordered_starts.push(PlannedDispatchStart {
            dispatch: candidate.dispatch.clone(),
            selection_source: candidate.selection_source,
        });
    }

    plan
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use protocol::SubjectDispatch;

    use super::*;

    #[test]
    fn queue_entries_take_priority_over_fallback_candidates() {
        let now = Utc.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap();
        let queued = DispatchCandidate {
            dispatch: SubjectDispatch::for_task_with_metadata(
                "TASK-1",
                orchestrator_core::STANDARD_WORKFLOW_REF,
                "em-queue",
                now,
            ),
            selection_source: DispatchSelectionSource::DispatchQueue,
        };
        let fallback = DispatchCandidate {
            dispatch: SubjectDispatch::for_task_with_metadata(
                "TASK-2",
                orchestrator_core::STANDARD_WORKFLOW_REF,
                "fallback-picker",
                now,
            ),
            selection_source: DispatchSelectionSource::FallbackPicker,
        };

        let plan = plan_ready_dispatch(&[queued], &[fallback], &[]);

        assert_eq!(plan.completed_subject_ids, Vec::<String>::new());
        assert_eq!(
            plan.ordered_starts,
            vec![
                PlannedDispatchStart {
                    dispatch: SubjectDispatch::for_task_with_metadata(
                        "TASK-1",
                        orchestrator_core::STANDARD_WORKFLOW_REF,
                        "em-queue",
                        now,
                    ),
                    selection_source: DispatchSelectionSource::DispatchQueue,
                },
                PlannedDispatchStart {
                    dispatch: SubjectDispatch::for_task_with_metadata(
                        "TASK-2",
                        orchestrator_core::STANDARD_WORKFLOW_REF,
                        "fallback-picker",
                        now,
                    ),
                    selection_source: DispatchSelectionSource::FallbackPicker,
                },
            ]
        );
    }

    #[test]
    fn falls_back_when_queue_yields_no_candidates() {
        let now = Utc.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap();
        let fallback = DispatchCandidate {
            dispatch: SubjectDispatch::for_task_with_metadata(
                "TASK-2",
                orchestrator_core::STANDARD_WORKFLOW_REF,
                "fallback-picker",
                now,
            ),
            selection_source: DispatchSelectionSource::FallbackPicker,
        };

        let plan = plan_ready_dispatch(&[], &[fallback], &[]);

        assert_eq!(
            plan.ordered_starts,
            vec![PlannedDispatchStart {
                dispatch: SubjectDispatch::for_task_with_metadata(
                    "TASK-2",
                    orchestrator_core::STANDARD_WORKFLOW_REF,
                    "fallback-picker",
                    now,
                ),
                selection_source: DispatchSelectionSource::FallbackPicker,
            }]
        );
    }

    #[test]
    fn records_completed_subjects() {
        let plan = plan_ready_dispatch(&[], &[], &["TASK-9".to_string()]);

        assert_eq!(plan.ordered_starts, Vec::<PlannedDispatchStart>::new());
        assert_eq!(plan.completed_subject_ids, vec!["TASK-9".to_string()]);
    }

    #[test]
    fn queue_entries_do_not_duplicate_fallback_candidates() {
        let now = Utc.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap();
        let queued = DispatchCandidate {
            dispatch: SubjectDispatch::for_task_with_metadata(
                "TASK-1",
                orchestrator_core::STANDARD_WORKFLOW_REF,
                "em-queue",
                now,
            ),
            selection_source: DispatchSelectionSource::DispatchQueue,
        };
        let fallback = DispatchCandidate {
            dispatch: SubjectDispatch::for_task_with_metadata(
                "TASK-1",
                orchestrator_core::STANDARD_WORKFLOW_REF,
                "fallback-picker",
                now,
            ),
            selection_source: DispatchSelectionSource::FallbackPicker,
        };

        let plan = plan_ready_dispatch(&[queued], &[fallback], &[]);

        assert_eq!(plan.ordered_starts.len(), 1);
        assert_eq!(plan.ordered_starts[0].selection_source, DispatchSelectionSource::DispatchQueue);
    }
}
