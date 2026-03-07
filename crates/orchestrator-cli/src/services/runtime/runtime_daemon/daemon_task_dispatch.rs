use super::*;
use orchestrator_core::WorkflowRunInput;
pub use orchestrator_daemon_runtime::{
    load_em_work_queue_state, mark_em_work_queue_entry_assigned, plan_ready_task_dispatch,
    routing_complexity_for_task, sync_task_status_for_workflow_result,
    workflow_current_phase_id, ReadyTaskWorkflowStart,
    ReadyTaskWorkflowStartSummary, TaskSelectionSource,
};
#[cfg(test)]
pub use orchestrator_daemon_runtime::{
    em_work_queue_state_path, save_em_work_queue_state, EmWorkQueueEntry, EmWorkQueueEntryStatus,
    EmWorkQueueState,
};
pub fn daemon_agent_assignee_for_workflow_start(
    project_root: &str,
    workflow: &orchestrator_core::OrchestratorWorkflow,
    task: &orchestrator_core::OrchestratorTask,
) -> (String, Option<String>) {
    let phase_id = workflow_current_phase_id(workflow).unwrap_or_else(|| "unknown".to_string());
    let runtime_config =
        orchestrator_core::load_agent_runtime_config_or_default(Path::new(project_root));
    let role = runtime_config
        .phase_agent_id(&phase_id)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| phase_id.clone());

    let fallback_models = runtime_config.phase_fallback_models(&phase_id);
    let caps = runtime_config.phase_capabilities(&phase_id);
    let execution_targets = PhaseTargetPlanner::build_phase_execution_targets(
        &phase_id,
        runtime_config.phase_model_override(&phase_id),
        runtime_config.phase_tool_override(&phase_id),
        fallback_models.as_slice(),
        routing_complexity_for_task(task),
        Some(project_root),
        &caps,
    );
    let model = execution_targets.first().map(|(_, model)| model.clone());
    (role, model)
}

pub async fn auto_assign_task_to_daemon_agent(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    workflow: &orchestrator_core::OrchestratorWorkflow,
) -> Result<()> {
    let (role, model) = daemon_agent_assignee_for_workflow_start(project_root, workflow, task);
    hub.tasks()
        .assign_agent(&task.id, role, model, protocol::ACTOR_DAEMON.to_string())
        .await?;
    Ok(())
}

pub async fn run_ready_task_workflows_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    max_tasks_per_tick: usize,
) -> Result<ReadyTaskWorkflowStartSummary> {
    if max_tasks_per_tick == 0 {
        return Ok(ReadyTaskWorkflowStartSummary::default());
    }

    let workflows = hub.workflows().list().await.unwrap_or_default();
    let candidates = hub.tasks().list_prioritized().await?;
    let task_lookup: std::collections::HashMap<String, orchestrator_core::OrchestratorTask> =
        candidates
            .iter()
            .cloned()
            .map(|task| (task.id.clone(), task))
            .collect();
    let queue_state = match load_em_work_queue_state(project_root) {
        Ok(state) => state,
        Err(error) => {
            eprintln!(
                "{}: failed to load EM work queue state: {}",
                protocol::ACTOR_DAEMON,
                error
            );
            None
        }
    };
    let plan = plan_ready_task_dispatch(
        &candidates,
        &workflows,
        queue_state.as_ref(),
        chrono::Utc::now(),
    );

    for task_id in &plan.completed_task_ids {
        let _ = hub
            .tasks()
            .set_status(task_id, TaskStatus::Done, false)
            .await;
    }

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
            let reason = dependency_blocked_reason(&dependency_issues);
            let _ = set_task_blocked_with_reason(hub.clone(), &task, reason, None).await;
            continue;
        }

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(
                task.id.clone(),
                Some(planned_start.dispatch.pipeline_id.clone()),
            ))
            .await?;
        if planned_start.selection_source == TaskSelectionSource::EmQueue {
            if let Err(error) =
                mark_em_work_queue_entry_assigned(project_root, &task.id, workflow.id.as_str())
            {
                eprintln!(
                    "{}: failed to mark EM queue entry assigned for task {}: {}",
                    protocol::ACTOR_DAEMON,
                    task.id,
                    error
                );
            }
        }
        auto_assign_task_to_daemon_agent(hub.clone(), project_root, &task, &workflow).await?;
        sync_task_status_for_workflow_result(
            hub.clone(),
            project_root,
            &task.id,
            workflow.status,
            Some(workflow.id.as_str()),
        )
        .await;
        started_workflows.push(ReadyTaskWorkflowStart {
            task_id: task.id.clone(),
            workflow_id: workflow.id.clone(),
            selection_source: planned_start.selection_source,
        });
    }

    Ok(ReadyTaskWorkflowStartSummary {
        started: started_workflows.len(),
        started_workflows,
    })
}
