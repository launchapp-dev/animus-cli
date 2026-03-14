use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use orchestrator_providers::{BuiltinGitProvider, GitProvider};

use crate::{
    services::ServiceHub, DependencyType, OrchestratorTask, OrchestratorWorkflow, TaskStatus,
    WorkflowStatus,
};

pub const DEPENDENCY_GATE_PREFIX: &str = "dependency gate:";
pub const MERGE_GATE_PREFIX: &str = "merge gate:";

pub(crate) fn dependency_blocked_reason(issues: &[String]) -> String {
    format!("{DEPENDENCY_GATE_PREFIX} {}", issues.join("; "))
}

pub fn is_dependency_gate_block(task: &OrchestratorTask) -> bool {
    task.blocked_reason
        .as_deref()
        .map(|reason| reason.starts_with(DEPENDENCY_GATE_PREFIX))
        .unwrap_or(false)
}

pub(crate) fn is_merge_gate_block(task: &OrchestratorTask) -> bool {
    task.blocked_reason
        .as_deref()
        .map(|reason| reason.starts_with(MERGE_GATE_PREFIX))
        .unwrap_or(false)
}

pub(crate) async fn dependency_gate_issues_for_task(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &OrchestratorTask,
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
                dependency.task_id, dependency_task.status
            ));
            continue;
        }

        if let Some(branch_name) = dependency_task
            .branch_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            match BuiltinGitProvider::new(project_root)
                .is_branch_merged(project_root, branch_name)
                .await
            {
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

fn active_workflow_task_ids(workflows: &[OrchestratorWorkflow]) -> HashSet<String> {
    workflows
        .iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            )
        })
        .map(|workflow| workflow.task_id.clone())
        .filter(|task_id| !task_id.is_empty())
        .collect()
}

pub async fn promote_backlog_tasks_to_ready(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids = active_workflow_task_ids(&workflows);

    let candidates = hub.tasks().list().await?;
    let mut promoted = 0usize;

    for task in &candidates {
        if task.paused || task.cancelled {
            continue;
        }
        if task.status != TaskStatus::Backlog {
            continue;
        }
        if active_task_ids.contains(&task.id) {
            continue;
        }

        let dependency_issues =
            dependency_gate_issues_for_task(hub.clone(), project_root, task).await;
        if !dependency_issues.is_empty() {
            let reason = dependency_blocked_reason(&dependency_issues);
            let _ = crate::project_task_blocked_with_reason(hub.clone(), task, reason, None).await;
            continue;
        }

        let _ = crate::project_task_status(hub.clone(), &task.id, TaskStatus::Ready).await;
        promoted = promoted.saturating_add(1);
    }

    Ok(promoted)
}

pub const DEFAULT_RETRY_COOLDOWN_SECS: i64 = 300;
pub const DEFAULT_MAX_TASK_RETRIES: usize = 3;

pub async fn retry_failed_task_workflows(hub: Arc<dyn ServiceHub>) -> Result<usize> {
    retry_failed_task_workflows_with_config(hub, DEFAULT_RETRY_COOLDOWN_SECS, DEFAULT_MAX_TASK_RETRIES).await
}

pub async fn retry_failed_task_workflows_with_config(
    hub: Arc<dyn ServiceHub>,
    cooldown_secs: i64,
    max_retries: usize,
) -> Result<usize> {

    let tasks = hub.tasks().list().await?;
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let now = Utc::now();
    let mut retried = 0usize;

    for task in &tasks {
        if retried >= 1 {
            break;
        }
        if task.paused || task.cancelled {
            continue;
        }
        if task.status != TaskStatus::Blocked {
            continue;
        }
        if is_merge_gate_block(task) || is_dependency_gate_block(task) {
            continue;
        }

        let task_workflows: Vec<_> = workflows.iter().filter(|w| w.task_id == task.id).collect();
        let latest = task_workflows.iter().max_by_key(|w| w.started_at);

        let Some(latest) = latest else {
            continue;
        };
        if latest.status != WorkflowStatus::Failed {
            continue;
        }

        let failed_count = task_workflows
            .iter()
            .filter(|w| w.status == WorkflowStatus::Failed)
            .count();
        if failed_count >= max_retries {
            continue;
        }

        if let Some(completed_at) = latest.completed_at {
            let elapsed = now.signed_duration_since(completed_at).num_seconds();
            if elapsed < cooldown_secs {
                continue;
            }
        }

        let _ = crate::project_task_status(hub.clone(), &task.id, TaskStatus::Ready).await;
        retried = retried.saturating_add(1);
    }

    Ok(retried)
}
