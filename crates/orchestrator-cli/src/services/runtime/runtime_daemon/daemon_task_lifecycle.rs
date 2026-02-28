use super::*;

fn normalize_requirement_lifecycle_phase(phase: &str) -> Option<&'static str> {
    match phase.trim().to_ascii_lowercase().as_str() {
        "refine" | "refined" => Some("refine"),
        "po-review" | "po_review" | "poreview" => Some("po-review"),
        "em-review" | "em_review" | "emreview" => Some("em-review"),
        "rework" | "needs-rework" | "needs_rework" => Some("rework"),
        "research" => Some("research"),
        "approved" => Some("approved"),
        _ => None,
    }
}

fn requirement_status_label(status: orchestrator_core::RequirementStatus) -> &'static str {
    match status {
        orchestrator_core::RequirementStatus::Draft => "draft",
        orchestrator_core::RequirementStatus::Refined => "refined",
        orchestrator_core::RequirementStatus::Planned => "planned",
        orchestrator_core::RequirementStatus::InProgress => "in-progress",
        orchestrator_core::RequirementStatus::Done => "done",
        orchestrator_core::RequirementStatus::PoReview => "po-review",
        orchestrator_core::RequirementStatus::EmReview => "em-review",
        orchestrator_core::RequirementStatus::NeedsRework => "needs-rework",
        orchestrator_core::RequirementStatus::Approved => "approved",
        orchestrator_core::RequirementStatus::Implemented => "implemented",
        orchestrator_core::RequirementStatus::Deprecated => "deprecated",
    }
}

fn requirement_lifecycle_comment_key(requirement_id: &str, phase: &str, content: &str) -> String {
    format!(
        "{}|{}|{}",
        requirement_id,
        phase.trim().to_ascii_lowercase(),
        content.trim().to_ascii_lowercase()
    )
}

pub fn collect_requirement_lifecycle_transitions(
    before: &[orchestrator_core::RequirementItem],
    after: &[orchestrator_core::RequirementItem],
) -> Vec<RequirementLifecycleTransition> {
    let mut seen_comment_keys = HashSet::new();
    for requirement in before {
        for comment in &requirement.comments {
            let Some(phase) = comment
                .phase
                .as_deref()
                .and_then(normalize_requirement_lifecycle_phase)
            else {
                continue;
            };
            seen_comment_keys.insert(requirement_lifecycle_comment_key(
                &requirement.id,
                phase,
                &comment.content,
            ));
        }
    }

    let mut transitions = Vec::new();
    for requirement in after {
        for comment in &requirement.comments {
            let Some(phase) = comment
                .phase
                .as_deref()
                .and_then(normalize_requirement_lifecycle_phase)
            else {
                continue;
            };
            let key = requirement_lifecycle_comment_key(&requirement.id, phase, &comment.content);
            if seen_comment_keys.contains(&key) {
                continue;
            }
            transitions.push(RequirementLifecycleTransition {
                requirement_id: requirement.id.clone(),
                requirement_title: requirement.title.clone(),
                phase: phase.to_string(),
                status: requirement_status_label(requirement.status).to_string(),
                transition_at: comment.timestamp.to_rfc3339(),
                comment: {
                    let trimmed = comment.content.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                },
            });
        }
    }

    transitions.sort_by(|a, b| {
        a.transition_at
            .cmp(&b.transition_at)
            .then(a.requirement_id.cmp(&b.requirement_id))
            .then(a.phase.cmp(&b.phase))
    });
    transitions
}

pub fn collect_task_state_transitions(
    before: &[orchestrator_core::OrchestratorTask],
    after: &[orchestrator_core::OrchestratorTask],
    workflows: &[orchestrator_core::OrchestratorWorkflow],
    phase_events: &[PhaseExecutionEvent],
    ready_starts: &[ReadyTaskWorkflowStart],
) -> Vec<TaskStateTransition> {
    let before_lookup: std::collections::HashMap<&str, &orchestrator_core::OrchestratorTask> =
        before.iter().map(|task| (task.id.as_str(), task)).collect();

    let mut phase_context_by_task: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    for event in phase_events {
        phase_context_by_task.insert(
            event.task_id.clone(),
            (event.workflow_id.clone(), event.phase_id.clone()),
        );
    }

    let mut workflow_context_by_task: std::collections::HashMap<
        String,
        (String, Option<String>, i64),
    > = std::collections::HashMap::new();
    for workflow in workflows {
        let started_at_unix_ms = workflow.started_at.timestamp_millis();
        let candidate = (
            workflow.id.clone(),
            normalize_optional_id(workflow.current_phase.as_deref()),
            started_at_unix_ms,
        );
        match workflow_context_by_task.get_mut(workflow.task_id.as_str()) {
            Some(existing) if existing.2 >= started_at_unix_ms => {}
            Some(existing) => {
                *existing = candidate;
            }
            None => {
                workflow_context_by_task.insert(workflow.task_id.clone(), candidate);
            }
        }
    }

    let mut selection_source_by_task: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    for start in ready_starts {
        selection_source_by_task.insert(
            start.task_id.clone(),
            (
                start.workflow_id.clone(),
                start.selection_source.as_str().to_string(),
            ),
        );
    }

    let mut transitions = Vec::new();
    for task in after {
        let Some(previous) = before_lookup.get(task.id.as_str()) else {
            continue;
        };
        if previous.status == task.status {
            continue;
        }

        let (mut workflow_id, phase_id) = match phase_context_by_task.get(task.id.as_str()) {
            Some((workflow_id, phase_id)) => (
                Some(workflow_id.clone()),
                normalize_optional_id(Some(phase_id.as_str())),
            ),
            None => workflow_context_by_task
                .get(task.id.as_str())
                .map(|(workflow_id, phase_id, _)| (Some(workflow_id.clone()), phase_id.clone()))
                .unwrap_or((None, None)),
        };
        let selection_source = selection_source_by_task.get(task.id.as_str()).map(
            |(started_workflow_id, selection_source)| {
                if workflow_id.is_none() {
                    workflow_id = Some(started_workflow_id.clone());
                }
                selection_source.clone()
            },
        );

        transitions.push(TaskStateTransition {
            task_id: task.id.clone(),
            from_status: task_status_label(previous.status).to_string(),
            to_status: task_status_label(task.status).to_string(),
            changed_at: task.metadata.updated_at.to_rfc3339(),
            workflow_id,
            phase_id,
            selection_source,
        });
    }

    transitions.sort_by(|a, b| {
        a.changed_at
            .cmp(&b.changed_at)
            .then(a.task_id.cmp(&b.task_id))
    });
    transitions
}

pub async fn ensure_tasks_for_unplanned_requirements(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let requirements = hub.planning().list_requirements().await?;
    let tasks = hub.tasks().list().await?;

    let unplanned: Vec<String> = requirements
        .iter()
        .filter(|req| {
            !matches!(
                req.status,
                RequirementStatus::Done
                    | RequirementStatus::Implemented
                    | RequirementStatus::Deprecated
            )
        })
        .filter(|req| !requirement_has_active_tasks(req, &tasks))
        .map(|req| req.id.clone())
        .take(1)
        .collect();

    if unplanned.is_empty() {
        return Ok(0);
    }

    let summary = ensure_ai_generated_tasks_for_requirements(hub, project_root, &unplanned).await?;
    Ok(summary.requirements_generated)
}

pub async fn promote_backlog_tasks_to_ready(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            )
        })
        .map(|workflow| workflow.task_id.clone())
        .collect();

    let candidates = hub.tasks().list().await?;
    let mut promoted = 0usize;

    for task in &candidates {
        if task.paused || task.cancelled {
            continue;
        }
        if task.status != TaskStatus::Backlog {
            continue;
        }
        if active_task_ids.contains(&task.id) {
            continue;
        }

        let dependency_issues =
            dependency_gate_issues_for_task(hub.clone(), project_root, task).await;
        if !dependency_issues.is_empty() {
            let reason = dependency_blocked_reason(&dependency_issues);
            let _ = set_task_blocked_with_reason(hub.clone(), task, reason, None).await;
            continue;
        }

        let _ = hub.tasks().set_status(&task.id, TaskStatus::Ready).await;
        promoted = promoted.saturating_add(1);
    }

    Ok(promoted)
}

const DEFAULT_RETRY_COOLDOWN_SECS: i64 = 300;
const DEFAULT_MAX_TASK_RETRIES: usize = 3;

pub async fn retry_failed_task_workflows(hub: Arc<dyn ServiceHub>) -> Result<usize> {
    let cooldown_secs = std::env::var("AO_RETRY_COOLDOWN_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_RETRY_COOLDOWN_SECS);
    let max_retries = std::env::var("AO_MAX_TASK_RETRIES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_MAX_TASK_RETRIES);

    let tasks = hub.tasks().list().await?;
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let now = Utc::now();
    let mut retried = 0usize;

    for task in &tasks {
        if retried >= 1 {
            break;
        }
        if task.paused || task.cancelled {
            continue;
        }
        if task.status != TaskStatus::Blocked {
            continue;
        }
        if is_merge_gate_block(task) || is_dependency_gate_block(task) {
            continue;
        }

        let task_workflows: Vec<_> = workflows.iter().filter(|w| w.task_id == task.id).collect();
        let latest = task_workflows.iter().max_by_key(|w| w.started_at);

        let Some(latest) = latest else {
            continue;
        };
        if latest.status != WorkflowStatus::Failed {
            continue;
        }

        let failed_count = task_workflows
            .iter()
            .filter(|w| w.status == WorkflowStatus::Failed)
            .count();
        if failed_count >= max_retries {
            continue;
        }

        if let Some(completed_at) = latest.completed_at {
            let elapsed = now.signed_duration_since(completed_at).num_seconds();
            if elapsed < cooldown_secs {
                continue;
            }
        }

        let _ = hub.tasks().set_status(&task.id, TaskStatus::Ready).await;
        retried = retried.saturating_add(1);
    }

    Ok(retried)
}
