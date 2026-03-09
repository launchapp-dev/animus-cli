use super::*;
use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;
use crate::services::runtime::runtime_daemon::daemon_reconciliation::recover_orphaned_running_workflows;
use orchestrator_core::{
    dependency_blocked_reason, dependency_gate_issues_for_task, dispatch_workflow_event,
    is_dependency_gate_block, is_merge_gate_block, project_task_blocked_with_reason,
    project_task_status, services::ServiceHub, TaskStatus, WorkflowEvent, WorkflowResumeManager,
    WorkflowStatus,
};
use orchestrator_daemon_runtime::{active_workflow_task_ids, is_terminally_completed_workflow};
use orchestrator_git_ops::is_branch_merged;

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
            project_terminal_workflow_result(
                hub.clone(),
                project_root,
                &task.id,
                Some(&task.id),
                None,
                None,
                WorkflowStatus::Completed,
                None,
            )
            .await;
            reconciled = reconciled.saturating_add(1);
            continue;
        }
        if failed_task_ids.contains(&task.id) {
            project_terminal_workflow_result(
                hub.clone(),
                project_root,
                &task.id,
                Some(&task.id),
                None,
                None,
                WorkflowStatus::Failed,
                None,
            )
            .await;
            reconciled = reconciled.saturating_add(1);
            continue;
        }
        if cancelled_task_ids.contains(&task.id) {
            project_terminal_workflow_result(
                hub.clone(),
                project_root,
                &task.id,
                Some(&task.id),
                None,
                None,
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
        let _ = dispatch_workflow_event(
            hub.clone(),
            project_root,
            WorkflowEvent::StaleReset {
                task_id: task.id.clone(),
                reason: Some(format!(
                    "stale in-progress task exceeded {} minutes",
                    threshold_minutes
                )),
            },
        )
        .await?;
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
        let outcome = dispatch_workflow_event(
            hub.clone(),
            root,
            WorkflowEvent::Resume {
                workflow_id: workflow.id.clone(),
            },
        )
        .await?;
        if let Some(updated) = outcome.workflow {
            resumed = resumed.saturating_add(1);
            project_terminal_workflow_result(
                hub.clone(),
                root,
                &updated.task_id,
                Some(updated.task_id.as_str()),
                updated.workflow_ref.as_deref(),
                Some(updated.id.as_str()),
                updated.status,
                None,
            )
            .await;
        }
    }

    Ok((cleaned, resumed))
}

#[cfg(test)]
pub async fn recover_orphaned_running_workflows_with_active_ids(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_ids: &std::collections::HashSet<String>,
) -> usize {
    recover_orphaned_running_workflows(hub, project_root, active_ids).await
}

#[cfg(test)]
mod tests {
    use super::recover_orphaned_running_workflows_with_active_ids;
    use orchestrator_core::{
        builtin_agent_runtime_config, register_workflow_runner_pid, unregister_workflow_runner_pid,
        write_agent_runtime_config, FileServiceHub, InMemoryServiceHub, PhaseExecutionMode,
        PhaseManualDefinition, ServiceHub, TaskCreateInput, TaskStatus, TaskType, WorkflowRunInput,
        WorkflowStatus,
    };
    use std::collections::HashSet;
    use std::path::Path;
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
    async fn registered_workflow_runner_ids_prevent_recovery() {
        let temp = TempDir::new().expect("tempdir should be created");
        let project_root = temp.path().to_string_lossy().to_string();
        let hub = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "externally active workflow".to_string(),
                description: "runner registry should protect it".to_string(),
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

        register_workflow_runner_pid(Path::new(&project_root), &workflow.id, std::process::id())
            .expect("workflow registry entry should be written");

        let recovered = recover_orphaned_running_workflows_with_active_ids(
            hub.clone(),
            &project_root,
            &HashSet::new(),
        )
        .await;
        unregister_workflow_runner_pid(Path::new(&project_root), &workflow.id)
            .expect("workflow registry entry should be removed");

        assert_eq!(recovered, 0);
        let reloaded = hub
            .workflows()
            .get(&workflow.id)
            .await
            .expect("workflow should still exist");
        assert_eq!(reloaded.status, WorkflowStatus::Running);
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
            timeout_secs: None,
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
