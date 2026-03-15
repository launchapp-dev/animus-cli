use std::sync::Arc;

use anyhow::Result;

use crate::{services::ServiceHub, OrchestratorTask, TaskStatus, WorkflowStatus};

#[derive(Debug, Clone)]
pub struct DaemonTickMetrics {
    pub tasks_total: usize,
    pub tasks_ready: usize,
    pub tasks_in_progress: usize,
    pub tasks_blocked: usize,
    pub tasks_done: usize,
    pub stale_in_progress_count: usize,
    pub stale_in_progress_task_ids: Vec<String>,
    pub workflows_running: usize,
    pub workflows_completed: usize,
    pub workflows_failed: usize,
}

impl DaemonTickMetrics {
    pub async fn collect(hub: Arc<dyn ServiceHub>, stale_threshold_hours: u64) -> Result<Self> {
        let tasks = hub.tasks().list().await?;
        let workflows = hub.workflows().list().await.unwrap_or_default();

        let tasks_total = tasks.len();
        let tasks_ready =
            tasks.iter().filter(|task| matches!(task.status, TaskStatus::Ready | TaskStatus::Backlog)).count();
        let tasks_in_progress = tasks.iter().filter(|task| task.status == TaskStatus::InProgress).count();
        let tasks_blocked = tasks.iter().filter(|task| task.status.is_blocked()).count();
        let tasks_done = tasks.iter().filter(|task| task.status.is_terminal()).count();
        let stale_in_progress = stale_in_progress_summary(&tasks, stale_threshold_hours, chrono::Utc::now());

        let workflows_running = workflows
            .iter()
            .filter(|workflow| matches!(workflow.status, WorkflowStatus::Running | WorkflowStatus::Paused))
            .count();
        let workflows_completed =
            workflows.iter().filter(|workflow| is_terminally_completed_workflow(workflow)).count();
        let workflows_failed = workflows.iter().filter(|workflow| workflow.status == WorkflowStatus::Failed).count();

        Ok(Self {
            tasks_total,
            tasks_ready,
            tasks_in_progress,
            tasks_blocked,
            tasks_done,
            stale_in_progress_count: stale_in_progress.count,
            stale_in_progress_task_ids: stale_in_progress.task_ids(),
            workflows_running,
            workflows_completed,
            workflows_failed,
        })
    }
}

#[derive(Debug, Clone)]
struct StaleInProgressSummary {
    count: usize,
    tasks: Vec<StaleInProgressEntry>,
}

impl StaleInProgressSummary {
    fn task_ids(&self) -> Vec<String> {
        self.tasks.iter().map(|entry| entry.task_id.clone()).collect()
    }
}

#[derive(Debug, Clone)]
struct StaleInProgressEntry {
    task_id: String,
}

fn stale_in_progress_summary(
    tasks: &[OrchestratorTask],
    threshold_hours: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> StaleInProgressSummary {
    let threshold_seconds = threshold_hours.saturating_mul(3600);
    let mut stale_tasks: Vec<&OrchestratorTask> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::InProgress)
        .filter(|task| task_age_seconds(now, task.metadata.updated_at) >= threshold_seconds)
        .collect();

    stale_tasks.sort_by(|a, b| a.metadata.updated_at.cmp(&b.metadata.updated_at).then(a.id.cmp(&b.id)));

    let stale_entries: Vec<StaleInProgressEntry> =
        stale_tasks.into_iter().map(|task| StaleInProgressEntry { task_id: task.id.clone() }).collect();

    StaleInProgressSummary { count: stale_entries.len(), tasks: stale_entries }
}

fn task_age_seconds(now: chrono::DateTime<chrono::Utc>, updated_at: chrono::DateTime<chrono::Utc>) -> u64 {
    now.signed_duration_since(updated_at).num_seconds().max(0) as u64
}

fn is_terminally_completed_workflow(workflow: &crate::OrchestratorWorkflow) -> bool {
    workflow.status == WorkflowStatus::Completed
        && workflow.machine_state == crate::WorkflowMachineState::Completed
        && workflow.completed_at.is_some()
}
