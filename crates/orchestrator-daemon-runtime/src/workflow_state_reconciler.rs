use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use orchestrator_core::{
    services::ServiceHub, TaskStatus, WorkflowMachineState, WorkflowResumeManager, WorkflowStatus,
};
use orchestrator_git_ops::is_branch_merged;

use crate::{
    active_workflow_task_ids, dependency_blocked_reason, dependency_gate_issues_for_task,
    is_dependency_gate_block, is_merge_gate_block, is_terminally_completed_workflow,
    project_task_blocked_with_reason, project_task_status, sync_task_status_for_workflow_result,
};

pub struct WorkflowStateReconciler;

impl WorkflowStateReconciler {
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

    pub async fn reconcile_stale_in_progress_tasks_for_project(
        hub: Arc<dyn ServiceHub>,
        project_root: &str,
        stale_threshold_hours: u64,
    ) -> Result<usize> {
        let workflows = hub.workflows().list().await.unwrap_or_default();
        let active_task_ids = active_workflow_task_ids(&workflows);
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

    pub async fn recover_orphaned_running_workflows_with_active_ids(
        hub: Arc<dyn ServiceHub>,
        project_root: &str,
        active_ids: &HashSet<String>,
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

    pub async fn recover_orphaned_running_workflows_on_startup(
        hub: Arc<dyn ServiceHub>,
        project_root: &str,
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

            eprintln!(
                "{}: startup orphan detection — cancelling orphaned workflow {} (task {})",
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
            let task = match hub.tasks().get(&task_id).await {
                Ok(t) => t,
                Err(_) => {
                    recovered = recovered.saturating_add(1);
                    continue;
                }
            };
            let _ = project_task_blocked_with_reason(
                hub.clone(),
                &task,
                "orphaned_after_daemon_restart".to_string(),
                Some(workflow.id.clone()),
            )
            .await;
            recovered = recovered.saturating_add(1);
        }
        recovered
    }
}
