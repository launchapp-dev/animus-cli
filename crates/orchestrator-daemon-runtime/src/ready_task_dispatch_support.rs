use std::collections::HashSet;

use chrono::Utc;
use orchestrator_core::{
    Complexity, DaemonHealth, OrchestratorTask, OrchestratorWorkflow, WorkflowStatus,
};

use crate::DaemonRuntimeOptions;

pub fn ready_task_dispatch_limit(max_tasks_per_tick: usize, health: &DaemonHealth) -> usize {
    dispatch_headroom(
        max_tasks_per_tick,
        health.active_agents,
        effective_capacity_limit(&[
            health.max_agents,
            health.pool_size.map(|value| value as usize),
        ]),
    )
}

pub fn ready_task_dispatch_limit_for_options(
    options: &DaemonRuntimeOptions,
    active_agents: usize,
    observed_max_agents: Option<usize>,
    observed_pool_size: Option<usize>,
) -> usize {
    dispatch_headroom(
        options.max_tasks_per_tick,
        active_agents,
        effective_capacity_limit(&[
            options.pool_size,
            options.max_agents,
            observed_max_agents,
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

pub fn routing_complexity_for_task(
    task: &OrchestratorTask,
) -> Option<protocol::ModelRoutingComplexity> {
    match task.complexity {
        Complexity::Low => Some(protocol::ModelRoutingComplexity::Low),
        Complexity::Medium => Some(protocol::ModelRoutingComplexity::Medium),
        Complexity::High => Some(protocol::ModelRoutingComplexity::High),
    }
}

pub fn should_skip_dispatch(task: &OrchestratorTask) -> bool {
    const MAX_DISPATCH_RETRIES: u32 = 3;
    const MIN_RETRY_DELAY_SECS: i64 = 60;

    if let Some(count) = task.consecutive_dispatch_failures {
        if count >= MAX_DISPATCH_RETRIES {
            return true;
        }
    }
    if let Some(ref last_failure) = task.last_dispatch_failure_at {
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(last_failure) {
            let elapsed = Utc::now().signed_duration_since(parsed.with_timezone(&Utc));
            if elapsed.num_seconds() < MIN_RETRY_DELAY_SECS {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use orchestrator_core::{DaemonHealth, DaemonStatus};

    use super::{ready_task_dispatch_limit, ready_task_dispatch_limit_for_options};
    use crate::DaemonRuntimeOptions;

    #[test]
    fn ready_task_dispatch_limit_uses_smallest_observed_capacity() {
        let health = DaemonHealth {
            healthy: true,
            status: DaemonStatus::Running,
            runner_connected: true,
            runner_pid: Some(42),
            active_agents: 1,
            max_agents: Some(5),
            project_root: Some("/tmp/project".to_string()),
            daemon_pid: Some(24),
            process_alive: Some(true),
            pool_size: Some(3),
            pool_utilization_percent: Some(33.0),
            queued_tasks: Some(0),
            total_agents_spawned: Some(1),
            total_agents_completed: Some(0),
            total_agents_failed: Some(0),
        };

        assert_eq!(ready_task_dispatch_limit(10, &health), 2);
    }

    #[test]
    fn ready_task_dispatch_limit_for_options_uses_smallest_available_capacity() {
        let options = DaemonRuntimeOptions {
            pool_size: Some(6),
            max_agents: Some(2),
            max_tasks_per_tick: 5,
            ..DaemonRuntimeOptions::default()
        };

        assert_eq!(
            ready_task_dispatch_limit_for_options(&options, 1, Some(4), Some(3)),
            1
        );
    }

    #[test]
    fn ready_task_dispatch_limit_for_options_returns_max_tasks_when_uncapped() {
        let options = DaemonRuntimeOptions {
            max_tasks_per_tick: 4,
            ..DaemonRuntimeOptions::default()
        };

        assert_eq!(
            ready_task_dispatch_limit_for_options(&options, 2, None, None),
            4
        );
    }
}
