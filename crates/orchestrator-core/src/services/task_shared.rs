use super::*;

pub(super) fn next_task_id(tasks: &HashMap<String, OrchestratorTask>) -> String {
    let next_seq = tasks
        .keys()
        .filter_map(|task_id| task_id.strip_prefix("TASK-"))
        .filter_map(|seq| seq.parse::<u32>().ok())
        .max()
        .map_or(1, |max_seq| max_seq.saturating_add(1));
    format!("TASK-{next_seq:03}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskStatusTransitionEvent {
    SetStatus(TaskStatus),
    Reopen(TaskStatus),
}

impl TaskStatusTransitionEvent {
    fn target(self) -> TaskStatus {
        match self {
            Self::SetStatus(target) | Self::Reopen(target) => target,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::SetStatus(_) => "set-status",
            Self::Reopen(_) => "reopen",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TaskStatusTransitionOutcome {
    pub from: TaskStatus,
    pub to: TaskStatus,
    pub event: TaskStatusTransitionEvent,
    pub matched: bool,
}

fn task_status_label(status: TaskStatus) -> &'static str {
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

fn invalid_task_status_transition_error(
    current: TaskStatus,
    event: TaskStatusTransitionEvent,
    reason: &str,
) -> anyhow::Error {
    anyhow!(
        "invalid task status transition: {} -> {} via {}; {}",
        task_status_label(current),
        task_status_label(event.target()),
        event.label(),
        reason
    )
}

pub(super) fn evaluate_task_status_transition(
    current: TaskStatus,
    event: TaskStatusTransitionEvent,
) -> Result<TaskStatusTransitionOutcome> {
    let target = event.target();
    match event {
        TaskStatusTransitionEvent::SetStatus(target) => {
            if current == target {
                return Ok(TaskStatusTransitionOutcome {
                    from: current,
                    to: target,
                    event,
                    matched: false,
                });
            }

            if current.is_terminal() {
                return Err(invalid_task_status_transition_error(
                    current,
                    event,
                    "terminal states require explicit reopen action",
                ));
            }

            if target == TaskStatus::InProgress
                && !matches!(current, TaskStatus::Backlog | TaskStatus::Ready)
            {
                return Err(invalid_task_status_transition_error(
                    current,
                    event,
                    "in-progress requires prior status backlog or ready",
                ));
            }

            if target == TaskStatus::Done && current != TaskStatus::InProgress {
                return Err(invalid_task_status_transition_error(
                    current,
                    event,
                    "done requires prior status in-progress",
                ));
            }
        }
        TaskStatusTransitionEvent::Reopen(target) => {
            if !current.is_terminal() {
                return Err(invalid_task_status_transition_error(
                    current,
                    event,
                    "reopen is only allowed from done or cancelled",
                ));
            }

            if !matches!(target, TaskStatus::Backlog | TaskStatus::Ready) {
                return Err(invalid_task_status_transition_error(
                    current,
                    event,
                    "reopen target must be backlog or ready",
                ));
            }
        }
    }

    Ok(TaskStatusTransitionOutcome {
        from: current,
        to: target,
        event,
        matched: true,
    })
}

pub(super) fn set_task_status(task: &mut OrchestratorTask, status: TaskStatus) -> Result<()> {
    evaluate_task_status_transition(task.status, TaskStatusTransitionEvent::SetStatus(status))?;
    apply_task_status(task, status);
    Ok(())
}

pub(super) fn reopen_task(task: &mut OrchestratorTask, status: TaskStatus) -> Result<()> {
    evaluate_task_status_transition(task.status, TaskStatusTransitionEvent::Reopen(status))?;
    apply_task_status(task, status);
    Ok(())
}

pub(super) fn apply_task_status(task: &mut OrchestratorTask, status: TaskStatus) {
    let previous_status = task.status;
    task.status = status;
    task.paused = status.is_blocked();
    task.cancelled = matches!(status, TaskStatus::Cancelled);
    if status == TaskStatus::InProgress && task.metadata.started_at.is_none() {
        task.metadata.started_at = Some(Utc::now());
    }
    if status == TaskStatus::Done {
        if task.metadata.completed_at.is_none() {
            task.metadata.completed_at = Some(Utc::now());
        }
    } else if previous_status == TaskStatus::Done {
        // Reopened tasks should record the next completion time when they reach done again.
        task.metadata.completed_at = None;
    }
    if status == TaskStatus::Blocked {
        if task.blocked_reason.is_none() {
            task.blocked_reason = Some("Blocked by status update".to_string());
        }
        if task.blocked_at.is_none() {
            task.blocked_at = Some(Utc::now());
        }
    }
    if !status.is_blocked() {
        task.blocked_reason = None;
        task.blocked_at = None;
        task.blocked_phase = None;
        task.blocked_by = None;
    }
}

pub(super) fn apply_task_update(task: &mut OrchestratorTask, input: TaskUpdateInput) -> Result<()> {
    let TaskUpdateInput {
        title,
        description,
        priority,
        status,
        assignee,
        tags,
        updated_by,
        deadline,
        linked_architecture_entities,
    } = input;

    if let Some(next_status) = status {
        evaluate_task_status_transition(
            task.status,
            TaskStatusTransitionEvent::SetStatus(next_status),
        )?;
    }

    if let Some(title) = title {
        task.title = title;
    }
    if let Some(description) = description {
        task.description = description;
    }
    if let Some(priority) = priority {
        task.priority = priority;
    }
    if let Some(status) = status {
        apply_task_status(task, status);
    }
    if let Some(assignee) = assignee {
        task.assignee = Assignee::Human { user_id: assignee };
    }
    if let Some(tags) = tags {
        task.tags = tags;
    }
    if let Some(deadline) = deadline {
        task.deadline = if deadline.trim().is_empty() {
            None
        } else {
            Some(deadline)
        };
    }
    if let Some(linked_architecture_entities) = linked_architecture_entities {
        task.linked_architecture_entities = linked_architecture_entities;
    }
    task.metadata.updated_at = Utc::now();
    task.metadata.version = task.metadata.version.saturating_add(1);
    if let Some(updated_by) = updated_by {
        task.metadata.updated_by = updated_by;
    }
    Ok(())
}

pub(super) fn validate_linked_architecture_entities(
    architecture: &ArchitectureGraph,
    entity_ids: &[String],
) -> Result<()> {
    for entity_id in entity_ids {
        if !architecture.has_entity(entity_id) {
            return Err(anyhow!("linked architecture entity not found: {entity_id}"));
        }
    }
    Ok(())
}

fn priority_rank(priority: Priority) -> usize {
    match priority {
        Priority::Critical => 0,
        Priority::High => 1,
        Priority::Medium => 2,
        Priority::Low => 3,
    }
}

fn assignee_type_label(assignee: &Assignee) -> &'static str {
    match assignee {
        Assignee::Agent { .. } => "agent",
        Assignee::Human { .. } => "human",
        Assignee::Unassigned => "unassigned",
    }
}

pub(super) fn task_matches_filter(task: &OrchestratorTask, filter: &TaskFilter) -> bool {
    if let Some(task_type) = filter.task_type {
        if task.task_type != task_type {
            return false;
        }
    }

    if let Some(status) = filter.status {
        if task.status != status {
            return false;
        }
    }

    if let Some(priority) = filter.priority {
        if task.priority != priority {
            return false;
        }
    }

    if let Some(risk) = filter.risk {
        if task.risk != risk {
            return false;
        }
    }

    if let Some(ref assignee_type) = filter.assignee_type {
        if assignee_type_label(&task.assignee) != assignee_type.as_str() {
            return false;
        }
    }

    if let Some(ref tags) = filter.tags {
        if !tags.iter().all(|tag| task.tags.contains(tag)) {
            return false;
        }
    }

    if let Some(ref requirement) = filter.linked_requirement {
        if !task.linked_requirements.contains(requirement) {
            return false;
        }
    }

    if let Some(ref entity_id) = filter.linked_architecture_entity {
        if !task.linked_architecture_entities.contains(entity_id) {
            return false;
        }
    }

    if let Some(ref search) = filter.search_text {
        let needle = search.to_ascii_lowercase();
        let haystack = format!("{} {}", task.title, task.description).to_ascii_lowercase();
        if !haystack.contains(&needle) {
            return false;
        }
    }

    true
}

pub(super) fn sort_tasks_by_priority(tasks: &mut [OrchestratorTask]) {
    tasks.sort_by(|a, b| {
        priority_rank(a.priority)
            .cmp(&priority_rank(b.priority))
            .then_with(|| b.metadata.updated_at.cmp(&a.metadata.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });
}

pub(super) fn build_task_statistics(tasks: &[OrchestratorTask]) -> TaskStatistics {
    let mut by_status: HashMap<String, usize> = HashMap::new();
    let mut by_priority: HashMap<String, usize> = HashMap::new();
    let mut by_type: HashMap<String, usize> = HashMap::new();

    for task in tasks {
        let status_key =
            serde_json::to_string(&task.status).unwrap_or_else(|_| "unknown".to_string());
        let status_key = status_key.trim_matches('"').to_string();
        *by_status.entry(status_key).or_insert(0) += 1;

        let priority_key =
            serde_json::to_string(&task.priority).unwrap_or_else(|_| "unknown".to_string());
        let priority_key = priority_key.trim_matches('"').to_string();
        *by_priority.entry(priority_key).or_insert(0) += 1;

        *by_type
            .entry(task.task_type.as_str().to_string())
            .or_insert(0) += 1;
    }

    TaskStatistics {
        total: tasks.len(),
        by_status,
        by_priority,
        by_type,
        in_progress: tasks
            .iter()
            .filter(|task| task.status == TaskStatus::InProgress)
            .count(),
        blocked: tasks.iter().filter(|task| task.status.is_blocked()).count(),
        completed: tasks
            .iter()
            .filter(|task| task.status.is_terminal())
            .count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task(status: TaskStatus) -> OrchestratorTask {
        let now = Utc::now();
        OrchestratorTask {
            id: "TASK-001".to_string(),
            title: "sample".to_string(),
            description: "sample task".to_string(),
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
            resource_requirements: Default::default(),
        }
    }

    #[test]
    fn set_status_rejects_in_progress_from_non_ready_states() {
        let blocked_error = evaluate_task_status_transition(
            TaskStatus::Blocked,
            TaskStatusTransitionEvent::SetStatus(TaskStatus::InProgress),
        )
        .expect_err("blocked -> in-progress should be rejected");
        assert!(blocked_error
            .to_string()
            .contains("in-progress requires prior status backlog or ready"));

        let on_hold_error = evaluate_task_status_transition(
            TaskStatus::OnHold,
            TaskStatusTransitionEvent::SetStatus(TaskStatus::InProgress),
        )
        .expect_err("on-hold -> in-progress should be rejected");
        assert!(on_hold_error
            .to_string()
            .contains("in-progress requires prior status backlog or ready"));
    }

    #[test]
    fn set_status_rejects_done_without_in_progress() {
        let error = evaluate_task_status_transition(
            TaskStatus::Ready,
            TaskStatusTransitionEvent::SetStatus(TaskStatus::Done),
        )
        .expect_err("ready -> done should be rejected");
        assert!(error
            .to_string()
            .contains("done requires prior status in-progress"));
    }

    #[test]
    fn set_status_rejects_terminal_exit_without_reopen() {
        let error = evaluate_task_status_transition(
            TaskStatus::Done,
            TaskStatusTransitionEvent::SetStatus(TaskStatus::Ready),
        )
        .expect_err("done -> ready should require reopen");
        assert!(error
            .to_string()
            .contains("terminal states require explicit reopen action"));
    }

    #[test]
    fn reopen_allows_terminal_to_ready_or_backlog() {
        let done_to_backlog = evaluate_task_status_transition(
            TaskStatus::Done,
            TaskStatusTransitionEvent::Reopen(TaskStatus::Backlog),
        )
        .expect("done -> backlog reopen should be allowed");
        assert!(done_to_backlog.matched);

        let cancelled_to_ready = evaluate_task_status_transition(
            TaskStatus::Cancelled,
            TaskStatusTransitionEvent::Reopen(TaskStatus::Ready),
        )
        .expect("cancelled -> ready reopen should be allowed");
        assert!(cancelled_to_ready.matched);
    }

    #[test]
    fn reopen_rejects_non_terminal_sources() {
        let error = evaluate_task_status_transition(
            TaskStatus::Ready,
            TaskStatusTransitionEvent::Reopen(TaskStatus::Backlog),
        )
        .expect_err("reopen should be rejected for non-terminal status");
        assert!(error
            .to_string()
            .contains("reopen is only allowed from done or cancelled"));
    }

    #[test]
    fn reopen_rejects_terminal_targets() {
        let error = evaluate_task_status_transition(
            TaskStatus::Done,
            TaskStatusTransitionEvent::Reopen(TaskStatus::Done),
        )
        .expect_err("reopen target should be limited to backlog or ready");
        assert!(error
            .to_string()
            .contains("reopen target must be backlog or ready"));
    }

    #[test]
    fn apply_task_update_rejects_invalid_status_without_mutating_task() {
        let mut task = sample_task(TaskStatus::Done);
        let original_title = task.title.clone();
        let original_updated_by = task.metadata.updated_by.clone();
        let original_status = task.status;
        let original_version = task.metadata.version;
        let original_updated_at = task.metadata.updated_at;

        let error = apply_task_update(
            &mut task,
            TaskUpdateInput {
                title: Some("new title".to_string()),
                description: None,
                priority: None,
                status: Some(TaskStatus::Ready),
                assignee: None,
                tags: None,
                updated_by: Some("changed".to_string()),
                deadline: None,
                linked_architecture_entities: None,
            },
        )
        .expect_err("invalid status update should fail");

        assert!(error
            .to_string()
            .contains("terminal states require explicit reopen action"));
        assert_eq!(task.status, original_status);
        assert_eq!(task.title, original_title);
        assert_eq!(task.metadata.updated_by, original_updated_by);
        assert_eq!(task.metadata.version, original_version);
        assert_eq!(task.metadata.updated_at, original_updated_at);
    }

    #[test]
    fn reopen_from_done_clears_stale_completed_at_and_recomputes_on_next_done() {
        let mut task = sample_task(TaskStatus::Done);
        let stale_completed_at = Utc::now() - chrono::Duration::days(1);
        task.metadata.completed_at = Some(stale_completed_at);

        reopen_task(&mut task, TaskStatus::Ready).expect("reopen from done should succeed");
        assert!(task.metadata.completed_at.is_none());

        set_task_status(&mut task, TaskStatus::InProgress)
            .expect("ready -> in-progress should succeed");
        set_task_status(&mut task, TaskStatus::Done).expect("in-progress -> done should succeed");
        let refreshed_completed_at = task
            .metadata
            .completed_at
            .expect("completed_at should be recomputed after reaching done");
        assert!(refreshed_completed_at > stale_completed_at);
    }
}
