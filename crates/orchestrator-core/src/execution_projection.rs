mod project_requirement_workflow_status;
mod project_task_terminal_workflow_status;
mod projector_registry;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use protocol::SubjectExecutionFact;

use crate::{
    load_schedule_state, save_schedule_state, services::ServiceHub, OrchestratorTask, TaskStatus, WorkflowStatus,
};

pub use project_requirement_workflow_status::project_requirement_workflow_status;
pub use project_task_terminal_workflow_status::project_task_terminal_workflow_status;
pub use projector_registry::{
    builtin_execution_projector_registry, execution_fact_subject_kind, ExecutionProjector, ExecutionProjectorRegistry,
};

pub const WORKFLOW_RUNNER_BLOCKED_PREFIX: &str = "workflow runner failed: ";

pub async fn project_task_status(hub: Arc<dyn ServiceHub>, task_id: &str, status: TaskStatus) -> Result<()> {
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

pub async fn project_task_workflow_start(
    hub: Arc<dyn ServiceHub>,
    task_id: &str,
    role: String,
    model: Option<String>,
    updated_by: String,
) -> Result<()> {
    hub.tasks().set_status(task_id, TaskStatus::InProgress, false).await?;
    hub.tasks().assign_agent(task_id, role, model, updated_by).await?;
    Ok(())
}

pub async fn project_task_execution_fact(hub: Arc<dyn ServiceHub>, _root: &str, fact: &SubjectExecutionFact) {
    let Some(task_id) = fact.task_id.as_deref() else {
        return;
    };

    if let Some(status) = fact.workflow_status {
        match status {
            WorkflowStatus::Pending | WorkflowStatus::Running | WorkflowStatus::Paused => return,
            WorkflowStatus::Completed => {
                // Workflow completion does not auto-mark the task done.
                // Only an agent or human should mark a task done after verifying
                // that the work actually landed (e.g. PR merged).
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
        // Successful execution does not auto-mark the task done.
        // Only an agent or human should mark a task done after verification.
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

pub async fn project_execution_fact(hub: Arc<dyn ServiceHub>, root: &str, fact: &SubjectExecutionFact) -> bool {
    match builtin_execution_projector_registry().project(hub.clone(), root, fact).await {
        Ok(projected) => projected,
        Err(err) => {
            let kind = execution_fact_subject_kind(fact).unwrap_or("unknown");
            eprintln!(
                "{}: failed to project execution fact for subject '{}' (kind='{}'): {}",
                protocol::ACTOR_DAEMON,
                fact.subject_id,
                kind,
                err
            );
            true
        }
    }
}

/// Record a SUCCESSFUL schedule dispatch attempt.
///
/// Updates `last_run`, increments `run_count`, and stores `status`. Call
/// this only when the workflow runner actually spawned (i.e.
/// `ProcessManager::spawn_workflow_runner` returned `Ok`). Recording
/// `last_run` for failed-capacity dispatches would suppress the schedule
/// on the next tick (cron dedup compares last_run minute) — the missed
/// cron minute would never be retried.
pub fn project_schedule_dispatch_attempt(root: &str, schedule_id: &str, run_at: chrono::DateTime<Utc>, status: &str) {
    update_schedule_state(root, schedule_id, Some(run_at), status, true, false);
}

/// Record a MISSED schedule dispatch attempt (e.g. pool at capacity).
///
/// Leaves `last_run` untouched so the schedule re-fires on the next tick.
/// Increments `missed_count` for ops visibility and stores `status` (which
/// typically captures the rejection reason). Separate from
/// `project_schedule_dispatch_attempt` so the schedule state file
/// distinguishes "ran" from "skipped: pool full".
pub fn project_schedule_dispatch_missed(root: &str, schedule_id: &str, status: &str) {
    update_schedule_state(root, schedule_id, None, status, false, true);
}

pub(crate) fn project_schedule_completion_status(root: &str, schedule_id: &str, status: &str) {
    update_schedule_state(root, schedule_id, None, status, false, false);
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
    increment_missed_count: bool,
) {
    let project_root = Path::new(root);
    let mut state = load_schedule_state(project_root).unwrap_or_default();
    let entry = state.schedules.entry(schedule_id.to_string()).or_default();
    if let Some(run_at) = run_at {
        entry.last_run = Some(run_at);
    }
    if increment_run_count {
        entry.run_count = entry.run_count.saturating_add(1);
    }
    if increment_missed_count {
        entry.missed_count = entry.missed_count.saturating_add(1);
    }
    entry.last_status = status.to_string();
    let _ = save_schedule_state(project_root, &state);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use protocol::{SubjectExecutionFact, SUBJECT_KIND_TASK};

    use super::{execution_fact_subject_kind, project_execution_fact};
    use crate::{
        services::ServiceHub, InMemoryServiceHub, OrchestratorTask, Priority, ResourceRequirements, Scope,
        TaskMetadata, TaskStatus, TaskType, WorkflowMetadata,
    };

    #[test]
    fn project_schedule_dispatch_missed_does_not_update_last_run() {
        // Regression for the audit P2 finding: a schedule that was due but
        // could not be dispatched (e.g. pool at capacity) must NOT have its
        // `last_run` field updated, otherwise the cron dedup logic in
        // `evaluate_schedules` would suppress it on the next tick — the
        // missed cron minute would silently never re-fire.
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_string_lossy().to_string();

        super::project_schedule_dispatch_missed(&root, "nightly", "failed: tick budget exhausted");

        let state = super::load_schedule_state(temp.path()).expect("load state");
        let entry = state.schedules.get("nightly").expect("schedule entry");
        assert!(entry.last_run.is_none(), "missed dispatch must not touch last_run");
        assert_eq!(entry.run_count, 0, "missed dispatch must not increment run_count");
        assert_eq!(entry.missed_count, 1, "missed_count tracks pool-rejected dispatches");
        assert_eq!(entry.last_status, "failed: tick budget exhausted");
    }

    #[test]
    fn project_schedule_dispatch_attempt_updates_last_run_and_run_count() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_string_lossy().to_string();
        let run_at: chrono::DateTime<Utc> = "2026-04-01T12:30:00Z".parse().unwrap();

        super::project_schedule_dispatch_attempt(&root, "nightly", run_at, "dispatched");

        let state = super::load_schedule_state(temp.path()).expect("load state");
        let entry = state.schedules.get("nightly").expect("entry");
        assert_eq!(entry.last_run, Some(run_at));
        assert_eq!(entry.run_count, 1);
        assert_eq!(entry.missed_count, 0, "successful dispatch must NOT increment missed_count");
        assert_eq!(entry.last_status, "dispatched");
    }

    #[test]
    fn project_schedule_dispatch_missed_then_attempt_separates_counters() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_string_lossy().to_string();
        let run_at: chrono::DateTime<Utc> = "2026-04-01T12:30:00Z".parse().unwrap();

        super::project_schedule_dispatch_missed(&root, "hourly", "failed: pool full");
        super::project_schedule_dispatch_missed(&root, "hourly", "failed: pool full");
        super::project_schedule_dispatch_attempt(&root, "hourly", run_at, "dispatched");

        let state = super::load_schedule_state(temp.path()).expect("load state");
        let entry = state.schedules.get("hourly").expect("entry");
        assert_eq!(entry.run_count, 1, "only the successful attempt counts as a run");
        assert_eq!(entry.missed_count, 2, "both failed attempts counted as missed");
        assert_eq!(entry.last_run, Some(run_at));
    }

    async fn upsert_task(hub: &Arc<InMemoryServiceHub>, id: &str, status: TaskStatus) -> OrchestratorTask {
        let now = Utc::now();
        let task = OrchestratorTask {
            id: id.to_string(),
            title: format!("Task {id}"),
            description: "Execution projection".to_string(),
            task_type: TaskType::Feature,
            status,
            blocked_reason: None,
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            priority: Priority::Medium,
            risk: crate::RiskLevel::Medium,
            scope: Scope::Medium,
            complexity: crate::Complexity::default(),
            impact_area: Vec::new(),
            assignee: crate::Assignee::Unassigned,
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
                created_at: now,
                updated_at: now,
                created_by: "test".to_string(),
                updated_by: "test".to_string(),
                started_at: None,
                completed_at: None,
                status_changed_at: None,
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
        };

        hub.tasks().replace(task.clone()).await.expect("upsert task");
        task
    }

    #[tokio::test]
    async fn project_execution_fact_does_not_auto_mark_task_done_on_success() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_task(&hub, "TASK-1", TaskStatus::Ready).await;

        let fact = SubjectExecutionFact {
            subject_id: "TASK-1".to_string(),
            subject_kind: Some(SUBJECT_KIND_TASK.to_string()),
            task_id: Some("TASK-1".to_string()),
            workflow_id: None,
            workflow_ref: None,
            workflow_status: None,
            schedule_id: None,
            exit_code: Some(0),
            success: true,
            failure_reason: None,
            runner_events: Vec::new(),
        };

        let projected = project_execution_fact(hub.clone(), ".", &fact).await;

        assert!(projected);
        let updated = hub.tasks().get("TASK-1").await.expect("task should exist");
        assert_eq!(updated.status, TaskStatus::Ready, "task should NOT be auto-marked done");
    }

    #[tokio::test]
    async fn project_execution_fact_does_not_auto_mark_done_for_legacy_facts() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_task(&hub, "TASK-2", TaskStatus::Ready).await;

        let fact = SubjectExecutionFact {
            subject_id: "TASK-2".to_string(),
            subject_kind: None,
            task_id: Some("TASK-2".to_string()),
            workflow_id: None,
            workflow_ref: None,
            workflow_status: None,
            schedule_id: None,
            exit_code: Some(0),
            success: true,
            failure_reason: None,
            runner_events: Vec::new(),
        };

        let projected = project_execution_fact(hub.clone(), ".", &fact).await;

        assert!(projected);
        assert_eq!(execution_fact_subject_kind(&fact), Some(SUBJECT_KIND_TASK));
        let updated = hub.tasks().get("TASK-2").await.expect("task should exist");
        assert_eq!(updated.status, TaskStatus::Ready, "task should NOT be auto-marked done");
    }

    #[tokio::test]
    async fn project_execution_fact_reports_unknown_subject_kind_as_unprojected() {
        let hub = Arc::new(InMemoryServiceHub::new());
        let fact = SubjectExecutionFact {
            subject_id: "REV-1".to_string(),
            subject_kind: Some("pack.review".to_string()),
            task_id: None,
            workflow_id: None,
            workflow_ref: None,
            workflow_status: None,
            schedule_id: None,
            exit_code: Some(0),
            success: true,
            failure_reason: None,
            runner_events: Vec::new(),
        };

        let projected = project_execution_fact(hub, ".", &fact).await;

        assert!(!projected);
    }
}
