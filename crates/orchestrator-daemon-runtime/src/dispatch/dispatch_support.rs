use std::collections::HashSet;

use orchestrator_core::{DaemonHealth, OrchestratorWorkflow, WorkflowStatus};

use crate::DaemonRuntimeOptions;

pub fn ready_dispatch_limit(max_tasks_per_tick: usize, health: &DaemonHealth) -> usize {
    dispatch_headroom(
        max_tasks_per_tick,
        health.active_agents,
        effective_capacity_limit(&[
            health.pool_size,
        ]),
    )
}

pub fn ready_dispatch_limit_for_options(
    options: &DaemonRuntimeOptions,
    active_agents: usize,
    observed_pool_size: Option<usize>,
) -> usize {
    dispatch_headroom(
        options.max_tasks_per_tick,
        active_agents,
        effective_capacity_limit(&[
            options.pool_size,
            observed_pool_size,
        ]),
    )
}

fn effective_capacity_limit(candidates: &[Option<usize>]) -> Option<usize> {
    candidates.iter().flatten().copied().min()
}

fn dispatch_headroom(
    max_tasks_per_tick: usize,
    active_agents: usize,
    capacity_limit: Option<usize>,
) -> usize {
    if max_tasks_per_tick == 0 {
        return 0;
    }

    match capacity_limit {
        Some(limit) => max_tasks_per_tick.min(limit.saturating_sub(active_agents)),
        None => max_tasks_per_tick,
    }
}

pub fn normalize_optional_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(|candidate| candidate.to_string())
}

pub fn is_terminally_completed_workflow(workflow: &OrchestratorWorkflow) -> bool {
    workflow.status == WorkflowStatus::Completed
        && workflow.machine_state == orchestrator_core::WorkflowMachineState::Completed
        && workflow.completed_at.is_some()
}

pub fn active_workflow_task_ids(workflows: &[OrchestratorWorkflow]) -> HashSet<String> {
    workflows
        .iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            ) && workflow.machine_state != orchestrator_core::WorkflowMachineState::MergeConflict
        })
        .map(|workflow| workflow.task_id.clone())
        .collect()
}

pub fn active_workflow_subject_ids(workflows: &[OrchestratorWorkflow]) -> HashSet<String> {
    workflows
        .iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            ) && workflow.machine_state != orchestrator_core::WorkflowMachineState::MergeConflict
        })
        .map(|workflow| workflow.subject.id().to_string())
        .collect()
}

pub fn workflow_current_phase_id(workflow: &OrchestratorWorkflow) -> Option<String> {
    workflow
        .current_phase
        .as_deref()
        .map(str::to_string)
        .or_else(|| {
            workflow
                .phases
                .get(workflow.current_phase_index)
                .map(|phase| phase.phase_id.clone())
        })
        .and_then(|phase_id| normalize_optional_id(Some(phase_id.as_str())))
}

#[cfg(test)]
mod tests {
    use orchestrator_core::{DaemonHealth, DaemonStatus};

    use super::{ready_dispatch_limit, ready_dispatch_limit_for_options};
    use crate::DaemonRuntimeOptions;

    #[test]
    fn ready_dispatch_limit_uses_smallest_observed_capacity() {
        let health = DaemonHealth {
            healthy: true,
            status: DaemonStatus::Running,
            runner_connected: true,
            runner_pid: Some(42),
            active_agents: 1,
            pool_size: Some(3),
            project_root: Some("/tmp/project".to_string()),
            daemon_pid: Some(24),
            process_alive: Some(true),
            pool_utilization_percent: Some(33.0),
            queued_tasks: Some(0),
            total_agents_spawned: Some(1),
            total_agents_completed: Some(0),
            total_agents_failed: Some(0),
        };

        assert_eq!(ready_dispatch_limit(10, &health), 2);
    }

    #[test]
    fn ready_dispatch_limit_for_options_uses_smallest_available_capacity() {
        let options = DaemonRuntimeOptions {
            pool_size: Some(2),
            max_tasks_per_tick: 5,
            ..DaemonRuntimeOptions::default()
        };

        assert_eq!(
            ready_dispatch_limit_for_options(&options, 1, Some(3)),
            1
        );
    }

    #[test]
    fn ready_dispatch_limit_for_options_returns_max_tasks_when_uncapped() {
        let options = DaemonRuntimeOptions {
            max_tasks_per_tick: 4,
            ..DaemonRuntimeOptions::default()
        };

        assert_eq!(ready_dispatch_limit_for_options(&options, 2, None), 4);
    }
}
