use super::*;
use orchestrator_daemon_runtime::WorkflowStateReconciler;

pub async fn reconcile_dependency_gate_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    WorkflowStateReconciler::reconcile_dependency_gate_tasks_for_project(hub, project_root).await
}

pub async fn reconcile_merge_gate_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    WorkflowStateReconciler::reconcile_merge_gate_tasks_for_project(hub, project_root).await
}

pub async fn reconcile_stale_in_progress_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    stale_threshold_hours: u64,
) -> Result<usize> {
    WorkflowStateReconciler::reconcile_stale_in_progress_tasks_for_project(
        hub,
        project_root,
        stale_threshold_hours,
    )
    .await
}

pub async fn resume_interrupted_workflows_for_project(
    hub: Arc<dyn ServiceHub>,
    root: &str,
) -> Result<(usize, usize)> {
    WorkflowStateReconciler::resume_interrupted_workflows_for_project(hub, root).await
}

pub async fn recover_orphaned_running_workflows_with_active_ids(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_ids: &std::collections::HashSet<String>,
) -> usize {
    WorkflowStateReconciler::recover_orphaned_running_workflows_with_active_ids(
        hub,
        project_root,
        active_ids,
    )
    .await
}

pub async fn recover_orphaned_running_workflows_on_startup(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> usize {
    WorkflowStateReconciler::recover_orphaned_running_workflows_on_startup(hub, project_root).await
}

#[cfg(test)]
mod tests {
    use super::recover_orphaned_running_workflows_with_active_ids;
    use orchestrator_core::{
        InMemoryServiceHub, ServiceHub, TaskCreateInput, TaskStatus, TaskType, WorkflowRunInput,
        WorkflowStatus,
    };
    use std::collections::HashSet;
    use std::sync::Arc;

    #[tokio::test]
    async fn active_subject_ids_prevent_runner_backed_workflow_from_being_recovered() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "runner-backed-workflow".to_string(),
                description: "should remain running while subprocess is active".to_string(),
                task_type: Some(TaskType::Feature),
                priority: None,
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&task.id, TaskStatus::InProgress, false)
            .await
            .expect("task should be in progress");
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task.id.clone(), None))
            .await
            .expect("workflow should start");

        let recovered = recover_orphaned_running_workflows_with_active_ids(
            hub.clone() as Arc<dyn ServiceHub>,
            "/tmp/project",
            &HashSet::from([task.id.clone()]),
        )
        .await;

        assert_eq!(recovered, 0);
        let workflow_state = hub
            .workflows()
            .get(&workflow.id)
            .await
            .expect("workflow should still be readable");
        assert_eq!(workflow_state.status, WorkflowStatus::Running);
    }
}
