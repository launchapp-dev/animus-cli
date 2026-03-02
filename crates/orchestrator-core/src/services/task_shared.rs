use super::*;
use std::collections::HashSet;

pub(crate) fn next_sequential_id<'a>(
    keys: impl Iterator<Item = &'a String>,
    prefix: &str,
) -> String {
    let next_seq = keys
        .filter_map(|id| id.strip_prefix(prefix))
        .filter_map(|seq| seq.parse::<u32>().ok())
        .max()
        .map_or(1, |max_seq| max_seq.saturating_add(1));
    format!("{prefix}{next_seq:03}")
}

pub(super) fn next_task_id(tasks: &HashMap<String, OrchestratorTask>) -> String {
    next_sequential_id(tasks.keys(), "TASK-")
}

pub(super) fn validate_task_status_transition(
    current: TaskStatus,
    target: TaskStatus,
) -> Result<()> {
    if current == target {
        return Ok(());
    }
    match current {
        TaskStatus::Done | TaskStatus::Cancelled => {
            Err(anyhow!(
                "cannot transition from {} to {} — use reopen to move out of terminal state",
                serde_json::to_value(current)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| format!("{:?}", current)),
                serde_json::to_value(target)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| format!("{:?}", target)),
            ))
        }
        _ => Ok(()),
    }
}

pub(super) fn apply_task_status(task: &mut OrchestratorTask, status: TaskStatus) {
    let is_blocked_status = status.is_blocked();
    task.status = status;
    task.paused = is_blocked_status;
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
    if !is_blocked_status {
        task.paused = false;
        task.blocked_reason = None;
        task.blocked_at = None;
        task.blocked_phase = None;
        task.blocked_by = None;
    }
}

pub(super) fn apply_task_update(task: &mut OrchestratorTask, input: TaskUpdateInput) -> Result<()> {
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
        validate_task_status_transition(task.status, status)?;
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
    if let Some(linked_architecture_entities) = input.linked_architecture_entities {
        task.linked_architecture_entities = linked_architecture_entities;
    }
    task.metadata.updated_at = Utc::now();
    task.metadata.version = task.metadata.version.saturating_add(1);
    if let Some(updated_by) = input.updated_by {
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

pub(super) fn evaluate_task_priority_policy_report(
    tasks: &[OrchestratorTask],
    high_budget_percent: u8,
) -> Result<TaskPriorityPolicyReport> {
    validate_high_budget_percent(high_budget_percent)?;

    let mut total_by_priority = TaskPriorityDistribution::default();
    let mut active_by_priority = TaskPriorityDistribution::default();
    let mut active_tasks = 0usize;

    for task in tasks {
        increment_priority_distribution(&mut total_by_priority, task.priority);
        if !task.status.is_terminal() {
            active_tasks = active_tasks.saturating_add(1);
            increment_priority_distribution(&mut active_by_priority, task.priority);
        }
    }

    let high_budget_limit = compute_high_budget_limit(active_tasks, high_budget_percent);
    let active_high_count = active_by_priority.high;
    let high_budget_overflow = active_high_count.saturating_sub(high_budget_limit);

    Ok(TaskPriorityPolicyReport {
        high_budget_percent,
        high_budget_limit,
        total_tasks: tasks.len(),
        active_tasks,
        total_by_priority,
        active_by_priority,
        high_budget_compliant: high_budget_overflow == 0,
        high_budget_overflow,
    })
}

pub(super) fn plan_task_priority_rebalance_from_tasks(
    tasks: &[OrchestratorTask],
    options: TaskPriorityRebalanceOptions,
) -> Result<TaskPriorityRebalancePlan> {
    let high_budget_percent = options.high_budget_percent;
    validate_high_budget_percent(high_budget_percent)?;

    let task_ids: HashSet<&str> = tasks.iter().map(|task| task.id.as_str()).collect();
    let essential_task_ids = normalized_task_id_set(&options.essential_task_ids);
    let nice_to_have_task_ids = normalized_task_id_set(&options.nice_to_have_task_ids);
    validate_override_task_ids(&task_ids, &essential_task_ids, "essential_task_ids")?;
    validate_override_task_ids(&task_ids, &nice_to_have_task_ids, "nice_to_have_task_ids")?;
    validate_conflicting_override_task_ids(&essential_task_ids, &nice_to_have_task_ids)?;

    let mut target_priorities: HashMap<String, Priority> = HashMap::new();
    for task in tasks
        .iter()
        .filter(|task| !task.status.is_terminal() && task.status.is_blocked())
    {
        target_priorities.insert(task.id.clone(), Priority::Critical);
    }

    let active_tasks = tasks
        .iter()
        .filter(|task| !task.status.is_terminal())
        .count();
    let high_budget_limit = compute_high_budget_limit(active_tasks, high_budget_percent);
    let mut high_candidates: Vec<&OrchestratorTask> = tasks
        .iter()
        .filter(|task| {
            !task.status.is_terminal()
                && !task.status.is_blocked()
                && !nice_to_have_task_ids.contains(task.id.as_str())
        })
        .collect();
    high_candidates.sort_by(|left, right| {
        essential_rank(left.id.as_str(), &essential_task_ids)
            .cmp(&essential_rank(right.id.as_str(), &essential_task_ids))
            .then_with(|| status_rank(left.status).cmp(&status_rank(right.status)))
            .then_with(|| {
                compare_optional_deadlines(left.deadline.as_deref(), right.deadline.as_deref())
            })
            .then_with(|| right.metadata.updated_at.cmp(&left.metadata.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    for task in high_candidates.into_iter().take(high_budget_limit) {
        target_priorities.insert(task.id.clone(), Priority::High);
    }

    for task in tasks {
        if target_priorities.contains_key(task.id.as_str()) {
            continue;
        }

        if nice_to_have_task_ids.contains(task.id.as_str()) || task.priority == Priority::Low {
            target_priorities.insert(task.id.clone(), Priority::Low);
        } else {
            target_priorities.insert(task.id.clone(), Priority::Medium);
        }
    }

    let mut planned_tasks = tasks.to_vec();
    for task in &mut planned_tasks {
        if let Some(priority) = target_priorities.get(task.id.as_str()) {
            task.priority = *priority;
        }
    }

    let before = evaluate_task_priority_policy_report(tasks, high_budget_percent)?;
    let after = evaluate_task_priority_policy_report(&planned_tasks, high_budget_percent)?;

    let mut changes = Vec::new();
    for task in tasks {
        let target = target_priorities
            .get(task.id.as_str())
            .copied()
            .unwrap_or(task.priority);
        if task.priority != target {
            changes.push(TaskPriorityRebalanceChange {
                task_id: task.id.clone(),
                from: task.priority,
                to: target,
            });
        }
    }
    changes.sort_by(|left, right| left.task_id.cmp(&right.task_id));

    Ok(TaskPriorityRebalancePlan {
        high_budget_percent,
        before,
        after,
        changes,
    })
}

fn validate_high_budget_percent(high_budget_percent: u8) -> Result<()> {
    if high_budget_percent > 100 {
        return Err(anyhow!(
            "invalid high_budget_percent {high_budget_percent}; expected value between 0 and 100"
        ));
    }
    Ok(())
}

fn increment_priority_distribution(
    distribution: &mut TaskPriorityDistribution,
    priority: Priority,
) {
    match priority {
        Priority::Critical => distribution.critical = distribution.critical.saturating_add(1),
        Priority::High => distribution.high = distribution.high.saturating_add(1),
        Priority::Medium => distribution.medium = distribution.medium.saturating_add(1),
        Priority::Low => distribution.low = distribution.low.saturating_add(1),
    }
}

fn compute_high_budget_limit(active_tasks: usize, high_budget_percent: u8) -> usize {
    active_tasks.saturating_mul(usize::from(high_budget_percent)) / 100
}

fn normalized_task_id_set(task_ids: &[String]) -> HashSet<String> {
    task_ids
        .iter()
        .map(|task_id| task_id.trim())
        .filter(|task_id| !task_id.is_empty())
        .map(str::to_string)
        .collect()
}

fn validate_override_task_ids(
    task_ids: &HashSet<&str>,
    overrides: &HashSet<String>,
    field_name: &str,
) -> Result<()> {
    let mut unknown_ids: Vec<&str> = overrides
        .iter()
        .map(String::as_str)
        .filter(|task_id| !task_ids.contains(*task_id))
        .collect();
    unknown_ids.sort_unstable();
    if unknown_ids.is_empty() {
        return Ok(());
    }

    Err(anyhow!(
        "unknown task ids provided in {field_name}: {}",
        unknown_ids.join(", ")
    ))
}

fn validate_conflicting_override_task_ids(
    essential_task_ids: &HashSet<String>,
    nice_to_have_task_ids: &HashSet<String>,
) -> Result<()> {
    let mut overlapping_ids: Vec<&str> = essential_task_ids
        .iter()
        .map(String::as_str)
        .filter(|task_id| nice_to_have_task_ids.contains(*task_id))
        .collect();
    overlapping_ids.sort_unstable();
    if overlapping_ids.is_empty() {
        return Ok(());
    }

    Err(anyhow!(
        "conflicting task ids provided in overrides; same id cannot be both essential and nice-to-have: {}",
        overlapping_ids.join(", ")
    ))
}

fn essential_rank(task_id: &str, essential_task_ids: &HashSet<String>) -> usize {
    if essential_task_ids.contains(task_id) {
        0
    } else {
        1
    }
}

fn status_rank(status: TaskStatus) -> usize {
    match status {
        TaskStatus::InProgress => 0,
        TaskStatus::Ready | TaskStatus::Backlog => 1,
        _ => 2,
    }
}

fn compare_optional_deadlines(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
    let left = parse_deadline(left);
    let right = parse_deadline(right);
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn parse_deadline(value: Option<&str>) -> Option<chrono::DateTime<Utc>> {
    value
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|timestamp| timestamp.with_timezone(&Utc))
}
