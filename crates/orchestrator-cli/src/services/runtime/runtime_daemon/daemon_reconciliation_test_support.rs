use super::*;
use crate::services::runtime::sync_task_status_for_workflow_result;
use orchestrator_core::{
    dependency_blocked_reason, dependency_gate_issues_for_task, is_dependency_gate_block,
    is_merge_gate_block, project_task_blocked_with_reason, project_task_status,
    services::ServiceHub, TaskStatus, WorkflowMachineState, WorkflowResumeManager, WorkflowStatus,
};
use orchestrator_daemon_runtime::{active_workflow_task_ids, is_terminally_completed_workflow};
use orchestrator_git_ops::is_branch_merged;
use std::path::Path;

#[cfg(test)]
pub async fn reconcile_dependency_gate_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids = active_workflow_task_ids(&workflows);

    let mut changed = 0usize;
    let tasks = hub.tasks().list().await?;
    for task in tasks {
        if active_task_ids.contains(&task.id) || task.cancelled {
            continue;
        }

        let dependency_issues =
            dependency_gate_issues_for_task(hub.clone(), project_root, &task).await;
        if dependency_issues.is_empty() {
            if task.status == TaskStatus::Blocked && is_dependency_gate_block(&task) {
                project_task_status(hub.clone(), &task.id, TaskStatus::Ready).await?;
                changed = changed.saturating_add(1);
            }
            continue;
        }

        let reason = dependency_blocked_reason(&dependency_issues);
        let should_block = match task.status {
            TaskStatus::Ready | TaskStatus::Backlog => true,
            TaskStatus::Blocked => task.blocked_reason.as_deref() != Some(reason.as_str()),
            _ => false,
        };

        if should_block {
            project_task_blocked_with_reason(hub.clone(), &task, reason, None).await?;
            changed = changed.saturating_add(1);
        }
    }

    Ok(changed)
}

#[cfg(test)]
pub async fn reconcile_merge_gate_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids = active_workflow_task_ids(&workflows);

    let mut resolved = 0usize;
    let tasks = hub.tasks().list().await?;
    for task in tasks {
        if active_task_ids.contains(&task.id) {
            continue;
        }
        if task.status != TaskStatus::Blocked || !is_merge_gate_block(&task) {
            continue;
        }

        let Some(branch_name) = task
            .branch_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            project_task_status(hub.clone(), &task.id, TaskStatus::Done).await?;
            resolved = resolved.saturating_add(1);
            continue;
        };

        match is_branch_merged(project_root, branch_name) {
            Ok(Some(true)) | Ok(None) => {
                project_task_status(hub.clone(), &task.id, TaskStatus::Done).await?;
                resolved = resolved.saturating_add(1);
            }
            Ok(Some(false)) | Err(_) => {}
        }
    }

    Ok(resolved)
}

#[cfg(test)]
pub async fn reconcile_stale_in_progress_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    stale_threshold_hours: u64,
) -> Result<usize> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids = active_workflow_task_ids(&workflows);
    let completed_task_ids: std::collections::HashSet<String> = workflows
        .iter()
        .filter(|workflow| is_terminally_completed_workflow(workflow))
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let failed_task_ids: std::collections::HashSet<String> = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Failed)
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let cancelled_task_ids: std::collections::HashSet<String> = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Cancelled)
        .map(|workflow| workflow.task_id.clone())
        .collect();

    let tasks = hub.tasks().list().await?;
    let mut reconciled = 0usize;
    let now = chrono::Utc::now();
    for task in tasks {
        if task.status != TaskStatus::InProgress {
            continue;
        }
        if active_task_ids.contains(&task.id) {
            continue;
        }
        if completed_task_ids.contains(&task.id) {
            sync_task_status_for_workflow_result(
                hub.clone(),
                project_root,
                &task.id,
                WorkflowStatus::Completed,
                None,
            )
            .await;
            reconciled = reconciled.saturating_add(1);
            continue;
        }
        if failed_task_ids.contains(&task.id) {
            sync_task_status_for_workflow_result(
                hub.clone(),
                project_root,
                &task.id,
                WorkflowStatus::Failed,
                None,
            )
            .await;
            reconciled = reconciled.saturating_add(1);
            continue;
        }
        if cancelled_task_ids.contains(&task.id) {
            sync_task_status_for_workflow_result(
                hub.clone(),
                project_root,
                &task.id,
                WorkflowStatus::Cancelled,
                None,
            )
            .await;
            reconciled = reconciled.saturating_add(1);
            continue;
        }
        let threshold_minutes = (stale_threshold_hours * 60) as i64;
        let age_minutes = now
            .signed_duration_since(task.metadata.updated_at)
            .num_minutes()
            .max(0);
        if age_minutes < threshold_minutes {
            continue;
        }
        project_task_status(hub.clone(), &task.id, TaskStatus::Ready).await?;
        reconciled = reconciled.saturating_add(1);
    }

    Ok(reconciled)
}

#[cfg(test)]
pub async fn resume_interrupted_workflows_for_project(
    hub: Arc<dyn ServiceHub>,
    root: &str,
) -> Result<(usize, usize)> {
    let resume_manager = WorkflowResumeManager::new(root)?;
    let cleaned = resume_manager.cleanup_stale_workflows()?;
    let resumable = resume_manager.get_resumable_workflows()?;

    let mut resumed = 0usize;
    for (workflow, _) in resumable {
        let updated = hub.workflows().resume(&workflow.id).await?;
        resumed = resumed.saturating_add(1);
        sync_task_status_for_workflow_result(
            hub.clone(),
            root,
            &updated.task_id,
            updated.status,
            Some(updated.id.as_str()),
        )
        .await;
    }

    Ok((cleaned, resumed))
}

#[cfg(test)]
pub async fn recover_orphaned_running_workflows_with_active_ids(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_ids: &std::collections::HashSet<String>,
) -> usize {
    let workflows = match hub.workflows().list().await {
        Ok(w) => w,
        Err(_) => return 0,
    };

    let mut recovered = 0usize;
    for workflow in workflows {
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        if workflow.machine_state == WorkflowMachineState::MergeConflict {
            continue;
        }
        if workflow_is_waiting_on_manual_phase(project_root, &workflow) {
            continue;
        }
        if active_ids.contains(&workflow.id)
            || active_ids.contains(workflow.subject.id())
            || (!workflow.task_id.is_empty() && active_ids.contains(&workflow.task_id))
        {
            continue;
        }

        eprintln!(
            "{}: recovering orphaned running workflow {} (task {})",
            protocol::ACTOR_DAEMON,
            workflow.id,
            workflow.task_id
        );
        let task_id = workflow.task_id.clone();
        if let Ok(_updated) = hub.workflows().cancel(&workflow.id).await {
            sync_task_status_for_workflow_result(
                hub.clone(),
                project_root,
                &task_id,
                WorkflowStatus::Cancelled,
                Some(workflow.id.as_str()),
            )
            .await;
        }
        recovered = recovered.saturating_add(1);
    }

    recovered
}

#[cfg(test)]
fn workflow_is_waiting_on_manual_phase(
    project_root: &str,
    workflow: &orchestrator_core::OrchestratorWorkflow,
) -> bool {
    let Some(phase_id) = workflow
        .current_phase
        .clone()
        .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
    else {
        return false;
    };

    orchestrator_core::load_agent_runtime_config(Path::new(project_root))
        .ok()
        .and_then(|config| config.phase_execution(&phase_id).cloned())
        .map(|definition| matches!(definition.mode, orchestrator_core::PhaseExecutionMode::Manual))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::recover_orphaned_running_workflows_with_active_ids;
    use orchestrator_core::{
        builtin_agent_runtime_config, write_agent_runtime_config, FileServiceHub, InMemoryServiceHub,
        PhaseExecutionMode, PhaseManualDefinition, ServiceHub, TaskCreateInput, TaskStatus,
        TaskType, WorkflowRunInput, WorkflowStatus,
    };
    use std::collections::HashSet;
    use std::sync::Arc;
    use tempfile::TempDir;

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

    #[tokio::test]
    async fn manual_phase_workflows_are_not_recovered_as_orphans() {
        let temp = TempDir::new().expect("temp dir");
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file hub"));

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "manual-gate".to_string(),
                description: "should survive without an active runner".to_string(),
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

        let current_phase = workflow
            .current_phase
            .clone()
            .expect("workflow should have current phase");
        let mut runtime = builtin_agent_runtime_config();
        let mut definition = runtime
            .phase_execution(&current_phase)
            .cloned()
            .expect("current phase should exist in runtime config");
        definition.mode = PhaseExecutionMode::Manual;
        definition.agent_id = None;
        definition.command = None;
        definition.manual = Some(PhaseManualDefinition {
            instructions: "Approve this step".to_string(),
            approval_note_required: false,
        });
        runtime.phases.insert(current_phase.clone(), definition);
        write_agent_runtime_config(temp.path(), &runtime).expect("runtime config should write");

        let recovered = recover_orphaned_running_workflows_with_active_ids(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root,
            &HashSet::new(),
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
