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

pub(super) fn apply_task_status(task: &mut OrchestratorTask, status: TaskStatus) {
    task.status = status;
    task.paused = status.is_blocked();
    task.cancelled = matches!(status, TaskStatus::Cancelled);
    if status == TaskStatus::InProgress && task.metadata.started_at.is_none() {
        task.metadata.started_at = Some(Utc::now());
    }
    if status == TaskStatus::Done && task.metadata.completed_at.is_none() {
        task.metadata.completed_at = Some(Utc::now());
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

pub(super) fn apply_task_update(task: &mut OrchestratorTask, input: TaskUpdateInput) {
    if let Some(title) = input.title {
        task.title = title;
    }
    if let Some(description) = input.description {
        task.description = description;
    }
    if let Some(priority) = input.priority {
        task.priority = priority;
    }
    if let Some(status) = input.status {
        apply_task_status(task, status);
    }
    if let Some(assignee) = input.assignee {
        task.assignee = Assignee::Human { user_id: assignee };
    }
    if let Some(tags) = input.tags {
        task.tags = tags;
    }
    if let Some(deadline) = input.deadline {
        task.deadline = if deadline.trim().is_empty() {
            None
        } else {
            Some(deadline)
        };
    }
    task.metadata.updated_at = Utc::now();
    task.metadata.version = task.metadata.version.saturating_add(1);
    if let Some(updated_by) = input.updated_by {
        task.metadata.updated_by = updated_by;
    }
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
