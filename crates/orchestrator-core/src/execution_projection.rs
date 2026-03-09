mod project_requirement_workflow_status;
mod project_task_terminal_workflow_status;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use protocol::SubjectExecutionFact;

use crate::{
    load_schedule_state, save_schedule_state, services::ServiceHub, OrchestratorTask,
    ScheduleRunState, TaskStatus, WorkflowStatus,
};

pub use project_requirement_workflow_status::project_requirement_workflow_status;
pub use project_task_terminal_workflow_status::project_task_terminal_workflow_status;

pub async fn project_task_status(
    hub: Arc<dyn ServiceHub>,
    task_id: &str,
    status: TaskStatus,
) -> Result<()> {
    hub.tasks().set_status(task_id, status, false).await?;
    Ok(())
}

pub async fn project_task_blocked_with_reason(
    hub: Arc<dyn ServiceHub>,
    task: &OrchestratorTask,
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
    updated.metadata.updated_by = protocol::ACTOR_DAEMON.to_string();
    updated.metadata.version = updated.metadata.version.saturating_add(1);
    hub.tasks().replace(updated).await?;
    Ok(())
}

pub async fn project_task_dispatch_failure(
    hub: Arc<dyn ServiceHub>,
    task_id: &str,
    max_dispatch_retries: u32,
) -> Result<()> {
    let task = match hub.tasks().get(task_id).await {
        Ok(task) => task,
        Err(_) => {
            return project_task_status(hub, task_id, TaskStatus::Blocked).await;
        }
    };

    let count = task
        .consecutive_dispatch_failures
        .unwrap_or(0)
        .saturating_add(1);

    if count >= max_dispatch_retries {
        let reason = format!("auto-blocked after {} consecutive dispatch failures", count);
        return project_task_blocked_with_reason(hub, &task, reason, None).await;
    }

    let mut updated = task;
    updated.consecutive_dispatch_failures = Some(count);
    updated.last_dispatch_failure_at = Some(Utc::now().to_rfc3339());
    hub.tasks().replace(updated).await?;
    project_task_status(hub, task_id, TaskStatus::Blocked).await
}

pub async fn project_task_workflow_start(
    hub: Arc<dyn ServiceHub>,
    task_id: &str,
    role: String,
    model: Option<String>,
    updated_by: String,
) -> Result<()> {
    hub.tasks()
        .set_status(task_id, TaskStatus::InProgress, false)
        .await?;
    hub.tasks()
        .assign_agent(task_id, role, model, updated_by)
        .await?;
    Ok(())
}

pub async fn project_task_execution_fact(
    hub: Arc<dyn ServiceHub>,
    _root: &str,
    fact: &SubjectExecutionFact,
) {
    let Some(task_id) = fact.task_id.as_deref() else {
        return;
    };

    if let Some(status) = fact.workflow_status {
        match status {
            WorkflowStatus::Pending | WorkflowStatus::Running | WorkflowStatus::Paused => return,
            WorkflowStatus::Completed => {
                let _ = project_task_status(hub, task_id, TaskStatus::Done).await;
                return;
            }
            WorkflowStatus::Cancelled => {
                let _ = project_task_status(hub, task_id, TaskStatus::Cancelled).await;
                return;
            }
            WorkflowStatus::Failed | WorkflowStatus::Escalated => {}
        }
    }

    if fact.success {
        let _ = project_task_status(hub, task_id, TaskStatus::Done).await;
        return;
    }

    if let Some(reason) = fact.failure_reason.clone() {
        if let Ok(task) = hub.tasks().get(task_id).await {
            let _ = project_task_blocked_with_reason(hub, &task, reason, None).await;
            return;
        }
    }

    let _ = project_task_status(hub, task_id, TaskStatus::Blocked).await;
}

pub fn project_schedule_dispatch_attempt(
    root: &str,
    schedule_id: &str,
    run_at: chrono::DateTime<Utc>,
    status: &str,
) {
    update_schedule_state(root, schedule_id, Some(run_at), status, true);
}

pub fn project_schedule_completion_status(root: &str, schedule_id: &str, status: &str) {
    update_schedule_state(root, schedule_id, None, status, false);
}

pub fn project_schedule_execution_fact(root: &str, fact: &SubjectExecutionFact) {
    let Some(schedule_id) = fact.schedule_id.as_deref() else {
        return;
    };

    project_schedule_completion_status(root, schedule_id, fact.completion_status());
}

fn update_schedule_state(
    root: &str,
    schedule_id: &str,
    run_at: Option<chrono::DateTime<Utc>>,
    status: &str,
    increment_run_count: bool,
) {
    let project_root = Path::new(root);
    let mut state = load_schedule_state(project_root).unwrap_or_default();
    let entry = state
        .schedules
        .entry(schedule_id.to_string())
        .or_insert_with(ScheduleRunState::default);
    if let Some(run_at) = run_at {
        entry.last_run = Some(run_at);
    }
    if increment_run_count {
        entry.run_count = entry.run_count.saturating_add(1);
    }
    entry.last_status = status.to_string();
    let _ = save_schedule_state(project_root, &state);
}
