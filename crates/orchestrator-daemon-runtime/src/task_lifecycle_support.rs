use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use orchestrator_core::{services::ServiceHub, TaskStatus, WorkflowStatus};

use crate::{
    active_workflow_task_ids, dependency_blocked_reason, dependency_gate_issues_for_task,
    is_dependency_gate_block, is_merge_gate_block, set_task_blocked_with_reason,
};

const DEFAULT_RETRY_COOLDOWN_SECS: i64 = 300;
const DEFAULT_MAX_TASK_RETRIES: usize = 3;

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
            let _ = set_task_blocked_with_reason(hub.clone(), task, reason, None).await;
            continue;
        }

        let _ = hub
            .tasks()
            .set_status(&task.id, TaskStatus::Ready, false)
            .await;
        promoted = promoted.saturating_add(1);
    }

    Ok(promoted)
}

pub async fn retry_failed_task_workflows(hub: Arc<dyn ServiceHub>) -> Result<usize> {
    let cooldown_secs = std::env::var("AO_RETRY_COOLDOWN_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_RETRY_COOLDOWN_SECS);
    let max_retries = std::env::var("AO_MAX_TASK_RETRIES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_MAX_TASK_RETRIES);

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

        let _ = hub
            .tasks()
            .set_status(&task.id, TaskStatus::Ready, false)
            .await;
        retried = retried.saturating_add(1);
    }

    Ok(retried)
}
