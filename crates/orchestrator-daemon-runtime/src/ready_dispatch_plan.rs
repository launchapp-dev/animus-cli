use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use orchestrator_core::{
    OrchestratorTask, OrchestratorWorkflow, TaskStatus, should_skip_task_dispatch,
    workflow_ref_for_task,
};

use crate::{
    active_workflow_task_ids, is_terminally_completed_workflow, DispatchSelectionSource,
    EmWorkQueueEntryStatus, EmWorkQueueState, SubjectDispatch,
};

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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadyDispatchPlan {
    pub ordered_starts: Vec<PlannedDispatchStart>,
    pub completed_task_ids: Vec<String>,
}

pub fn plan_ready_dispatch(
    tasks: &[OrchestratorTask],
    workflows: &[OrchestratorWorkflow],
    em_queue_state: Option<&EmWorkQueueState>,
    requested_at: DateTime<Utc>,
) -> ReadyDispatchPlan {
    let active_task_ids = active_workflow_task_ids(workflows);
    let completed_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| is_terminally_completed_workflow(workflow))
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let task_lookup: HashMap<&str, &OrchestratorTask> =
        tasks.iter().map(|task| (task.id.as_str(), task)).collect();

    let mut plan = ReadyDispatchPlan::default();
    let mut seen_task_ids = HashSet::new();
    let mut seen_completed_ids = HashSet::new();

    if let Some(queue_state) = em_queue_state {
        for entry in &queue_state.entries {
            if entry.status != EmWorkQueueEntryStatus::Pending {
                continue;
            }

            let Some(task) = task_lookup.get(entry.task_id.as_str()).copied() else {
                continue;
            };
            if !seen_task_ids.insert(task.id.clone()) {
                continue;
            }
            if should_include_completed_task(
                task,
                &completed_task_ids,
                &mut plan.completed_task_ids,
                &mut seen_completed_ids,
            ) {
                continue;
            }
            if !is_dispatch_eligible(task, &active_task_ids) {
                continue;
            }

            let dispatch = entry.dispatch.clone().unwrap_or_else(|| {
                SubjectDispatch::for_task_with_metadata(
                    task.id.clone(),
                    workflow_ref_for_task(task),
                    "em-queue",
                    requested_at,
                )
            });

            plan.ordered_starts.push(PlannedDispatchStart {
                dispatch,
                selection_source: DispatchSelectionSource::EmQueue,
            });
        }
    }

    for task in tasks {
        if !seen_task_ids.insert(task.id.clone()) {
            continue;
        }
        if should_include_completed_task(
            task,
            &completed_task_ids,
            &mut plan.completed_task_ids,
            &mut seen_completed_ids,
        ) {
            continue;
        }
        if !is_dispatch_eligible(task, &active_task_ids) {
            continue;
        }

        plan.ordered_starts.push(PlannedDispatchStart {
            dispatch: SubjectDispatch::for_task_with_metadata(
                task.id.clone(),
                workflow_ref_for_task(task),
                "fallback-picker",
                requested_at,
            ),
            selection_source: DispatchSelectionSource::FallbackPicker,
        });
    }

    plan
}

fn is_dispatch_eligible(task: &OrchestratorTask, active_task_ids: &HashSet<String>) -> bool {
    if task.paused || task.cancelled {
        return false;
    }
    if task.status != TaskStatus::Ready {
        return false;
    }
    if active_task_ids.contains(&task.id) {
        return false;
    }
    if should_skip_task_dispatch(task) {
        return false;
    }
    true
}

fn should_include_completed_task(
    task: &OrchestratorTask,
    completed_task_ids: &HashSet<String>,
    completed_targets: &mut Vec<String>,
    seen_completed_ids: &mut HashSet<String>,
) -> bool {
    if !completed_task_ids.contains(&task.id) {
        return false;
    }
    if seen_completed_ids.insert(task.id.clone()) {
        completed_targets.push(task.id.clone());
    }
    true
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use orchestrator_core::{
        Assignee, Complexity, OrchestratorTask, OrchestratorWorkflow, Priority,
        ResourceRequirements, TaskMetadata, TaskStatus, TaskType, WorkflowCheckpointMetadata,
        WorkflowMachineState, WorkflowMetadata, WorkflowPhaseExecution, WorkflowPhaseStatus,
        WorkflowStatus, WorkflowSubject,
    };

    use super::*;
    use crate::{EmWorkQueueEntry, EmWorkQueueState};

    #[test]
    fn queue_entries_take_priority_over_fallback_candidates() {
        let queued = task("TASK-1", TaskStatus::Ready);
        let fallback = task("TASK-2", TaskStatus::Ready);
        let now = Utc.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap();
        let queue = EmWorkQueueState {
            entries: vec![EmWorkQueueEntry {
                task_id: "TASK-1".to_string(),
                dispatch: None,
                status: EmWorkQueueEntryStatus::Pending,
                workflow_id: None,
                assigned_at: None,
                held_at: None,
            }],
        };

        let plan = plan_ready_dispatch(&[queued, fallback], &[], Some(&queue), now);

        assert_eq!(plan.completed_task_ids, Vec::<String>::new());
        assert_eq!(
            plan.ordered_starts,
            vec![
                PlannedDispatchStart {
                    dispatch: SubjectDispatch::for_task_with_metadata(
                        "TASK-1",
                        orchestrator_core::STANDARD_PIPELINE_ID,
                        "em-queue",
                        now,
                    ),
                    selection_source: DispatchSelectionSource::EmQueue,
                },
                PlannedDispatchStart {
                    dispatch: SubjectDispatch::for_task_with_metadata(
                        "TASK-2",
                        orchestrator_core::STANDARD_PIPELINE_ID,
                        "fallback-picker",
                        now,
                    ),
                    selection_source: DispatchSelectionSource::FallbackPicker,
                },
            ]
        );
    }

    #[test]
    fn falls_back_to_prioritized_tasks_when_queue_yields_no_starts() {
        let queued = task("TASK-1", TaskStatus::Blocked);
        let fallback = task("TASK-2", TaskStatus::Ready);
        let now = Utc.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap();
        let queue = EmWorkQueueState {
            entries: vec![EmWorkQueueEntry {
                task_id: "TASK-1".to_string(),
                dispatch: None,
                status: EmWorkQueueEntryStatus::Pending,
                workflow_id: None,
                assigned_at: None,
                held_at: None,
            }],
        };

        let plan = plan_ready_dispatch(&[queued, fallback], &[], Some(&queue), now);

        assert_eq!(
            plan.ordered_starts,
            vec![PlannedDispatchStart {
                dispatch: SubjectDispatch::for_task_with_metadata(
                    "TASK-2",
                    orchestrator_core::STANDARD_PIPELINE_ID,
                    "fallback-picker",
                    now,
                ),
                selection_source: DispatchSelectionSource::FallbackPicker,
            }]
        );
    }

    #[test]
    fn records_completed_tasks_instead_of_restarting_them() {
        let done_candidate = task("TASK-9", TaskStatus::Ready);
        let workflows = vec![completed_workflow("wf-1", "TASK-9")];
        let now = Utc.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap();

        let plan = plan_ready_dispatch(&[done_candidate], &workflows, None, now);

        assert_eq!(plan.ordered_starts, Vec::<PlannedDispatchStart>::new());
        assert_eq!(plan.completed_task_ids, vec!["TASK-9".to_string()]);
    }

    #[test]
    fn queue_entries_do_not_duplicate_fallback_candidates() {
        let queued = task("TASK-1", TaskStatus::Ready);
        let now = Utc.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap();
        let queue = EmWorkQueueState {
            entries: vec![EmWorkQueueEntry {
                task_id: "TASK-1".to_string(),
                dispatch: None,
                status: EmWorkQueueEntryStatus::Pending,
                workflow_id: None,
                assigned_at: None,
                held_at: None,
            }],
        };

        let plan = plan_ready_dispatch(&[queued], &[], Some(&queue), now);

        assert_eq!(
            plan.ordered_starts,
            vec![PlannedDispatchStart {
                dispatch: SubjectDispatch::for_task_with_metadata(
                    "TASK-1",
                    orchestrator_core::STANDARD_PIPELINE_ID,
                    "em-queue",
                    now,
                ),
                selection_source: DispatchSelectionSource::EmQueue,
            }]
        );
    }

    fn task(id: &str, status: TaskStatus) -> OrchestratorTask {
        OrchestratorTask {
            id: id.to_string(),
            title: id.to_string(),
            description: String::new(),
            status,
            task_type: TaskType::Feature,
            priority: Priority::Medium,
            complexity: Complexity::Medium,
            risk: orchestrator_core::RiskLevel::default(),
            scope: orchestrator_core::Scope::default(),
            impact_area: Vec::new(),
            assignee: Assignee::default(),
            estimated_effort: None,
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
            dependencies: Vec::new(),
            checklist: Vec::new(),
            tags: Vec::new(),
            workflow_metadata: WorkflowMetadata::default(),
            worktree_path: None,
            branch_name: None,
            metadata: TaskMetadata {
                created_at: Utc::now(),
                updated_at: Utc::now(),
                created_by: "test".to_string(),
                updated_by: "test".to_string(),
                started_at: None,
                completed_at: None,
                version: 1,
            },
            deadline: None,
            paused: false,
            cancelled: false,
            blocked_reason: None,
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            resolution: None,
            resource_requirements: ResourceRequirements::default(),
            consecutive_dispatch_failures: None,
            last_dispatch_failure_at: None,
            dispatch_history: Vec::new(),
        }
    }

    fn completed_workflow(id: &str, task_id: &str) -> OrchestratorWorkflow {
        let now = Utc::now();
        OrchestratorWorkflow {
            id: id.to_string(),
            task_id: task_id.to_string(),
            pipeline_id: Some(orchestrator_core::STANDARD_PIPELINE_ID.to_string()),
            subject: WorkflowSubject::Task {
                id: task_id.to_string(),
            },
            status: WorkflowStatus::Completed,
            current_phase: None,
            current_phase_index: 0,
            phases: vec![WorkflowPhaseExecution {
                phase_id: "implementation".to_string(),
                status: WorkflowPhaseStatus::Success,
                started_at: Some(now),
                completed_at: Some(now),
                attempt: 1,
                error_message: None,
            }],
            machine_state: WorkflowMachineState::Completed,
            started_at: now,
            completed_at: Some(now),
            failure_reason: None,
            checkpoint_metadata: WorkflowCheckpointMetadata::default(),
            rework_counts: Default::default(),
            total_reworks: 0,
            decision_history: Vec::new(),
        }
    }
}
