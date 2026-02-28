use super::*;

pub async fn set_task_blocked_with_reason(
    hub: Arc<dyn ServiceHub>,
    task: &orchestrator_core::OrchestratorTask,
    reason: String,
    blocked_by: Option<String>,
) -> Result<()> {
    let mut updated = task.clone();
    updated.status = TaskStatus::Blocked;
    updated.paused = true;
    updated.blocked_reason = Some(reason);
    updated.blocked_at = Some(Utc::now());
    updated.blocked_phase = None;
    updated.blocked_by = blocked_by;
    updated.metadata.updated_at = Utc::now();
    updated.metadata.updated_by = "ao-daemon".to_string();
    updated.metadata.version = updated.metadata.version.saturating_add(1);
    hub.tasks().replace(updated).await?;
    Ok(())
}

pub async fn dependency_gate_issues_for_task(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
) -> Vec<String> {
    let mut issues = Vec::new();

    for dependency in &task.dependencies {
        if dependency.dependency_type != DependencyType::BlockedBy {
            continue;
        }

        let dependency_task = match hub.tasks().get(&dependency.task_id).await {
            Ok(task) => task,
            Err(_) => {
                issues.push(format!("dependency {} does not exist", dependency.task_id));
                continue;
            }
        };

        if dependency_task.status != TaskStatus::Done {
            issues.push(format!(
                "dependency {} is {}",
                dependency.task_id,
                task_status_label(dependency_task.status)
            ));
            continue;
        }

        if let Some(branch_name) = dependency_task
            .branch_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            match is_branch_merged(project_root, branch_name) {
                Ok(Some(true)) | Ok(None) => {}
                Ok(Some(false)) => {
                    issues.push(format!(
                        "dependency {} branch `{}` is not merged",
                        dependency.task_id, branch_name
                    ));
                }
                Err(error) => {
                    issues.push(format!(
                        "unable to verify dependency {} merge status: {}",
                        dependency.task_id, error
                    ));
                }
            }
        }
    }

    issues
}

pub async fn reconcile_dependency_gate_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids: HashSet<String> = workflows
        .into_iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            )
        })
        .map(|workflow| workflow.task_id)
        .collect();

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
                hub.tasks().set_status(&task.id, TaskStatus::Ready).await?;
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
            set_task_blocked_with_reason(hub.clone(), &task, reason, None).await?;
            changed = changed.saturating_add(1);
        }
    }

    Ok(changed)
}

pub async fn reconcile_merge_gate_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids: HashSet<String> = workflows
        .into_iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            )
        })
        .map(|workflow| workflow.task_id)
        .collect();

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
            hub.tasks().set_status(&task.id, TaskStatus::Done).await?;
            resolved = resolved.saturating_add(1);
            continue;
        };

        match is_branch_merged(project_root, branch_name) {
            Ok(Some(true)) | Ok(None) => {
                hub.tasks().set_status(&task.id, TaskStatus::Done).await?;
                resolved = resolved.saturating_add(1);
            }
            Ok(Some(false)) | Err(_) => {}
        }
    }

    Ok(resolved)
}

pub async fn reconcile_stale_in_progress_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            )
        })
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let completed_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| is_terminally_completed_workflow(workflow))
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let failed_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Failed)
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let cancelled_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Cancelled)
        .map(|workflow| workflow.task_id.clone())
        .collect();

    let tasks = hub.tasks().list().await?;
    let mut reconciled = 0usize;
    let now = Utc::now();
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
        let age_minutes = now
            .signed_duration_since(task.metadata.updated_at)
            .num_minutes()
            .max(0);
        if age_minutes < 10 {
            continue;
        }
        hub.tasks().set_status(&task.id, TaskStatus::Ready).await?;
        reconciled = reconciled.saturating_add(1);
    }
    Ok(reconciled)
}

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

pub async fn recover_orphaned_running_workflows(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> usize {
    let workflows = match hub.workflows().list().await {
        Ok(w) => w,
        Err(_) => return 0,
    };
    let in_flight: std::collections::HashSet<String> =
        with_reactive_phase_pool_state_mut(project_root, |state| {
            state.in_flight_workflow_ids.clone()
        });

    let mut recovered = 0usize;
    for workflow in workflows {
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        if in_flight.contains(&workflow.id) {
            continue;
        }

        eprintln!(
            "ao-daemon: recovering orphaned running workflow {} (task {})",
            workflow.id, workflow.task_id
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
        if hub
            .tasks()
            .set_status(&task_id, TaskStatus::Ready)
            .await
            .is_ok()
        {
            recovered = recovered.saturating_add(1);
        }
    }
    recovered
}
