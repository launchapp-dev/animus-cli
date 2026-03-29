use serde::Serialize;

#[cfg(test)]
use chrono::{DateTime, Utc};
#[cfg(test)]
use orchestrator_core::{OrchestratorTask, TaskStatus};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct StaleInProgressEntry {
    pub(crate) task_id: String,
    pub(crate) title: String,
    pub(crate) updated_at: String,
    pub(crate) age_hours: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct StaleInProgressSummary {
    pub(crate) threshold_hours: u64,
    pub(crate) count: usize,
    pub(crate) tasks: Vec<StaleInProgressEntry>,
}

#[cfg(test)]
impl StaleInProgressSummary {
    pub(crate) fn task_ids(&self) -> Vec<String> {
        self.tasks.iter().map(|entry| entry.task_id.clone()).collect()
    }
}

#[cfg(test)]
pub(crate) fn stale_in_progress_summary(
    tasks: &[OrchestratorTask],
    threshold_hours: u64,
    now: DateTime<Utc>,
) -> StaleInProgressSummary {
    let threshold_seconds = threshold_hours.saturating_mul(3600);
    let mut stale_tasks: Vec<&OrchestratorTask> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::InProgress)
        .filter(|task| task_age_seconds(now, task.metadata.updated_at) >= threshold_seconds)
        .collect();

    stale_tasks.sort_by(|a, b| a.metadata.updated_at.cmp(&b.metadata.updated_at).then(a.id.cmp(&b.id)));

    let stale_entries: Vec<StaleInProgressEntry> = stale_tasks
        .into_iter()
        .map(|task| {
            let age_seconds = task_age_seconds(now, task.metadata.updated_at);
            StaleInProgressEntry {
                task_id: task.id.clone(),
                title: task.title.clone(),
                updated_at: task.metadata.updated_at.to_rfc3339(),
                age_hours: age_seconds / 3600,
            }
        })
        .collect();

    StaleInProgressSummary { threshold_hours, count: stale_entries.len(), tasks: stale_entries }
}

#[cfg(test)]
fn task_age_seconds(now: DateTime<Utc>, updated_at: DateTime<Utc>) -> u64 {
    now.signed_duration_since(updated_at).num_seconds().max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use orchestrator_core::{
        Assignee, Complexity, OrchestratorTask, Priority, ResourceRequirements, RiskLevel, Scope, TaskMetadata,
        TaskType, WorkflowMetadata,
    };

    fn sample_task(id: &str, status: TaskStatus, updated_at: DateTime<Utc>) -> OrchestratorTask {
        OrchestratorTask {
            id: id.to_string(),
            title: format!("Task {id}"),
            description: String::new(),
            task_type: TaskType::Feature,
            status,
            blocked_reason: None,
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            priority: Priority::Medium,
            risk: RiskLevel::Medium,
            scope: Scope::Medium,
            complexity: Complexity::Medium,
            impact_area: Vec::new(),
            assignee: Assignee::Unassigned,
            estimated_effort: None,
            linked_requirements: Vec::new(),
            linked_architecture_entities: Vec::new(),
            dependencies: Vec::new(),
            checklist: Vec::new(),
            tags: Vec::new(),
            workflow_metadata: WorkflowMetadata::default(),
            worktree_path: None,
            branch_name: None,
            metadata: TaskMetadata {
                created_at: updated_at,
                updated_at,
                created_by: "test".to_string(),
                updated_by: "test".to_string(),
                started_at: None,
                completed_at: None,
                version: 1,
            },
            deadline: None,
            paused: false,
            cancelled: false,
            resolution: None,
            resource_requirements: ResourceRequirements::default(),
            consecutive_dispatch_failures: None,
            last_dispatch_failure_at: None,
            dispatch_history: Vec::new(),
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z").expect("valid fixed timestamp").with_timezone(&Utc)
    }

    #[test]
    fn stale_detector_includes_exact_threshold_boundary() {
        let now = fixed_now();
        let tasks = vec![sample_task("TASK-001", TaskStatus::InProgress, now - Duration::hours(24))];

        let summary = stale_in_progress_summary(&tasks, 24, now);

        assert_eq!(summary.count, 1);
        assert_eq!(summary.tasks[0].task_id, "TASK-001");
        assert_eq!(summary.tasks[0].age_hours, 24);
    }

    #[test]
    fn stale_detector_excludes_non_in_progress_tasks() {
        let now = fixed_now();
        let tasks = vec![
            sample_task("TASK-001", TaskStatus::Ready, now - Duration::hours(48)),
            sample_task("TASK-002", TaskStatus::Blocked, now - Duration::hours(48)),
        ];

        let summary = stale_in_progress_summary(&tasks, 24, now);

        assert_eq!(summary.count, 0);
        assert!(summary.tasks.is_empty());
    }

    #[test]
    fn stale_detector_excludes_tasks_with_future_updated_at() {
        let now = fixed_now();
        let tasks = vec![sample_task("TASK-001", TaskStatus::InProgress, now + Duration::hours(6))];

        let summary = stale_in_progress_summary(&tasks, 24, now);

        assert_eq!(summary.count, 0);
        assert!(summary.tasks.is_empty());
    }

    #[test]
    fn stale_detector_sorts_by_updated_at_then_task_id() {
        let now = fixed_now();
        let tied_timestamp = now - Duration::hours(30);
        let tasks = vec![
            sample_task("TASK-003", TaskStatus::InProgress, tied_timestamp),
            sample_task("TASK-001", TaskStatus::InProgress, tied_timestamp),
            sample_task("TASK-002", TaskStatus::InProgress, now - Duration::hours(36)),
        ];

        let summary = stale_in_progress_summary(&tasks, 24, now);

        assert_eq!(summary.task_ids(), vec!["TASK-002".to_string(), "TASK-001".to_string(), "TASK-003".to_string()]);
    }
}
