use super::*;
use chrono::{Duration, Utc};

fn next_task(id: &str, title: &str) -> NextTaskItem {
    NextTaskItem {
        id: id.to_string(),
        title: title.to_string(),
        priority: "Medium".to_string(),
        status: "Ready".to_string(),
        linked_requirements: Vec::new(),
    }
}

fn active_workflow(id: &str, task_id: &str, task_title: &str) -> ActiveWorkflowItem {
    ActiveWorkflowItem {
        id: id.to_string(),
        task_id: task_id.to_string(),
        task_title: task_title.to_string(),
        status: "Running".to_string(),
        current_phase: Some("implementation".to_string()),
    }
}

fn blocked_item(id: &str, title: &str) -> BlockedItem {
    BlockedItem {
        id: id.to_string(),
        item_type: "task".to_string(),
        title: title.to_string(),
        blocked_reason: Some("Waiting for review".to_string()),
        blocked_at: None,
    }
}

fn stale_item(id: &str, title: &str, days_stale: u32) -> StaleItem {
    StaleItem {
        id: id.to_string(),
        item_type: "task".to_string(),
        title: title.to_string(),
        last_updated: Utc::now() - Duration::days(i64::from(days_stale)),
        days_stale,
    }
}

#[test]
fn test_build_now_surface_with_next_task() {
    let surface = build_now_surface(Utc::now(), Some(next_task("TASK-001", "Test task")), vec![], vec![], vec![]);

    assert!(surface.next_task.is_some());
    assert_eq!(surface.next_task.as_ref().unwrap().id, "TASK-001");
    assert_eq!(surface.next_task.as_ref().unwrap().title, "Test task");
}

#[test]
fn test_build_now_surface_without_next_task() {
    let surface = build_now_surface(Utc::now(), None, vec![], vec![], vec![]);

    assert!(surface.next_task.is_none());
}

#[test]
fn test_next_task_with_linked_requirements() {
    let mut next = next_task("TASK-001", "Feature task");
    next.linked_requirements = vec![
        LinkedRequirement {
            id: "REQ-001".to_string(),
            title: "Requirement 1".to_string(),
            priority: "Must".to_string(),
        },
        LinkedRequirement {
            id: "REQ-002".to_string(),
            title: "Requirement 2".to_string(),
            priority: "Should".to_string(),
        },
    ];

    let surface = build_now_surface(Utc::now(), Some(next), vec![], vec![], vec![]);

    assert!(surface.next_task.is_some());
    let task_item = surface.next_task.as_ref().unwrap();
    assert_eq!(task_item.linked_requirements.len(), 2);
    assert_eq!(task_item.linked_requirements[0].id, "REQ-001");
    assert_eq!(task_item.linked_requirements[1].id, "REQ-002");
}

#[test]
fn test_blocked_items_filtering() {
    let surface = build_now_surface(Utc::now(), None, vec![], vec![blocked_item("TASK-001", "Blocked task")], vec![]);

    assert_eq!(surface.blocked_items.len(), 1);
    assert_eq!(surface.blocked_items[0].id, "TASK-001");
}

#[test]
fn test_multiple_blocked_items() {
    let surface = build_now_surface(
        Utc::now(),
        None,
        vec![],
        vec![blocked_item("TASK-001", "Blocked 1"), blocked_item("TASK-002", "Blocked 2")],
        vec![],
    );

    assert_eq!(surface.blocked_items.len(), 2);
    assert!(surface.blocked_items.iter().any(|item| item.id == "TASK-001"));
    assert!(surface.blocked_items.iter().any(|item| item.id == "TASK-002"));
}

#[test]
fn test_active_workflows_filtering() {
    let surface =
        build_now_surface(Utc::now(), None, vec![active_workflow("WF-001", "TASK-001", "Task 1")], vec![], vec![]);

    assert_eq!(surface.active_workflows.len(), 1);
    assert_eq!(surface.active_workflows[0].id, "WF-001");
    assert_eq!(surface.active_workflows[0].task_id, "TASK-001");
    assert_eq!(surface.active_workflows[0].task_title, "Task 1");
}

#[test]
fn test_stale_items_detection() {
    let surface = build_now_surface(Utc::now(), None, vec![], vec![], vec![stale_item("TASK-001", "Stale task", 10)]);

    assert_eq!(surface.stale_items.len(), 1);
    assert_eq!(surface.stale_items[0].id, "TASK-001");
    assert!(surface.stale_items[0].days_stale >= 10);
}

#[test]
fn test_stale_items_exclude_recent_tasks() {
    let surface = build_now_surface(Utc::now(), None, vec![], vec![], vec![]);

    assert_eq!(surface.stale_items.len(), 0);
}

#[test]
fn test_stale_items_exclude_non_in_progress_tasks() {
    let surface = build_now_surface(Utc::now(), None, vec![], vec![blocked_item("TASK-001", "Blocked task")], vec![]);

    assert_eq!(surface.stale_items.len(), 0);
}
