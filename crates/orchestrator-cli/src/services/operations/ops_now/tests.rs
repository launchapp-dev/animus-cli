use super::*;
use orchestrator_core::{
    Assignee, ChecklistItem, Complexity, ImpactArea, Priority, RequirementItem, RequirementPriority,
    ResourceRequirements, RiskLevel, Scope, TaskDependency, TaskMetadata, TaskType, WorkflowMetadata,
    WorkflowPhaseStatus, WorkflowStatus,
};
use chrono::{Duration, Utc};

fn make_task(id: &str, title: &str, status: TaskStatus) -> OrchestratorTask {
    let now = Utc::now();
    OrchestratorTask {
        id: id.to_string(),
        title: title.to_string(),
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
        impact_area: Vec::<ImpactArea>::new(),
        assignee: Assignee::Unassigned,
        estimated_effort: None,
        linked_requirements: Vec::new(),
        linked_architecture_entities: Vec::new(),
        dependencies: Vec::<TaskDependency>::new(),
        checklist: Vec::<ChecklistItem>::new(),
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

fn make_requirement(id: &str, title: &str) -> RequirementItem {
    RequirementItem {
        id: id.to_string(),
        title: title.to_string(),
        description: String::new(),
        priority: RequirementPriority::Must,
        status: "draft".to_string(),
        category: None,
        tags: vec![],
    }
}

fn make_workflow(id: &str, task_id: &str, status: WorkflowStatus) -> OrchestratorWorkflow {
    use orchestrator_core::{SubjectRef, WorkflowMachineState, WorkflowPhaseExecution};

    OrchestratorWorkflow {
        id: id.to_string(),
        task_id: task_id.to_string(),
        workflow_ref: Some("test".to_string()),
        subject: SubjectRef::task(task_id),
        input: None,
        vars: Default::default(),
        status,
        current_phase_index: 0,
        phases: vec![WorkflowPhaseExecution {
            phase_id: "implementation".to_string(),
            status: WorkflowPhaseStatus::Running,
            started_at: Some(Utc::now()),
            completed_at: None,
            attempt: 1,
            error_message: None,
        }],
        machine_state: WorkflowMachineState::default(),
        current_phase: Some("implementation".to_string()),
        started_at: Utc::now(),
        completed_at: None,
        failure_reason: None,
    }
}

#[test]
fn test_build_now_surface_with_next_task() {
    let next_task = make_task("TASK-001", "Test task", TaskStatus::Ready);
    let all_tasks = vec![next_task.clone()];
    let all_workflows = vec![];
    let all_requirements = vec![];

    let surface = build_now_surface(Some(next_task), &all_tasks, &all_workflows, &all_requirements);

    assert!(surface.next_task.is_some());
    assert_eq!(surface.next_task.as_ref().unwrap().id, "TASK-001");
    assert_eq!(surface.next_task.as_ref().unwrap().title, "Test task");
}

#[test]
fn test_build_now_surface_without_next_task() {
    let all_tasks = vec![];
    let all_workflows = vec![];
    let all_requirements = vec![];

    let surface = build_now_surface(None, &all_tasks, &all_workflows, &all_requirements);

    assert!(surface.next_task.is_none());
}

#[test]
fn test_next_task_with_linked_requirements() {
    let mut next_task = make_task("TASK-001", "Feature task", TaskStatus::Ready);
    next_task.linked_requirements = vec!["REQ-001".to_string(), "REQ-002".to_string()];

    let all_tasks = vec![next_task.clone()];
    let all_requirements = vec![
        make_requirement("REQ-001", "Requirement 1"),
        make_requirement("REQ-002", "Requirement 2"),
    ];
    let all_workflows = vec![];

    let surface = build_now_surface(Some(next_task), &all_tasks, &all_workflows, &all_requirements);

    assert!(surface.next_task.is_some());
    let task_item = surface.next_task.as_ref().unwrap();
    assert_eq!(task_item.linked_requirements.len(), 2);
    assert_eq!(task_item.linked_requirements[0].id, "REQ-001");
    assert_eq!(task_item.linked_requirements[1].id, "REQ-002");
}

#[test]
fn test_blocked_items_filtering() {
    let blocked_task = {
        let mut task = make_task("TASK-001", "Blocked task", TaskStatus::Blocked);
        task.blocked_reason = Some("Waiting for review".to_string());
        task
    };
    let ready_task = make_task("TASK-002", "Ready task", TaskStatus::Ready);

    let all_tasks = vec![blocked_task, ready_task];
    let all_workflows = vec![];
    let all_requirements = vec![];

    let surface = build_now_surface(None, &all_tasks, &all_workflows, &all_requirements);

    assert_eq!(surface.blocked_items.len(), 1);
    assert_eq!(surface.blocked_items[0].id, "TASK-001");
}

#[test]
fn test_multiple_blocked_items() {
    let blocked_task1 = {
        let mut task = make_task("TASK-001", "Blocked 1", TaskStatus::Blocked);
        task.blocked_reason = Some("Waiting for review".to_string());
        task
    };
    let blocked_task2 = {
        let mut task = make_task("TASK-002", "Blocked 2", TaskStatus::Blocked);
        task.blocked_reason = Some("Dependency not ready".to_string());
        task
    };
    let ready_task = make_task("TASK-003", "Ready task", TaskStatus::Ready);

    let all_tasks = vec![blocked_task1, blocked_task2, ready_task];
    let all_workflows = vec![];
    let all_requirements = vec![];

    let surface = build_now_surface(None, &all_tasks, &all_workflows, &all_requirements);

    assert_eq!(surface.blocked_items.len(), 2);
    assert!(surface.blocked_items.iter().any(|item| item.id == "TASK-001"));
    assert!(surface.blocked_items.iter().any(|item| item.id == "TASK-002"));
}

#[test]
fn test_active_workflows_filtering() {
    let task1 = make_task("TASK-001", "Task 1", TaskStatus::InProgress);
    let task2 = make_task("TASK-002", "Task 2", TaskStatus::Ready);

    let running_workflow = make_workflow("WF-001", "TASK-001", WorkflowStatus::Running);
    let completed_workflow = make_workflow("WF-002", "TASK-002", WorkflowStatus::Completed);

    let all_tasks = vec![task1, task2];
    let all_workflows = vec![running_workflow, completed_workflow];
    let all_requirements = vec![];

    let surface = build_now_surface(None, &all_tasks, &all_workflows, &all_requirements);

    assert_eq!(surface.active_workflows.len(), 1);
    assert_eq!(surface.active_workflows[0].id, "WF-001");
    assert_eq!(surface.active_workflows[0].task_id, "TASK-001");
    assert_eq!(surface.active_workflows[0].task_title, "Task 1");
}

#[test]
fn test_stale_items_detection() {
    let now = Utc::now();
    let old_update_time = now - Duration::days(10);

    let stale_task = {
        let mut task = make_task("TASK-001", "Stale task", TaskStatus::InProgress);
        task.metadata.updated_at = old_update_time;
        task
    };

    let recent_task = make_task("TASK-002", "Recent task", TaskStatus::InProgress);

    let all_tasks = vec![stale_task, recent_task];
    let all_workflows = vec![];
    let all_requirements = vec![];

    let surface = build_now_surface(None, &all_tasks, &all_workflows, &all_requirements);

    assert_eq!(surface.stale_items.len(), 1);
    assert_eq!(surface.stale_items[0].id, "TASK-001");
    assert!(surface.stale_items[0].days_stale >= 10);
}

#[test]
fn test_stale_items_exclude_recent_tasks() {
    let recent_task1 = make_task("TASK-001", "Recent 1", TaskStatus::InProgress);
    let recent_task2 = make_task("TASK-002", "Recent 2", TaskStatus::InProgress);

    let all_tasks = vec![recent_task1, recent_task2];
    let all_workflows = vec![];
    let all_requirements = vec![];

    let surface = build_now_surface(None, &all_tasks, &all_workflows, &all_requirements);

    assert_eq!(surface.stale_items.len(), 0);
}

#[test]
fn test_stale_items_exclude_non_in_progress_tasks() {
    let now = Utc::now();
    let old_update_time = now - Duration::days(10);

    let stale_blocked = {
        let mut task = make_task("TASK-001", "Stale blocked", TaskStatus::Blocked);
        task.metadata.updated_at = old_update_time;
        task
    };

    let stale_done = {
        let mut task = make_task("TASK-002", "Stale done", TaskStatus::Done);
        task.metadata.updated_at = old_update_time;
        task
    };

    let all_tasks = vec![stale_blocked, stale_done];
    let all_workflows = vec![];
    let all_requirements = vec![];

    let surface = build_now_surface(None, &all_tasks, &all_workflows, &all_requirements);

    assert_eq!(surface.stale_items.len(), 0);
}
