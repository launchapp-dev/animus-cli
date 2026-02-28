use orchestrator_core::{Assignee, OrchestratorTask, TaskStatus};

#[derive(Debug, Clone)]
pub(crate) struct TaskSnapshot {
    pub(crate) id: String,
    pub(crate) status: TaskStatus,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) assignee_label: String,
}

impl TaskSnapshot {
    pub(crate) fn from_task(task: OrchestratorTask) -> Self {
        let assignee_label = match &task.assignee {
            Assignee::Agent { role, model } => {
                if let Some(m) = model {
                    format!("agent:{role}/{m}")
                } else {
                    format!("agent:{role}")
                }
            }
            Assignee::Human { user_id } => user_id.clone(),
            Assignee::Unassigned => String::new(),
        };
        Self {
            id: task.id,
            status: task.status,
            title: task.title,
            description: task.description,
            assignee_label,
        }
    }

    pub(crate) fn status_label(&self) -> &'static str {
        status_label(self.status)
    }

    pub(crate) fn label(&self) -> String {
        format!("{} [{}] {}", self.id, self.status_label(), self.title)
    }
}

pub(crate) fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Backlog => "backlog",
        TaskStatus::Ready => "ready",
        TaskStatus::InProgress => "in-progress",
        TaskStatus::Blocked => "blocked",
        TaskStatus::OnHold => "on-hold",
        TaskStatus::Done => "done",
        TaskStatus::Cancelled => "cancelled",
    }
}

pub(crate) const STATUS_CYCLE: &[TaskStatus] = &[
    TaskStatus::Backlog,
    TaskStatus::Ready,
    TaskStatus::InProgress,
    TaskStatus::OnHold,
    TaskStatus::Done,
    TaskStatus::Cancelled,
];

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{ResourceRequirements, TaskMetadata, WorkflowMetadata};

    fn make_task(id: &str, status: TaskStatus, title: &str) -> OrchestratorTask {
        let now = chrono::Utc::now();
        OrchestratorTask {
            id: id.to_string(),
            status,
            title: title.to_string(),
            description: "Test description".to_string(),
            task_type: orchestrator_core::TaskType::Feature,
            priority: orchestrator_core::Priority::Medium,
            assignee: Assignee::Unassigned,
            blocked_reason: None,
            blocked_at: None,
            blocked_phase: None,
            blocked_by: None,
            risk: orchestrator_core::RiskLevel::default(),
            scope: orchestrator_core::Scope::default(),
            complexity: orchestrator_core::Complexity::default(),
            impact_area: vec![],
            estimated_effort: None,
            linked_requirements: vec![],
            linked_architecture_entities: vec![],
            dependencies: vec![],
            checklist: vec![],
            tags: vec![],
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
            resource_requirements: ResourceRequirements::default(),
        }
    }

    #[test]
    fn from_task_extracts_fields_correctly() {
        let task = make_task("TASK-001", TaskStatus::InProgress, "Test Title");
        let snapshot = TaskSnapshot::from_task(task);
        assert_eq!(snapshot.id, "TASK-001");
        assert_eq!(snapshot.status, TaskStatus::InProgress);
        assert_eq!(snapshot.title, "Test Title");
    }

    #[test]
    fn label_formats_correctly() {
        let snapshot = TaskSnapshot {
            id: "TASK-042".to_string(),
            status: TaskStatus::Ready,
            title: "Implement feature X".to_string(),
            description: String::new(),
            assignee_label: String::new(),
        };
        assert_eq!(snapshot.label(), "TASK-042 [ready] Implement feature X");
    }

    #[test]
    fn status_label_returns_correct_strings() {
        assert_eq!(status_label(TaskStatus::Backlog), "backlog");
        assert_eq!(status_label(TaskStatus::Ready), "ready");
        assert_eq!(status_label(TaskStatus::InProgress), "in-progress");
        assert_eq!(status_label(TaskStatus::Blocked), "blocked");
        assert_eq!(status_label(TaskStatus::OnHold), "on-hold");
        assert_eq!(status_label(TaskStatus::Done), "done");
        assert_eq!(status_label(TaskStatus::Cancelled), "cancelled");
    }

    #[test]
    fn status_cycle_contains_all_expected_statuses() {
        assert_eq!(STATUS_CYCLE.len(), 6);
        assert!(STATUS_CYCLE.contains(&TaskStatus::Backlog));
        assert!(STATUS_CYCLE.contains(&TaskStatus::Ready));
        assert!(STATUS_CYCLE.contains(&TaskStatus::InProgress));
        assert!(STATUS_CYCLE.contains(&TaskStatus::OnHold));
        assert!(STATUS_CYCLE.contains(&TaskStatus::Done));
        assert!(STATUS_CYCLE.contains(&TaskStatus::Cancelled));
    }

    #[test]
    fn status_cycle_excludes_blocked() {
        assert!(!STATUS_CYCLE.contains(&TaskStatus::Blocked));
    }

    #[test]
    fn status_cycle_order_is_deterministic() {
        let first: Vec<_> = STATUS_CYCLE.to_vec();
        let second: Vec<_> = STATUS_CYCLE.to_vec();
        assert_eq!(first, second);
    }

    #[test]
    fn assignee_label_unassigned() {
        let task = make_task("TASK-001", TaskStatus::Ready, "Test");
        let snapshot = TaskSnapshot::from_task(task);
        assert!(snapshot.assignee_label.is_empty());
    }

    #[test]
    fn assignee_label_human() {
        let mut task = make_task("TASK-001", TaskStatus::Ready, "Test");
        task.assignee = Assignee::Human {
            user_id: "alice".to_string(),
        };
        let snapshot = TaskSnapshot::from_task(task);
        assert_eq!(snapshot.assignee_label, "alice");
    }

    #[test]
    fn assignee_label_agent_without_model() {
        let mut task = make_task("TASK-001", TaskStatus::Ready, "Test");
        task.assignee = Assignee::Agent {
            role: "developer".to_string(),
            model: None,
        };
        let snapshot = TaskSnapshot::from_task(task);
        assert_eq!(snapshot.assignee_label, "agent:developer");
    }

    #[test]
    fn assignee_label_agent_with_model() {
        let mut task = make_task("TASK-001", TaskStatus::Ready, "Test");
        task.assignee = Assignee::Agent {
            role: "reviewer".to_string(),
            model: Some("claude-3".to_string()),
        };
        let snapshot = TaskSnapshot::from_task(task);
        assert_eq!(snapshot.assignee_label, "agent:reviewer/claude-3");
    }

    #[test]
    fn status_label_method_matches_function() {
        let snapshot = TaskSnapshot {
            id: "TASK-001".to_string(),
            status: TaskStatus::InProgress,
            title: "Test".to_string(),
            description: String::new(),
            assignee_label: String::new(),
        };
        assert_eq!(
            snapshot.status_label(),
            status_label(TaskStatus::InProgress)
        );
    }
}
