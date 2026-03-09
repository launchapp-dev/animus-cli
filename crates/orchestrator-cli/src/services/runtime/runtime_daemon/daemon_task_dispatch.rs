use super::*;
#[cfg(test)]
use crate::services::runtime::workflow_mutation_surface::daemon_workflow_assignment;
use crate::services::runtime::workflow_mutation_surface::start_workflow_for_dispatch;
use orchestrator_core::{
    dependency_gate_issues_for_task, should_skip_task_dispatch, workflow_ref_for_task,
};
pub use orchestrator_daemon_runtime::{
    active_workflow_subject_ids, active_workflow_task_ids, is_terminally_completed_workflow,
    load_dispatch_queue_state, mark_dispatch_queue_entry_assigned, plan_ready_dispatch,
    DispatchCandidate, DispatchQueueEntryStatus, DispatchQueueState, DispatchSelectionSource,
    DispatchWorkflowStart, DispatchWorkflowStartSummary, SubjectDispatch,
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

pub async fn run_ready_task_workflows_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    max_tasks_per_tick: usize,
) -> Result<DispatchWorkflowStartSummary> {
    if max_tasks_per_tick == 0 {
        return Ok(DispatchWorkflowStartSummary::default());
    }

    let workflows = hub.workflows().list().await.unwrap_or_default();
    let candidates = hub.tasks().list_prioritized().await?;
    let active_subject_ids = active_workflow_subject_ids(&workflows);
    let task_lookup: std::collections::HashMap<String, orchestrator_core::OrchestratorTask> =
        candidates
            .iter()
            .cloned()
            .map(|task| (task.id.clone(), task))
            .collect();
    let queue_state = match load_dispatch_queue_state(project_root) {
        Ok(state) => state,
        Err(error) => {
            eprintln!(
                "{}: failed to load dispatch queue state: {}",
                protocol::ACTOR_DAEMON,
                error
            );
            None
        }
    };
    let prepared = prepare_ready_dispatch_candidates(
        &candidates,
        &workflows,
        queue_state.as_ref(),
        &active_subject_ids,
        chrono::Utc::now(),
    );
    let plan = plan_ready_dispatch(
        &prepared.queued_candidates,
        &prepared.fallback_candidates,
        &prepared.completed_subject_ids,
    );

    let mut started_workflows = Vec::new();
    for planned_start in plan.ordered_starts {
        if started_workflows.len() >= max_tasks_per_tick {
            break;
        }

        let Some(task_id) = planned_start.task_id() else {
            continue;
        };
        let Some(task) = task_lookup.get(task_id).cloned() else {
            continue;
        };
        let dependency_issues =
            dependency_gate_issues_for_task(hub.clone(), project_root, &task).await;
        if !dependency_issues.is_empty() {
            eprintln!(
                "{}: skipping queued task dispatch for {} until dependency gates clear",
                protocol::ACTOR_DAEMON,
                task.id
            );
            continue;
        }

        let workflow =
            start_workflow_for_dispatch(hub.clone(), project_root, &planned_start.dispatch).await?;
        if planned_start.selection_source == DispatchSelectionSource::DispatchQueue {
            if let Err(error) = mark_dispatch_queue_entry_assigned(
                project_root,
                &planned_start.dispatch,
                workflow.id.as_str(),
            ) {
                eprintln!(
                    "{}: failed to mark dispatch queue entry assigned for task {}: {}",
                    protocol::ACTOR_DAEMON,
                    planned_start.dispatch.subject_id(),
                    error
                );
            }
        }
        started_workflows.push(DispatchWorkflowStart {
            dispatch: planned_start.dispatch.clone(),
            workflow_id: workflow.id.clone(),
            selection_source: planned_start.selection_source,
        });
    }

    Ok(DispatchWorkflowStartSummary {
        started: started_workflows.len(),
        started_workflows,
    })
}

pub async fn dispatch_ready_tasks_via_runner(
    hub: Arc<dyn ServiceHub>,
    root: &str,
    process_manager: &mut ProcessManager,
    limit: usize,
) -> Result<DispatchWorkflowStartSummary> {
    let candidates = hub.tasks().list_prioritized().await?;
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_subject_ids = process_manager.active_subject_ids();
    let queue_state = match load_dispatch_queue_state(root) {
        Ok(state) => state,
        Err(error) => {
            eprintln!(
                "{}: failed to load dispatch queue state: {}",
                protocol::ACTOR_DAEMON,
                error
            );
            None
        }
    };
    let prepared = prepare_ready_dispatch_candidates(
        &candidates,
        &workflows,
        queue_state.as_ref(),
        &active_subject_ids,
        chrono::Utc::now(),
    );
    let task_lookup: std::collections::HashMap<String, orchestrator_core::OrchestratorTask> =
        candidates
            .iter()
            .cloned()
            .map(|task| (task.id.clone(), task))
            .collect();
    let plan = plan_ready_dispatch(
        &prepared.queued_candidates,
        &prepared.fallback_candidates,
        &prepared.completed_subject_ids,
    );
    let mut started_workflows = Vec::new();

    for planned_start in plan.ordered_starts {
        if started_workflows.len() >= limit {
            break;
        }

        if let Some(task_id) = planned_start.task_id() {
            let Some(task) = task_lookup.get(task_id).cloned() else {
                continue;
            };
            let dependency_issues = dependency_gate_issues_for_task(hub.clone(), root, &task).await;
            if !dependency_issues.is_empty() {
                eprintln!(
                    "{}: skipping queued task dispatch for {} until dependency gates clear",
                    protocol::ACTOR_DAEMON,
                    task.id
                );
                continue;
            }
        }

        match process_manager.spawn_workflow_runner(&planned_start.dispatch, root) {
            Ok(_) => {
                if planned_start.selection_source == DispatchSelectionSource::DispatchQueue {
                    if let Err(error) = mark_dispatch_queue_entry_assigned(
                        root,
                        &planned_start.dispatch,
                        planned_start.dispatch.subject_id(),
                    ) {
                        eprintln!(
                            "{}: failed to mark dispatch queue entry assigned for subject {}: {}",
                            protocol::ACTOR_DAEMON,
                            planned_start.dispatch.subject_id(),
                            error
                        );
                    }
                }
                started_workflows.push(DispatchWorkflowStart {
                    dispatch: planned_start.dispatch.clone(),
                    workflow_id: planned_start.dispatch.subject_id().to_string(),
                    selection_source: planned_start.selection_source,
                });
            }
            Err(error) => {
                eprintln!(
                    "{}: failed to start workflow runner for subject {}: {}",
                    protocol::ACTOR_DAEMON,
                    planned_start.dispatch.subject_id(),
                    error
                );
            }
        }
    }

    Ok(DispatchWorkflowStartSummary {
        started: started_workflows.len(),
        started_workflows,
    })
}

struct PreparedReadyDispatchCandidates {
    queued_candidates: Vec<DispatchCandidate>,
    fallback_candidates: Vec<DispatchCandidate>,
    completed_subject_ids: Vec<String>,
}

fn prepare_ready_dispatch_candidates(
    tasks: &[orchestrator_core::OrchestratorTask],
    workflows: &[orchestrator_core::OrchestratorWorkflow],
    queue_state: Option<&DispatchQueueState>,
    active_subject_ids: &std::collections::HashSet<String>,
    requested_at: chrono::DateTime<chrono::Utc>,
) -> PreparedReadyDispatchCandidates {
    let active_task_ids = active_workflow_task_ids(workflows);
    let completed_task_ids: std::collections::HashSet<String> = workflows
        .iter()
        .filter(|workflow| is_terminally_completed_workflow(workflow))
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let task_lookup: std::collections::HashMap<&str, &orchestrator_core::OrchestratorTask> =
        tasks.iter().map(|task| (task.id.as_str(), task)).collect();

    let mut queued_candidates = Vec::new();
    let mut fallback_candidates = Vec::new();
    let mut completed_subject_ids = Vec::new();
    let mut seen_completed_ids = std::collections::HashSet::new();

    if let Some(queue_state) = queue_state {
        for entry in &queue_state.entries {
            if entry.status != DispatchQueueEntryStatus::Pending {
                continue;
            }

            let Some(dispatch) = entry.dispatch.clone().or_else(|| {
                entry.task_id().and_then(|task_id| {
                    task_lookup.get(task_id).map(|task| {
                        SubjectDispatch::for_task_with_metadata(
                            task.id.clone(),
                            workflow_ref_for_task(task),
                            "em-queue",
                            requested_at,
                        )
                    })
                })
            }) else {
                continue;
            };

            if active_subject_ids.contains(dispatch.subject_id()) {
                continue;
            }

            if let Some(task) = dispatch
                .task_id()
                .and_then(|task_id| task_lookup.get(task_id).copied())
            {
                if !is_queued_task_dispatch_eligible(task, &active_task_ids) {
                    continue;
                }
            }

            queued_candidates.push(DispatchCandidate {
                dispatch,
                selection_source: DispatchSelectionSource::DispatchQueue,
            });
        }
    }

    for task in tasks {
        if should_include_completed_task(
            task,
            &completed_task_ids,
            &mut completed_subject_ids,
            &mut seen_completed_ids,
        ) {
            continue;
        }
        if !is_dispatch_eligible(task, &active_task_ids) {
            continue;
        }
        if active_subject_ids.contains(&task.id) {
            continue;
        }

        fallback_candidates.push(DispatchCandidate {
            dispatch: SubjectDispatch::for_task_with_metadata(
                task.id.clone(),
                workflow_ref_for_task(task),
                "fallback-picker",
                requested_at,
            ),
            selection_source: DispatchSelectionSource::FallbackPicker,
        });
    }

    PreparedReadyDispatchCandidates {
        queued_candidates,
        fallback_candidates,
        completed_subject_ids,
    }
}

fn is_queued_task_dispatch_eligible(
    task: &orchestrator_core::OrchestratorTask,
    active_task_ids: &std::collections::HashSet<String>,
) -> bool {
    if task.cancelled || task.paused {
        return false;
    }
    if active_task_ids.contains(&task.id) {
        return false;
    }
    true
}

fn is_dispatch_eligible(
    task: &orchestrator_core::OrchestratorTask,
    active_task_ids: &std::collections::HashSet<String>,
) -> bool {
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
    task: &orchestrator_core::OrchestratorTask,
    completed_task_ids: &std::collections::HashSet<String>,
    completed_targets: &mut Vec<String>,
    seen_completed_ids: &mut std::collections::HashSet<String>,
) -> bool {
    if !completed_task_ids.contains(&task.id) {
        return false;
    }
    if seen_completed_ids.insert(task.id.clone()) {
        completed_targets.push(task.id.clone());
    }
    true
}
