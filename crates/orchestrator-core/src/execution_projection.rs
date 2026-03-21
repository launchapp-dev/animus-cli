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
pub const WORKFLOW_RUNNER_CANCELLED_PREFIX: &str = "workflow runner cancelled: ";
pub const WORKFLOW_RUNNER_EXITED_PREFIX: &str = "workflow runner exited without workflow status";
pub const MAX_RUNNER_FAILURE_RESETS: u32 = 3;

/// Returns true when a task is blocked specifically because a workflow runner
/// exited with a non-zero status (a transient infrastructure failure), not
/// because of a dependency gate or human-required input.
pub fn is_workflow_runner_blocked(task: &OrchestratorTask) -> bool {
    if !task.status.is_blocked() || !task.paused {
        return false;
    }
    task.blocked_reason.as_deref().is_some_and(|reason| {
        reason.starts_with(WORKFLOW_RUNNER_BLOCKED_PREFIX)
            || reason.starts_with(WORKFLOW_RUNNER_CANCELLED_PREFIX)
            || reason.starts_with(WORKFLOW_RUNNER_EXITED_PREFIX)
    })
}

/// Resets a runner-blocked task back to `Ready` so the daemon can retry it.
///
/// Uses `consecutive_dispatch_failures` to track how many times this task has
/// been reset.  Once the count reaches `MAX_RUNNER_FAILURE_RESETS` the task is
/// left blocked and an error message is logged, signalling that human
/// intervention is needed.
pub async fn reconcile_runner_blocked_task(hub: Arc<dyn ServiceHub>, task: &OrchestratorTask) -> anyhow::Result<bool> {
    let count = task.consecutive_dispatch_failures.unwrap_or(0).saturating_add(1);

    if count > MAX_RUNNER_FAILURE_RESETS {
        eprintln!(
            "{}: task {} has been reset {} times after runner failures — escalating to human review (blocked_reason={:?})",
            protocol::ACTOR_DAEMON,
            task.id,
            count,
            task.blocked_reason,
        );
        return Ok(false);
    }

    let mut updated = task.clone();
    updated.status = TaskStatus::Ready;
    updated.paused = false;
    updated.blocked_reason = None;
    updated.blocked_at = None;
    updated.blocked_phase = None;
    updated.blocked_by = None;
    updated.consecutive_dispatch_failures = Some(count);
    updated.last_dispatch_failure_at = Some(Utc::now().to_rfc3339());
    updated.metadata.updated_at = Utc::now();
    updated.metadata.updated_by = protocol::ACTOR_DAEMON.to_string();
    updated.metadata.version = updated.metadata.version.saturating_add(1);
    hub.tasks().replace(updated).await?;
    eprintln!(
        "{}: unblocked task {} after runner failure (reset #{}/{}, previous reason: {:?})",
        protocol::ACTOR_DAEMON,
        task.id,
        count,
        MAX_RUNNER_FAILURE_RESETS,
        task.blocked_reason,
    );
    Ok(true)
}

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

    let count = task.consecutive_dispatch_failures.unwrap_or(0).saturating_add(1);

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

pub fn project_schedule_dispatch_attempt(root: &str, schedule_id: &str, run_at: chrono::DateTime<Utc>, status: &str) {
    update_schedule_state(root, schedule_id, Some(run_at), status, true);
}

pub(crate) fn project_schedule_completion_status(root: &str, schedule_id: &str, status: &str) {
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
    let entry = state.schedules.entry(schedule_id.to_string()).or_default();
    if let Some(run_at) = run_at {
        entry.last_run = Some(run_at);
    }
    if increment_run_count {
        entry.run_count = entry.run_count.saturating_add(1);
    }
    entry.last_status = status.to_string();
    let _ = save_schedule_state(project_root, &state);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use protocol::{SubjectExecutionFact, SUBJECT_KIND_TASK};

    use super::{
        execution_fact_subject_kind, is_workflow_runner_blocked, project_execution_fact, reconcile_runner_blocked_task,
        MAX_RUNNER_FAILURE_RESETS,
    };
    use crate::{
        services::ServiceHub, InMemoryServiceHub, OrchestratorTask, Priority, ResourceRequirements, Scope,
        TaskMetadata, TaskStatus, TaskType, WorkflowMetadata,
    };

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
    async fn project_execution_fact_uses_task_projector_for_subject_kind() {
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
        assert_eq!(updated.status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn project_execution_fact_preserves_legacy_task_fact_compatibility() {
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
        assert_eq!(updated.status, TaskStatus::Done);
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

    // --- runner-blocked reconciliation tests ---

    async fn upsert_runner_blocked_task(
        hub: &Arc<InMemoryServiceHub>,
        id: &str,
        blocked_reason: &str,
        dispatch_failures: Option<u32>,
    ) -> OrchestratorTask {
        let now = Utc::now();
        let task = OrchestratorTask {
            id: id.to_string(),
            title: format!("Task {id}"),
            description: "Runner blocked task".to_string(),
            task_type: TaskType::Feature,
            status: TaskStatus::Blocked,
            blocked_reason: Some(blocked_reason.to_string()),
            blocked_at: Some(now),
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
                version: 1,
            },
            deadline: None,
            paused: true,
            cancelled: false,
            resolution: None,
            resource_requirements: ResourceRequirements::default(),
            consecutive_dispatch_failures: dispatch_failures,
            last_dispatch_failure_at: None,
            dispatch_history: Vec::new(),
        };

        hub.tasks().replace(task.clone()).await.expect("upsert task");
        task
    }

    #[test]
    fn is_workflow_runner_blocked_detects_runner_failure() {
        let task = OrchestratorTask {
            status: TaskStatus::Blocked,
            paused: true,
            blocked_reason: Some(
                "workflow runner failed: workflow runner exited unsuccessfully with status Some(1)".to_string(),
            ),
            ..base_test_task("TASK-1")
        };
        assert!(is_workflow_runner_blocked(&task));
    }

    #[test]
    fn is_workflow_runner_blocked_detects_exited_without_status() {
        let task = OrchestratorTask {
            status: TaskStatus::Blocked,
            paused: true,
            blocked_reason: Some(
                "workflow runner exited without workflow status: workflow runner exited with status Some(1)"
                    .to_string(),
            ),
            ..base_test_task("TASK-1")
        };
        assert!(is_workflow_runner_blocked(&task));
    }

    #[test]
    fn is_workflow_runner_blocked_detects_cancelled() {
        let task = OrchestratorTask {
            status: TaskStatus::Blocked,
            paused: true,
            blocked_reason: Some("workflow runner cancelled: operator requested".to_string()),
            ..base_test_task("TASK-1")
        };
        assert!(is_workflow_runner_blocked(&task));
    }

    #[test]
    fn is_workflow_runner_blocked_rejects_non_runner_reasons() {
        let task = OrchestratorTask {
            status: TaskStatus::Blocked,
            paused: true,
            blocked_reason: Some("dependency gate: waiting on TASK-001".to_string()),
            ..base_test_task("TASK-1")
        };
        assert!(!is_workflow_runner_blocked(&task));
    }

    #[test]
    fn is_workflow_runner_blocked_rejects_not_paused() {
        let task = OrchestratorTask {
            status: TaskStatus::Blocked,
            paused: false,
            blocked_reason: Some("workflow runner failed: something".to_string()),
            ..base_test_task("TASK-1")
        };
        assert!(!is_workflow_runner_blocked(&task));
    }

    #[test]
    fn is_workflow_runner_blocked_rejects_not_blocked() {
        let task = OrchestratorTask {
            status: TaskStatus::Ready,
            paused: false,
            blocked_reason: None,
            ..base_test_task("TASK-1")
        };
        assert!(!is_workflow_runner_blocked(&task));
    }

    #[tokio::test]
    async fn reconcile_resets_runner_blocked_task_to_ready() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_runner_blocked_task(
            &hub,
            "TASK-R1",
            "workflow runner failed: workflow runner exited unsuccessfully with status Some(1)",
            None,
        )
        .await;

        let task = hub.tasks().get("TASK-R1").await.unwrap();
        let result = reconcile_runner_blocked_task(hub.clone(), &task).await.unwrap();

        assert!(result);
        let updated = hub.tasks().get("TASK-R1").await.unwrap();
        assert_eq!(updated.status, TaskStatus::Ready);
        assert!(!updated.paused);
        assert!(updated.blocked_reason.is_none());
    }

    #[tokio::test]
    async fn reconcile_increments_and_persists_failure_counter() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_runner_blocked_task(
            &hub,
            "TASK-R3",
            "workflow runner exited without workflow status: workflow runner exited with status Some(1)",
            Some(1),
        )
        .await;

        let task = hub.tasks().get("TASK-R3").await.unwrap();
        let result = reconcile_runner_blocked_task(hub.clone(), &task).await.unwrap();

        assert!(result);
        let updated = hub.tasks().get("TASK-R3").await.unwrap();
        assert_eq!(updated.status, TaskStatus::Ready);
        assert!(!updated.paused);
        assert!(updated.blocked_reason.is_none());
        assert_eq!(updated.consecutive_dispatch_failures, Some(2));
        assert!(updated.last_dispatch_failure_at.is_some());
    }

    #[tokio::test]
    async fn reconcile_stops_resetting_after_max_retries() {
        let hub = Arc::new(InMemoryServiceHub::new());
        upsert_runner_blocked_task(
            &hub,
            "TASK-R2",
            "workflow runner failed: workflow runner exited unsuccessfully with status Some(1)",
            Some(MAX_RUNNER_FAILURE_RESETS),
        )
        .await;

        let task = hub.tasks().get("TASK-R2").await.unwrap();
        let result = reconcile_runner_blocked_task(hub.clone(), &task).await.unwrap();

        assert!(!result);
        let still_blocked = hub.tasks().get("TASK-R2").await.unwrap();
        assert_eq!(still_blocked.status, TaskStatus::Blocked);
    }

    fn base_test_task(id: &str) -> OrchestratorTask {
        let now = Utc::now();
        OrchestratorTask {
            id: id.to_string(),
            title: format!("Task {id}"),
            description: String::new(),
            task_type: TaskType::Feature,
            status: TaskStatus::Backlog,
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
}
