use super::*;
use crate::services::runtime::stale_in_progress_summary;

const EM_SCHEDULING_RESULT_KIND: &str = "em_scheduling_decision";
const EM_SCHEDULING_TIMEOUT_SECS: u64 = 120;
const EM_SCHEDULING_MAX_TASKS: usize = 200;
const EM_SCHEDULING_MAX_DECISIONS: usize = 40;
const EM_SCHEDULING_MAX_FAILURES: usize = 20;
const EM_SCHEDULING_AGENT_TOGGLE_ENV: &str = "AO_ENABLE_EM_SCHEDULING_AGENT";
const EM_SCHEDULING_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/runtime/em_scheduling.prompt"
));

#[derive(Debug, Clone, Serialize)]
struct EmSchedulingPromptTaskDependency {
    task_id: String,
    dependency_type: String,
}

#[derive(Debug, Clone, Serialize)]
struct EmSchedulingPromptTask {
    task_id: String,
    title: String,
    status: String,
    priority: String,
    risk: String,
    complexity: String,
    paused: bool,
    cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked_reason: Option<String>,
    linked_requirements: Vec<String>,
    dependencies: Vec<EmSchedulingPromptTaskDependency>,
}

#[derive(Debug, Clone, Serialize)]
struct EmSchedulingPromptWorkflow {
    workflow_id: String,
    task_id: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct EmSchedulingPromptDecisionRecord {
    workflow_id: String,
    task_id: String,
    phase_id: String,
    decision: String,
    risk: String,
    confidence: f32,
    reason: String,
    timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
struct EmSchedulingPromptFailure {
    workflow_id: String,
    task_id: String,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct EmSchedulingRequirementCoverage {
    requirement_id: String,
    total_tasks: usize,
    ready_tasks: usize,
    in_progress_tasks: usize,
    blocked_tasks: usize,
    done_tasks: usize,
}

#[derive(Debug, Clone, Serialize)]
struct EmSchedulingPromptInput {
    available_agent_slots: usize,
    tasks: Vec<EmSchedulingPromptTask>,
    running_workflows: Vec<EmSchedulingPromptWorkflow>,
    recent_decisions: Vec<EmSchedulingPromptDecisionRecord>,
    recent_failures: Vec<EmSchedulingPromptFailure>,
    requirement_coverage: Vec<EmSchedulingRequirementCoverage>,
}

#[derive(Debug, Clone, Deserialize)]
struct EmSchedulingQueueDecision {
    task_id: String,
    reason: String,
}

#[derive(Debug, Clone, Deserialize)]
struct EmSchedulingDecision {
    kind: String,
    queue: Vec<EmSchedulingQueueDecision>,
    defer: Vec<String>,
    preempt: Vec<String>,
}

#[derive(Debug, Clone)]
struct EmSchedulingAppliedDecision {
    queue_task_ids: Vec<String>,
    defer_task_ids: Vec<String>,
    preempt_task_ids: Vec<String>,
}

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

fn collect_requirement_lifecycle_transitions(
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

fn normalize_optional_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(|candidate| candidate.to_string())
}

fn is_terminally_completed_workflow(workflow: &orchestrator_core::OrchestratorWorkflow) -> bool {
    workflow.status == WorkflowStatus::Completed
        && workflow.machine_state == orchestrator_core::WorkflowMachineState::Completed
        && workflow.completed_at.is_some()
}

fn workflow_current_phase_id(workflow: &orchestrator_core::OrchestratorWorkflow) -> Option<String> {
    workflow
        .current_phase
        .as_deref()
        .map(str::to_string)
        .or_else(|| {
            workflow
                .phases
                .get(workflow.current_phase_index)
                .map(|phase| phase.phase_id.clone())
        })
        .and_then(|phase_id| normalize_optional_id(Some(phase_id.as_str())))
}

fn routing_complexity_for_task(
    task: &orchestrator_core::OrchestratorTask,
) -> Option<protocol::ModelRoutingComplexity> {
    match task.complexity {
        orchestrator_core::Complexity::Low => Some(protocol::ModelRoutingComplexity::Low),
        orchestrator_core::Complexity::Medium => Some(protocol::ModelRoutingComplexity::Medium),
        orchestrator_core::Complexity::High => Some(protocol::ModelRoutingComplexity::High),
    }
}

fn daemon_agent_assignee_for_workflow_start(
    project_root: &str,
    workflow: &orchestrator_core::OrchestratorWorkflow,
    task: &orchestrator_core::OrchestratorTask,
) -> (String, Option<String>) {
    let phase_id = workflow_current_phase_id(workflow).unwrap_or_else(|| "unknown".to_string());
    let runtime_config =
        orchestrator_core::load_agent_runtime_config_or_default(Path::new(project_root));
    let role = runtime_config
        .phase_agent_id(&phase_id)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| phase_id.clone());

    let fallback_models = runtime_config.phase_fallback_models(&phase_id);
    let execution_targets = PhaseTargetPlanner::build_phase_execution_targets(
        &phase_id,
        runtime_config.phase_model_override(&phase_id),
        runtime_config.phase_tool_override(&phase_id),
        fallback_models.as_slice(),
        routing_complexity_for_task(task),
    );
    let model = execution_targets.first().map(|(_, model)| model.clone());
    (role, model)
}

async fn auto_assign_task_to_daemon_agent(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    workflow: &orchestrator_core::OrchestratorWorkflow,
) -> Result<()> {
    let (role, model) = daemon_agent_assignee_for_workflow_start(project_root, workflow, task);
    hub.tasks()
        .assign_agent(&task.id, role, model, "ao-daemon".to_string())
        .await?;
    Ok(())
}

fn collect_task_state_transitions(
    before: &[orchestrator_core::OrchestratorTask],
    after: &[orchestrator_core::OrchestratorTask],
    workflows: &[orchestrator_core::OrchestratorWorkflow],
    phase_events: &[PhaseExecutionEvent],
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

    let mut transitions = Vec::new();
    for task in after {
        let Some(previous) = before_lookup.get(task.id.as_str()) else {
            continue;
        };
        if previous.status == task.status {
            continue;
        }

        let (workflow_id, phase_id) = match phase_context_by_task.get(task.id.as_str()) {
            Some((workflow_id, phase_id)) => (
                Some(workflow_id.clone()),
                normalize_optional_id(Some(phase_id.as_str())),
            ),
            None => workflow_context_by_task
                .get(task.id.as_str())
                .map(|(workflow_id, phase_id, _)| (Some(workflow_id.clone()), phase_id.clone()))
                .unwrap_or((None, None)),
        };

        transitions.push(TaskStateTransition {
            task_id: task.id.clone(),
            from_status: task_status_label(previous.status).to_string(),
            to_status: task_status_label(task.status).to_string(),
            changed_at: task.metadata.updated_at.to_rfc3339(),
            workflow_id,
            phase_id,
        });
    }

    transitions.sort_by(|a, b| {
        a.changed_at
            .cmp(&b.changed_at)
            .then(a.task_id.cmp(&b.task_id))
    });
    transitions
}

async fn set_task_blocked_with_reason(
    hub: Arc<dyn ServiceHub>,
    task: &orchestrator_core::OrchestratorTask,
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
    updated.metadata.updated_by = "ao-daemon".to_string();
    updated.metadata.version = updated.metadata.version.saturating_add(1);
    hub.tasks().replace(updated).await?;
    Ok(())
}

async fn dependency_gate_issues_for_task(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
) -> Vec<String> {
    let mut issues = Vec::new();

    for dependency in &task.dependencies {
        if dependency.dependency_type != DependencyType::BlockedBy {
            continue;
        }

        let dependency_task = match hub.tasks().get(&dependency.task_id).await {
            Ok(task) => task,
            Err(_) => {
                issues.push(format!("dependency {} does not exist", dependency.task_id));
                continue;
            }
        };

        if dependency_task.status != TaskStatus::Done {
            issues.push(format!(
                "dependency {} is {}",
                dependency.task_id,
                task_status_label(dependency_task.status)
            ));
            continue;
        }

        if let Some(branch_name) = dependency_task
            .branch_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            match is_branch_merged(project_root, branch_name) {
                Ok(Some(true)) | Ok(None) => {}
                Ok(Some(false)) => {
                    issues.push(format!(
                        "dependency {} branch `{}` is not merged",
                        dependency.task_id, branch_name
                    ));
                }
                Err(error) => {
                    issues.push(format!(
                        "unable to verify dependency {} merge status: {}",
                        dependency.task_id, error
                    ));
                }
            }
        }
    }

    issues
}

pub(super) async fn reconcile_dependency_gate_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids: HashSet<String> = workflows
        .into_iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            )
        })
        .map(|workflow| workflow.task_id)
        .collect();

    let mut changed = 0usize;
    let tasks = hub.tasks().list().await?;
    for task in tasks {
        if active_task_ids.contains(&task.id) || task.cancelled {
            continue;
        }

        let dependency_issues =
            dependency_gate_issues_for_task(hub.clone(), project_root, &task).await;
        if dependency_issues.is_empty() {
            if task.status == TaskStatus::Blocked && is_dependency_gate_block(&task) {
                hub.tasks().set_status(&task.id, TaskStatus::Ready).await?;
                changed = changed.saturating_add(1);
            }
            continue;
        }

        let reason = dependency_blocked_reason(&dependency_issues);
        let should_block = match task.status {
            TaskStatus::Ready | TaskStatus::Backlog => true,
            TaskStatus::Blocked => task.blocked_reason.as_deref() != Some(reason.as_str()),
            _ => false,
        };

        if should_block {
            set_task_blocked_with_reason(hub.clone(), &task, reason, None).await?;
            changed = changed.saturating_add(1);
        }
    }

    Ok(changed)
}

async fn reconcile_merge_gate_tasks_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
) -> Result<usize> {
    let workflows = hub.workflows().list().await.unwrap_or_default();
    let active_task_ids: HashSet<String> = workflows
        .into_iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            )
        })
        .map(|workflow| workflow.task_id)
        .collect();

    let mut resolved = 0usize;
    let tasks = hub.tasks().list().await?;
    for task in tasks {
        if active_task_ids.contains(&task.id) {
            continue;
        }
        if task.status != TaskStatus::Blocked || !is_merge_gate_block(&task) {
            continue;
        }

        let Some(branch_name) = task
            .branch_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            hub.tasks().set_status(&task.id, TaskStatus::Done).await?;
            resolved = resolved.saturating_add(1);
            continue;
        };

        match is_branch_merged(project_root, branch_name) {
            Ok(Some(true)) | Ok(None) => {
                hub.tasks().set_status(&task.id, TaskStatus::Done).await?;
                resolved = resolved.saturating_add(1);
            }
            Ok(Some(false)) | Err(_) => {}
        }
    }

    Ok(resolved)
}

async fn sync_task_status_for_workflow_result(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    task_id: &str,
    workflow_status: WorkflowStatus,
    workflow_id: Option<&str>,
) {
    match workflow_status {
        WorkflowStatus::Completed => {
            let task = hub.tasks().get(task_id).await;
            let Ok(task) = task else {
                let _ = hub.tasks().set_status(task_id, TaskStatus::Done).await;
                return;
            };

            match post_success_merge_push_and_cleanup(hub.clone(), project_root, &task).await {
                Ok(git_ops::PostMergeOutcome::Completed) => {
                    let _ = hub.tasks().set_status(task_id, TaskStatus::Done).await;
                    return;
                }
                Ok(git_ops::PostMergeOutcome::Skipped) => {}
                Ok(git_ops::PostMergeOutcome::Conflict { context }) => {
                    let conflict_summary = if context.conflicted_files.is_empty() {
                        "merge conflict detected".to_string()
                    } else {
                        format!(
                            "merge conflict detected in files: {}",
                            context.conflicted_files.join(", ")
                        )
                    };
                    if let Some(workflow_id) = workflow_id {
                        let _ = hub
                            .workflows()
                            .mark_merge_conflict(workflow_id, conflict_summary)
                            .await;
                    }

                    let recovery_result =
                        attempt_ai_merge_conflict_recovery(project_root, &task, &context).await;
                    if let Err(error) = recovery_result {
                        cleanup_merge_conflict_worktree(project_root, &context);
                        let _ = set_task_blocked_with_reason(
                            hub.clone(),
                            &task,
                            format!(
                                "{MERGE_GATE_PREFIX} auto-merge conflict recovery failed: {error}"
                            ),
                            None,
                        )
                        .await;
                        return;
                    }

                    match finalize_merge_conflict_resolution(
                        hub.clone(),
                        project_root,
                        &task,
                        &context,
                    )
                    .await
                    {
                        Ok(()) => {
                            if let Some(workflow_id) = workflow_id {
                                let _ = hub.workflows().resolve_merge_conflict(workflow_id).await;
                            }
                            let _ = hub.tasks().set_status(task_id, TaskStatus::Done).await;
                        }
                        Err(error) => {
                            cleanup_merge_conflict_worktree(project_root, &context);
                            let _ = set_task_blocked_with_reason(
                                hub.clone(),
                                &task,
                                format!(
                                    "{MERGE_GATE_PREFIX} merge conflict recovery finalize failed: {error}"
                                ),
                                None,
                            )
                            .await;
                        }
                    }
                    return;
                }
                Err(error) => {
                    if let Some(workflow_id) = workflow_id {
                        let _ = hub
                            .workflows()
                            .mark_merge_conflict(workflow_id, error.to_string())
                            .await;
                    }
                    let _ = set_task_blocked_with_reason(
                        hub.clone(),
                        &task,
                        format!("{MERGE_GATE_PREFIX} auto-merge failed: {error}"),
                        None,
                    )
                    .await;
                    return;
                }
            }

            let Some(branch_name) = task
                .branch_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                let _ = hub.tasks().set_status(task_id, TaskStatus::Done).await;
                return;
            };

            match is_branch_merged(project_root, branch_name) {
                Ok(Some(true)) | Ok(None) => {
                    let _ = hub.tasks().set_status(task_id, TaskStatus::Done).await;
                }
                Ok(Some(false)) => {
                    let _ = set_task_blocked_with_reason(
                        hub.clone(),
                        &task,
                        merge_blocked_reason(branch_name),
                        None,
                    )
                    .await;
                }
                Err(error) => {
                    let _ = set_task_blocked_with_reason(
                        hub.clone(),
                        &task,
                        format!("{MERGE_GATE_PREFIX} unable to verify merge status: {error}"),
                        None,
                    )
                    .await;
                }
            }
        }
        WorkflowStatus::Failed => {
            let _ = hub.tasks().set_status(task_id, TaskStatus::Blocked).await;
        }
        WorkflowStatus::Cancelled => {
            let _ = hub.tasks().set_status(task_id, TaskStatus::Cancelled).await;
        }
        WorkflowStatus::Paused | WorkflowStatus::Running | WorkflowStatus::Pending => {
            let _ = hub
                .tasks()
                .set_status(task_id, TaskStatus::InProgress)
                .await;
        }
    }
}

pub(super) async fn reconcile_stale_in_progress_tasks_for_project(
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
    let completed_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| is_terminally_completed_workflow(workflow))
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let failed_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Failed)
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let cancelled_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Cancelled)
        .map(|workflow| workflow.task_id.clone())
        .collect();

    let tasks = hub.tasks().list().await?;
    let mut reconciled = 0usize;
    let now = Utc::now();
    for task in tasks {
        if task.status != TaskStatus::InProgress {
            continue;
        }
        if active_task_ids.contains(&task.id) {
            continue;
        }
        if completed_task_ids.contains(&task.id) {
            sync_task_status_for_workflow_result(
                hub.clone(),
                project_root,
                &task.id,
                WorkflowStatus::Completed,
                None,
            )
            .await;
            reconciled = reconciled.saturating_add(1);
            continue;
        }
        if failed_task_ids.contains(&task.id) {
            hub.tasks()
                .set_status(&task.id, TaskStatus::Blocked)
                .await?;
            reconciled = reconciled.saturating_add(1);
            continue;
        }
        if cancelled_task_ids.contains(&task.id) {
            hub.tasks()
                .set_status(&task.id, TaskStatus::Cancelled)
                .await?;
            reconciled = reconciled.saturating_add(1);
            continue;
        }
        let age_minutes = now
            .signed_duration_since(task.metadata.updated_at)
            .num_minutes()
            .max(0);
        if age_minutes < 10 {
            continue;
        }
        hub.tasks().set_status(&task.id, TaskStatus::Ready).await?;
        reconciled = reconciled.saturating_add(1);
    }
    Ok(reconciled)
}

async fn resume_interrupted_workflows_for_project(
    hub: Arc<dyn ServiceHub>,
    root: &str,
) -> Result<(usize, usize)> {
    let resume_manager = WorkflowResumeManager::new(root)?;
    let cleaned = resume_manager.cleanup_stale_workflows()?;
    let resumable = resume_manager.get_resumable_workflows()?;

    let mut resumed = 0usize;
    for (workflow, _) in resumable {
        let updated = hub.workflows().resume(&workflow.id).await?;
        resumed = resumed.saturating_add(1);
        sync_task_status_for_workflow_result(
            hub.clone(),
            root,
            &updated.task_id,
            updated.status,
            Some(updated.id.as_str()),
        )
        .await;
    }

    Ok((cleaned, resumed))
}

fn em_scheduling_agent_enabled() -> bool {
    std::env::var(EM_SCHEDULING_AGENT_TOGGLE_ENV)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .map(|value| !matches!(value.as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(false)
}

fn priority_label(priority: orchestrator_core::Priority) -> &'static str {
    match priority {
        orchestrator_core::Priority::Critical => "critical",
        orchestrator_core::Priority::High => "high",
        orchestrator_core::Priority::Medium => "medium",
        orchestrator_core::Priority::Low => "low",
    }
}

fn risk_label(risk: orchestrator_core::RiskLevel) -> &'static str {
    match risk {
        orchestrator_core::RiskLevel::High => "high",
        orchestrator_core::RiskLevel::Medium => "medium",
        orchestrator_core::RiskLevel::Low => "low",
    }
}

fn complexity_label(complexity: orchestrator_core::Complexity) -> &'static str {
    match complexity {
        orchestrator_core::Complexity::High => "high",
        orchestrator_core::Complexity::Medium => "medium",
        orchestrator_core::Complexity::Low => "low",
    }
}

fn dependency_type_label(kind: DependencyType) -> &'static str {
    match kind {
        DependencyType::BlocksBy => "blocks_by",
        DependencyType::BlockedBy => "blocked_by",
        DependencyType::RelatedTo => "related_to",
    }
}

fn workflow_status_label(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Pending => "pending",
        WorkflowStatus::Running => "running",
        WorkflowStatus::Paused => "paused",
        WorkflowStatus::Completed => "completed",
        WorkflowStatus::Failed => "failed",
        WorkflowStatus::Cancelled => "cancelled",
    }
}

fn workflow_decision_action_label(
    action: orchestrator_core::WorkflowDecisionAction,
) -> &'static str {
    match action {
        orchestrator_core::WorkflowDecisionAction::Advance => "advance",
        orchestrator_core::WorkflowDecisionAction::Skip => "skip",
        orchestrator_core::WorkflowDecisionAction::Rework => "rework",
        orchestrator_core::WorkflowDecisionAction::Repeat => "repeat",
        orchestrator_core::WorkflowDecisionAction::Fail => "fail",
    }
}

fn workflow_decision_risk_label(risk: orchestrator_core::WorkflowDecisionRisk) -> &'static str {
    match risk {
        orchestrator_core::WorkflowDecisionRisk::Low => "low",
        orchestrator_core::WorkflowDecisionRisk::Medium => "medium",
        orchestrator_core::WorkflowDecisionRisk::High => "high",
    }
}

fn build_em_scheduling_prompt_input(
    tasks: &[orchestrator_core::OrchestratorTask],
    workflows: &[orchestrator_core::OrchestratorWorkflow],
    available_agent_slots: usize,
) -> EmSchedulingPromptInput {
    let mut requirement_coverage: std::collections::BTreeMap<
        String,
        EmSchedulingRequirementCoverage,
    > = std::collections::BTreeMap::new();

    let tasks = tasks
        .iter()
        .take(EM_SCHEDULING_MAX_TASKS)
        .map(|task| {
            for requirement_id in task
                .linked_requirements
                .iter()
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                let entry = requirement_coverage
                    .entry(requirement_id.to_string())
                    .or_insert_with(|| EmSchedulingRequirementCoverage {
                        requirement_id: requirement_id.to_string(),
                        ..EmSchedulingRequirementCoverage::default()
                    });
                entry.total_tasks = entry.total_tasks.saturating_add(1);
                match task.status {
                    TaskStatus::Ready | TaskStatus::Backlog => {
                        entry.ready_tasks = entry.ready_tasks.saturating_add(1);
                    }
                    TaskStatus::InProgress => {
                        entry.in_progress_tasks = entry.in_progress_tasks.saturating_add(1);
                    }
                    TaskStatus::Blocked | TaskStatus::OnHold => {
                        entry.blocked_tasks = entry.blocked_tasks.saturating_add(1);
                    }
                    TaskStatus::Done | TaskStatus::Cancelled => {
                        entry.done_tasks = entry.done_tasks.saturating_add(1);
                    }
                }
            }

            EmSchedulingPromptTask {
                task_id: task.id.clone(),
                title: task.title.clone(),
                status: task_status_label(task.status).to_string(),
                priority: priority_label(task.priority).to_string(),
                risk: risk_label(task.risk).to_string(),
                complexity: complexity_label(task.complexity).to_string(),
                paused: task.paused,
                cancelled: task.cancelled,
                blocked_reason: task
                    .blocked_reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
                linked_requirements: task
                    .linked_requirements
                    .iter()
                    .map(String::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect(),
                dependencies: task
                    .dependencies
                    .iter()
                    .map(|dependency| EmSchedulingPromptTaskDependency {
                        task_id: dependency.task_id.clone(),
                        dependency_type: dependency_type_label(dependency.dependency_type)
                            .to_string(),
                    })
                    .collect(),
            }
        })
        .collect();

    let running_workflows = workflows
        .iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            )
        })
        .map(|workflow| EmSchedulingPromptWorkflow {
            workflow_id: workflow.id.clone(),
            task_id: workflow.task_id.clone(),
            status: workflow_status_label(workflow.status).to_string(),
            current_phase: workflow_current_phase_id(workflow),
            failure_reason: workflow
                .failure_reason
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        })
        .collect();

    let mut recent_decisions = Vec::new();
    for workflow in workflows {
        for record in &workflow.decision_history {
            recent_decisions.push(EmSchedulingPromptDecisionRecord {
                workflow_id: workflow.id.clone(),
                task_id: workflow.task_id.clone(),
                phase_id: record.phase_id.clone(),
                decision: workflow_decision_action_label(record.decision).to_string(),
                risk: workflow_decision_risk_label(record.risk).to_string(),
                confidence: record.confidence,
                reason: record.reason.clone(),
                timestamp: record.timestamp.to_rfc3339(),
            });
        }
    }
    recent_decisions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    recent_decisions.truncate(EM_SCHEDULING_MAX_DECISIONS);

    let mut recent_failures: Vec<(i64, EmSchedulingPromptFailure)> = workflows
        .iter()
        .filter_map(|workflow| {
            let reason = workflow
                .failure_reason
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| {
                    if workflow.status == WorkflowStatus::Failed {
                        Some("workflow failed without explicit reason".to_string())
                    } else {
                        None
                    }
                })?;
            let completed_at = workflow
                .completed_at
                .map(|timestamp| timestamp.to_rfc3339());
            let timestamp_ms = workflow
                .completed_at
                .unwrap_or(workflow.started_at)
                .timestamp_millis();
            Some((
                timestamp_ms,
                EmSchedulingPromptFailure {
                    workflow_id: workflow.id.clone(),
                    task_id: workflow.task_id.clone(),
                    reason,
                    completed_at,
                },
            ))
        })
        .collect();
    recent_failures.sort_by(|left, right| right.0.cmp(&left.0));
    let recent_failures = recent_failures
        .into_iter()
        .map(|(_, failure)| failure)
        .take(EM_SCHEDULING_MAX_FAILURES)
        .collect();

    EmSchedulingPromptInput {
        available_agent_slots,
        tasks,
        running_workflows,
        recent_decisions,
        recent_failures,
        requirement_coverage: requirement_coverage.into_values().collect(),
    }
}

fn build_em_scheduling_prompt(input: &EmSchedulingPromptInput) -> String {
    let tasks_json =
        serde_json::to_string_pretty(&input.tasks).unwrap_or_else(|_| "[]".to_string());
    let running_workflows_json =
        serde_json::to_string_pretty(&input.running_workflows).unwrap_or_else(|_| "[]".to_string());
    let recent_decisions_json =
        serde_json::to_string_pretty(&input.recent_decisions).unwrap_or_else(|_| "[]".to_string());
    let recent_failures_json =
        serde_json::to_string_pretty(&input.recent_failures).unwrap_or_else(|_| "[]".to_string());
    let requirement_coverage_json = serde_json::to_string_pretty(&input.requirement_coverage)
        .unwrap_or_else(|_| "[]".to_string());

    EM_SCHEDULING_PROMPT_TEMPLATE
        .replace(
            "__AVAILABLE_AGENT_SLOTS__",
            &input.available_agent_slots.to_string(),
        )
        .replace("__TASK_LIST_JSON__", tasks_json.as_str())
        .replace(
            "__RUNNING_WORKFLOWS_JSON__",
            running_workflows_json.as_str(),
        )
        .replace("__RECENT_DECISIONS_JSON__", recent_decisions_json.as_str())
        .replace("__RECENT_FAILURES_JSON__", recent_failures_json.as_str())
        .replace(
            "__REQUIREMENT_COVERAGE_JSON__",
            requirement_coverage_json.as_str(),
        )
}

fn is_valid_em_scheduling_decision(decision: &EmSchedulingDecision) -> bool {
    if decision.kind.trim() != EM_SCHEDULING_RESULT_KIND {
        return false;
    }

    if decision
        .queue
        .iter()
        .any(|item| item.task_id.trim().is_empty() || item.reason.trim().is_empty())
    {
        return false;
    }

    if decision
        .defer
        .iter()
        .any(|task_id| task_id.trim().is_empty())
    {
        return false;
    }
    if decision
        .preempt
        .iter()
        .any(|task_id| task_id.trim().is_empty())
    {
        return false;
    }

    true
}

fn parse_em_scheduling_decision_from_payload(payload: &Value) -> Option<EmSchedulingDecision> {
    match payload {
        Value::Array(items) => {
            let mut parsed_decision = None;
            for item in items {
                if let Some(decision) = parse_em_scheduling_decision_from_payload(item) {
                    parsed_decision = Some(decision);
                }
            }
            parsed_decision
        }
        Value::Object(object) => {
            if let Ok(decision) = serde_json::from_value::<EmSchedulingDecision>(payload.clone()) {
                if is_valid_em_scheduling_decision(&decision) {
                    return Some(decision);
                }
            }

            for key in ["proposal", "data", "payload", "result", "output", "item"] {
                if let Some(value) = object.get(key) {
                    if let Some(decision) = parse_em_scheduling_decision_from_payload(value) {
                        return Some(decision);
                    }
                }
            }

            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(nested_payload) = serde_json::from_str::<Value>(trimmed) {
                        if let Some(decision) =
                            parse_em_scheduling_decision_from_payload(&nested_payload)
                        {
                            return Some(decision);
                        }
                    }
                }
            }

            None
        }
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Ok(payload) = serde_json::from_str::<Value>(trimmed) {
                return parse_em_scheduling_decision_from_payload(&payload);
            }
            None
        }
        _ => None,
    }
}

fn parse_em_scheduling_decision_response(text: &str) -> Option<EmSchedulingDecision> {
    let mut parsed_decision = None;
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(decision) = parse_em_scheduling_decision_from_payload(&payload) {
            parsed_decision = Some(decision);
        }
    }
    if parsed_decision.is_some() {
        return parsed_decision;
    }

    if let Ok(payload) = serde_json::from_str::<Value>(text.trim()) {
        return parse_em_scheduling_decision_from_payload(&payload);
    }
    None
}

fn normalize_em_scheduling_decision(
    decision: EmSchedulingDecision,
    known_task_ids: &HashSet<String>,
    running_task_ids: &HashSet<String>,
    available_agent_slots: usize,
) -> EmSchedulingAppliedDecision {
    let mut preempt_seen = HashSet::new();
    let preempt_task_ids: Vec<String> = decision
        .preempt
        .into_iter()
        .filter_map(|task_id| normalize_optional_id(Some(task_id.as_str())))
        .filter(|task_id| running_task_ids.contains(task_id))
        .filter(|task_id| preempt_seen.insert(task_id.clone()))
        .collect();

    let preempt_lookup: HashSet<String> = preempt_task_ids.iter().cloned().collect();
    let mut defer_seen = HashSet::new();
    let defer_task_ids: Vec<String> = decision
        .defer
        .into_iter()
        .filter_map(|task_id| normalize_optional_id(Some(task_id.as_str())))
        .filter(|task_id| known_task_ids.contains(task_id))
        .filter(|task_id| !preempt_lookup.contains(task_id))
        .filter(|task_id| defer_seen.insert(task_id.clone()))
        .collect();

    let defer_lookup: HashSet<String> = defer_task_ids.iter().cloned().collect();
    let mut queue_seen = HashSet::new();
    let mut queue_task_ids: Vec<String> = decision
        .queue
        .into_iter()
        .filter_map(|item| {
            let task_id = normalize_optional_id(Some(item.task_id.as_str()))?;
            if item.reason.trim().is_empty() {
                return None;
            }
            if !known_task_ids.contains(&task_id) {
                return None;
            }
            if defer_lookup.contains(&task_id) || preempt_lookup.contains(&task_id) {
                return None;
            }
            if !queue_seen.insert(task_id.clone()) {
                return None;
            }
            Some(task_id)
        })
        .collect();
    let max_queue_len = available_agent_slots.saturating_add(preempt_task_ids.len());
    queue_task_ids.truncate(max_queue_len);

    EmSchedulingAppliedDecision {
        queue_task_ids,
        defer_task_ids,
        preempt_task_ids,
    }
}

async fn apply_em_scheduling_preemptions(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    workflows: &[orchestrator_core::OrchestratorWorkflow],
    preempt_task_ids: &[String],
) {
    if preempt_task_ids.is_empty() {
        return;
    }

    let preempt_lookup: HashSet<String> = preempt_task_ids.iter().cloned().collect();
    for workflow in workflows {
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        if !preempt_lookup.contains(workflow.task_id.as_str()) {
            continue;
        }

        match hub.workflows().pause(&workflow.id).await {
            Ok(updated) => {
                sync_task_status_for_workflow_result(
                    hub.clone(),
                    project_root,
                    &updated.task_id,
                    updated.status,
                    Some(updated.id.as_str()),
                )
                .await;
            }
            Err(error) => {
                eprintln!(
                    "ao-daemon: failed to preempt workflow {} for task {}: {}",
                    workflow.id, workflow.task_id, error
                );
            }
        }
    }
}

async fn run_em_scheduling_prompt_against_runner(
    project_root: &str,
    prompt: &str,
    model: &str,
    tool: &str,
    timeout_secs: u64,
) -> Result<String> {
    let run_id = RunId(format!("em-scheduling-{}", Uuid::new_v4()));
    let mut context = serde_json::json!({
        "tool": tool,
        "prompt": prompt,
        "cwd": project_root,
        "project_root": project_root,
        "planning_stage": "em-scheduling",
        "allowed_tools": ["Read", "Glob", "Grep"],
        "timeout_secs": timeout_secs,
    });
    if let Some(runtime_contract) = build_runtime_contract(tool, model, prompt) {
        context["runtime_contract"] = runtime_contract;
    }

    let request = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run_id.clone(),
        model: ModelId(model.to_string()),
        context,
        timeout_secs: Some(timeout_secs),
    };

    let config_dir = runner_config_dir(Path::new(project_root));
    let stream = connect_runner(&config_dir).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut transcript = String::new();
    let mut finished_successfully = false;
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, &run_id) {
            continue;
        }

        match event {
            AgentRunEvent::OutputChunk { text, .. } => {
                transcript.push_str(&text);
                transcript.push('\n');
            }
            AgentRunEvent::Thinking { content, .. } => {
                transcript.push_str(&content);
                transcript.push('\n');
            }
            AgentRunEvent::Error { error, .. } => {
                anyhow::bail!("em scheduling run failed: {error}");
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    anyhow::bail!(
                        "em scheduling run exited with non-zero code: {:?}",
                        exit_code
                    );
                }
                finished_successfully = true;
                break;
            }
            _ => {}
        }
    }

    if !finished_successfully {
        anyhow::bail!("runner disconnected before em scheduling completed");
    }
    if transcript.trim().is_empty() {
        anyhow::bail!("em scheduling run produced empty output");
    }

    Ok(transcript)
}

async fn em_scheduling_decision_for_ready_tasks(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    max_tasks_per_tick: usize,
    workflows: &[orchestrator_core::OrchestratorWorkflow],
    tasks: &[orchestrator_core::OrchestratorTask],
) -> Result<Option<EmSchedulingAppliedDecision>> {
    if max_tasks_per_tick == 0 || tasks.is_empty() {
        return Ok(None);
    }

    let prompt_input = build_em_scheduling_prompt_input(tasks, workflows, max_tasks_per_tick);
    let prompt = build_em_scheduling_prompt(&prompt_input);
    let model = default_primary_model_for_phase("requirements", None).to_string();
    let tool = tool_for_model_id(&model).to_string();
    let transcript = run_em_scheduling_prompt_against_runner(
        project_root,
        &prompt,
        &model,
        &tool,
        EM_SCHEDULING_TIMEOUT_SECS,
    )
    .await?;

    let decision = parse_em_scheduling_decision_response(&transcript)
        .ok_or_else(|| anyhow!("EM scheduling output was not parseable JSON"))?;
    let known_task_ids: HashSet<String> = tasks.iter().map(|task| task.id.clone()).collect();
    let running_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Running)
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let applied_decision = normalize_em_scheduling_decision(
        decision,
        &known_task_ids,
        &running_task_ids,
        max_tasks_per_tick,
    );

    if applied_decision.queue_task_ids.is_empty()
        && applied_decision.defer_task_ids.is_empty()
        && applied_decision.preempt_task_ids.is_empty()
    {
        return Ok(None);
    }

    apply_em_scheduling_preemptions(
        hub,
        project_root,
        workflows,
        &applied_decision.preempt_task_ids,
    )
    .await;

    Ok(Some(applied_decision))
}

pub(super) async fn run_ready_task_workflows_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    max_tasks_per_tick: usize,
) -> Result<usize> {
    if max_tasks_per_tick == 0 {
        return Ok(0);
    }

    let workflows = hub.workflows().list().await.unwrap_or_default();
    let mut active_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused | WorkflowStatus::Pending
            )
        })
        .map(|workflow| workflow.task_id.clone())
        .collect();
    let completed_task_ids: HashSet<String> = workflows
        .iter()
        .filter(|workflow| is_terminally_completed_workflow(workflow))
        .map(|workflow| workflow.task_id.clone())
        .collect();

    let mut started = 0usize;
    let candidates = hub.tasks().list_prioritized().await?;
    let mut scheduled_candidates = candidates.clone();
    if em_scheduling_agent_enabled() {
        match em_scheduling_decision_for_ready_tasks(
            hub.clone(),
            project_root,
            max_tasks_per_tick,
            &workflows,
            &candidates,
        )
        .await
        {
            Ok(Some(decision)) => {
                let lookup: std::collections::HashMap<String, orchestrator_core::OrchestratorTask> =
                    candidates
                        .iter()
                        .cloned()
                        .map(|task| (task.id.clone(), task))
                        .collect();
                scheduled_candidates = decision
                    .queue_task_ids
                    .iter()
                    .filter_map(|task_id| lookup.get(task_id).cloned())
                    .collect();
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!(
                    "ao-daemon: failed to compute EM scheduling decision: {}",
                    error
                );
            }
        }
    }

    for task in scheduled_candidates {
        if started >= max_tasks_per_tick {
            break;
        }
        if task.paused || task.cancelled {
            continue;
        }
        if task.status != TaskStatus::Ready {
            continue;
        }
        if active_task_ids.contains(&task.id) {
            continue;
        }
        if completed_task_ids.contains(&task.id) {
            let _ = hub.tasks().set_status(&task.id, TaskStatus::Done).await;
            continue;
        }
        let dependency_issues =
            dependency_gate_issues_for_task(hub.clone(), project_root, &task).await;
        if !dependency_issues.is_empty() {
            let reason = dependency_blocked_reason(&dependency_issues);
            let _ = set_task_blocked_with_reason(hub.clone(), &task, reason, None).await;
            continue;
        }

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: Some(pipeline_for_task(&task)),
            })
            .await?;
        auto_assign_task_to_daemon_agent(hub.clone(), project_root, &task, &workflow).await?;
        sync_task_status_for_workflow_result(
            hub.clone(),
            project_root,
            &task.id,
            workflow.status,
            Some(workflow.id.as_str()),
        )
        .await;
        active_task_ids.insert(task.id.clone());
        started = started.saturating_add(1);
    }

    Ok(started)
}

async fn execute_running_workflow_phases_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    max_phases_per_tick: usize,
) -> Result<(usize, usize, Vec<PhaseExecutionEvent>)> {
    if max_phases_per_tick == 0 {
        return Ok((0, 0, Vec::new()));
    }

    struct ScheduledPhaseRun {
        workflow: orchestrator_core::OrchestratorWorkflow,
        task: orchestrator_core::OrchestratorTask,
        phase_id: String,
        phase_attempt: u32,
        execution_cwd: String,
    }

    let workflows = hub.workflows().list().await.unwrap_or_default();
    let mut executed = 0usize;
    let mut failed = 0usize;
    let mut processed = 0usize;
    let mut phase_events = Vec::new();
    let mut scheduled_runs: Vec<ScheduledPhaseRun> = Vec::new();

    for workflow in workflows {
        if processed >= max_phases_per_tick {
            break;
        }
        if workflow.status != WorkflowStatus::Running {
            continue;
        }

        let phase_id = workflow
            .current_phase
            .clone()
            .or_else(|| {
                workflow
                    .phases
                    .get(workflow.current_phase_index)
                    .map(|phase| phase.phase_id.clone())
            })
            .unwrap_or_else(|| "unknown".to_string());
        let phase_attempt = workflow
            .phases
            .get(workflow.current_phase_index)
            .map(|phase| phase.attempt.max(1))
            .unwrap_or(1);

        let task = match hub.tasks().get(&workflow.task_id).await {
            Ok(task) => task,
            Err(error) => {
                let error_message = format!(
                    "workflow {} cannot load task {}: {}",
                    workflow.id, workflow.task_id, error
                );
                if PhaseFailureClassifier::is_transient_runner_error_message(&error_message) {
                    processed = processed.saturating_add(1);
                    continue;
                }
                let updated = hub
                    .workflows()
                    .fail_current_phase(&workflow.id, error_message)
                    .await?;
                sync_task_status_for_workflow_result(
                    hub.clone(),
                    project_root,
                    &updated.task_id,
                    updated.status,
                    Some(updated.id.as_str()),
                )
                .await;
                failed = failed.saturating_add(1);
                processed = processed.saturating_add(1);
                continue;
            }
        };

        if phase_id != "research"
            && task_requires_research(&task)
            && !workflow_has_completed_research(&workflow)
            && !workflow_has_active_research(&workflow)
        {
            let reason =
                "requirements validation requested research evidence before execution".to_string();
            let updated = hub
                .workflows()
                .request_research(&workflow.id, reason)
                .await?;
            sync_task_status_for_workflow_result(
                hub.clone(),
                project_root,
                &updated.task_id,
                updated.status,
                Some(updated.id.as_str()),
            )
            .await;
            processed = processed.saturating_add(1);
            continue;
        }

        let execution_cwd = ensure_task_execution_cwd(hub.clone(), project_root, &task)
            .await
            .unwrap_or_else(|_| project_root.to_string());
        scheduled_runs.push(ScheduledPhaseRun {
            workflow,
            task,
            phase_id,
            phase_attempt,
            execution_cwd,
        });
        processed = processed.saturating_add(1);
    }

    let mut join_set = tokio::task::JoinSet::new();
    for scheduled in scheduled_runs {
        let project_root = project_root.to_string();
        join_set.spawn(async move {
            let run_result = run_workflow_phase_with_agent(
                &project_root,
                &scheduled.execution_cwd,
                &scheduled.workflow.id,
                &scheduled.workflow.task_id,
                &scheduled.task.title,
                &scheduled.task.description,
                Some(scheduled.task.complexity),
                &scheduled.phase_id,
                scheduled.phase_attempt,
            )
            .await
            .and_then(|result| {
                append_phase_execution_metadata_artifact(
                    &project_root,
                    &scheduled.workflow.id,
                    &result,
                )?;
                Ok(result)
            });

            (
                scheduled.workflow,
                scheduled.task,
                scheduled.phase_id,
                run_result,
            )
        });
    }

    while let Some(joined) = join_set.join_next().await {
        let (workflow, task, phase_id, run_result) = match joined {
            Ok(value) => value,
            Err(_) => {
                failed = failed.saturating_add(1);
                continue;
            }
        };

        match run_result {
            Ok(result) => {
                phase_events.extend(phase_execution_events_from_signals(
                    project_root,
                    &workflow,
                    &result.metadata,
                    &result.signals,
                ));

                match result.outcome {
                    PhaseExecutionOutcome::Completed { phase_decision, .. } => {
                        enforce_frontend_phase_gate(project_root, &workflow.id, &phase_id, &task)?;
                        let updated = hub
                            .workflows()
                            .complete_current_phase_with_decision(&workflow.id, phase_decision)
                            .await?;
                        sync_task_status_for_workflow_result(
                            hub.clone(),
                            project_root,
                            &updated.task_id,
                            updated.status,
                            Some(updated.id.as_str()),
                        )
                        .await;
                        executed = executed.saturating_add(1);
                    }
                    PhaseExecutionOutcome::NeedsResearch { reason } => {
                        if phase_id == "research" {
                            let updated = hub
                                .workflows()
                                .fail_current_phase(
                                    &workflow.id,
                                    format!(
                                        "research phase requested additional research: {reason}"
                                    ),
                                )
                                .await?;
                            sync_task_status_for_workflow_result(
                                hub.clone(),
                                project_root,
                                &updated.task_id,
                                updated.status,
                                Some(updated.id.as_str()),
                            )
                            .await;
                            failed = failed.saturating_add(1);
                        } else {
                            let prior_research_rework =
                                workflow.decision_history.iter().any(|record| {
                                    record.phase_id == phase_id
                                        && record.decision
                                            == orchestrator_core::WorkflowDecisionAction::Rework
                                        && record.target_phase.as_deref() == Some("research")
                                });

                            let updated = if prior_research_rework {
                                hub.workflows().complete_current_phase(&workflow.id).await?
                            } else {
                                hub.workflows()
                                    .request_research(&workflow.id, reason)
                                    .await?
                            };
                            sync_task_status_for_workflow_result(
                                hub.clone(),
                                project_root,
                                &updated.task_id,
                                updated.status,
                                Some(updated.id.as_str()),
                            )
                            .await;
                            if prior_research_rework {
                                executed = executed.saturating_add(1);
                            }
                        }
                    }
                    PhaseExecutionOutcome::ManualPending { .. } => {
                        // Manual mode waits for explicit CLI approval.
                    }
                }
            }
            Err(error) => {
                let error_message = error.to_string();
                if error_message.contains("contract violation")
                    || error_message.contains("schema validation failed")
                    || error_message.contains("payload kind mismatch")
                {
                    phase_events.push(PhaseExecutionEvent {
                        event_type: "workflow-phase-contract-violation".to_string(),
                        project_root: project_root.to_string(),
                        workflow_id: workflow.id.clone(),
                        task_id: workflow.task_id.clone(),
                        phase_id: phase_id.clone(),
                        phase_mode: "unknown".to_string(),
                        metadata: PhaseExecutionMetadata {
                            phase_id: phase_id.clone(),
                            phase_mode: "unknown".to_string(),
                            phase_definition_hash: "unknown".to_string(),
                            agent_runtime_config_hash: "unknown".to_string(),
                            agent_runtime_schema: orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_SCHEMA_ID
                                .to_string(),
                            agent_runtime_version:
                                orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_VERSION,
                            agent_runtime_source: "unknown".to_string(),
                            agent_id: None,
                            agent_profile_hash: None,
                            selected_tool: None,
                            selected_model: None,
                        },
                        payload: serde_json::json!({
                            "workflow_id": workflow.id,
                            "task_id": workflow.task_id,
                            "phase_id": phase_id,
                            "error": error_message,
                        }),
                    });
                }
                if PhaseFailureClassifier::is_transient_runner_error_message(&error_message) {
                    continue;
                }

                let recovery = attempt_ai_failure_recovery(
                    project_root,
                    &task,
                    &phase_id,
                    &error_message,
                    &workflow.decision_history,
                )
                .await;

                match recovery {
                    AiRecoveryAction::Retry => {
                        continue;
                    }
                    AiRecoveryAction::SkipPhase => {
                        let updated = hub.workflows().complete_current_phase(&workflow.id).await?;
                        sync_task_status_for_workflow_result(
                            hub.clone(),
                            project_root,
                            &updated.task_id,
                            updated.status,
                            Some(updated.id.as_str()),
                        )
                        .await;
                        executed = executed.saturating_add(1);
                    }
                    AiRecoveryAction::Decompose(subtasks) => {
                        let linked_requirements = task.linked_requirements.clone();
                        for subtask_def in subtasks {
                            let _ = hub
                                .tasks()
                                .create(TaskCreateInput {
                                    title: subtask_def.title,
                                    description: subtask_def.description,
                                    task_type: Some(TaskType::Feature),
                                    priority: Some(task.priority),
                                    created_by: Some(AI_RECOVERY_MARKER.to_string()),
                                    tags: vec![
                                        "ai-decomposed".to_string(),
                                        "ai-generated".to_string(),
                                    ],
                                    linked_requirements: linked_requirements.clone(),
                                    linked_architecture_entities: Vec::new(),
                                })
                                .await;
                        }
                        let fail_reason = format!(
                            "{AI_RECOVERY_MARKER}: decomposed into subtasks — {}",
                            error_message
                        );
                        let updated = hub
                            .workflows()
                            .fail_current_phase(&workflow.id, fail_reason)
                            .await?;
                        sync_task_status_for_workflow_result(
                            hub.clone(),
                            project_root,
                            &updated.task_id,
                            updated.status,
                            Some(updated.id.as_str()),
                        )
                        .await;
                        failed = failed.saturating_add(1);
                    }
                    AiRecoveryAction::Fail => {
                        let updated = hub
                            .workflows()
                            .fail_current_phase(&workflow.id, error_message)
                            .await?;
                        sync_task_status_for_workflow_result(
                            hub.clone(),
                            project_root,
                            &updated.task_id,
                            updated.status,
                            Some(updated.id.as_str()),
                        )
                        .await;
                        failed = failed.saturating_add(1);
                    }
                }
            }
        }
    }

    Ok((executed, failed, phase_events))
}

fn phase_execution_events_from_signals(
    project_root: &str,
    workflow: &orchestrator_core::OrchestratorWorkflow,
    metadata: &PhaseExecutionMetadata,
    signals: &[PhaseExecutionSignal],
) -> Vec<PhaseExecutionEvent> {
    signals
        .iter()
        .map(|signal| PhaseExecutionEvent {
            event_type: signal.event_type.clone(),
            project_root: project_root.to_string(),
            workflow_id: workflow.id.clone(),
            task_id: workflow.task_id.clone(),
            phase_id: metadata.phase_id.clone(),
            phase_mode: metadata.phase_mode.clone(),
            metadata: metadata.clone(),
            payload: signal.payload.clone(),
        })
        .collect()
}

fn phase_execution_metadata_artifact_path(project_root: &str, run_id: &str) -> PathBuf {
    Path::new(project_root)
        .join(".ao")
        .join("runs")
        .join(run_id)
        .join("phase-exec-metadata.json")
}

fn append_phase_execution_metadata_artifact(
    project_root: &str,
    run_id: &str,
    run_result: &PhaseExecutionRunResult,
) -> Result<()> {
    let path = phase_execution_metadata_artifact_path(project_root, run_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut existing = if path.exists() {
        serde_json::from_str::<Value>(&std::fs::read_to_string(&path)?)
            .unwrap_or_else(|_| serde_json::json!({ "entries": [] }))
    } else {
        serde_json::json!({ "entries": [] })
    };

    let entry = serde_json::json!({
        "timestamp": Utc::now().to_rfc3339(),
        "phase_id": run_result.metadata.phase_id,
        "phase_mode": run_result.metadata.phase_mode,
        "metadata": run_result.metadata,
        "signals": run_result.signals,
        "outcome": run_result.outcome,
    });

    if let Some(entries) = existing.get_mut("entries").and_then(Value::as_array_mut) {
        entries.push(entry);
    } else {
        existing = serde_json::json!({ "entries": [entry] });
    }

    let payload = serde_json::to_string_pretty(&existing)?;
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("phase-exec-metadata"),
        Uuid::new_v4()
    ));
    std::fs::write(&tmp_path, payload)?;
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}

pub(super) async fn bootstrap_from_vision_if_needed(
    hub: Arc<dyn ServiceHub>,
    include_codebase_scan: bool,
    ai_task_generation: bool,
) -> Result<()> {
    let planning = hub.planning();
    let Some(vision) = planning.get_vision().await? else {
        return Ok(());
    };

    // Preserve explicit operator scope once tasks exist. Auto-bootstrap should only
    // materialize requirements when the project has not produced any tasks yet.
    let tasks = hub.tasks().list().await?;
    if !tasks.is_empty() {
        return Ok(());
    }

    let mut requirements = planning.list_requirements().await?;
    if requirements.is_empty() {
        let draft = planning
            .draft_requirements(RequirementsDraftInput {
                include_codebase_scan,
                append_only: true,
                max_requirements: bootstrap_max_requirements(),
            })
            .await?;
        requirements = draft.requirements;
    }

    if requirements.is_empty() {
        return Ok(());
    }

    if requirements.iter().any(requirement_needs_refinement) {
        let requirement_ids = requirements
            .iter()
            .map(|requirement| requirement.id.clone())
            .collect();
        planning
            .refine_requirements(RequirementsRefineInput {
                requirement_ids,
                focus: Some(
                    "Production-quality scope with measurable outcomes, QA gates, and delivery readiness."
                        .to_string(),
                ),
            })
            .await?;
        requirements = planning.list_requirements().await?;
    }

    let mut requirement_ids: Vec<String> = requirements
        .iter()
        .filter(|requirement| !requirement.source.eq_ignore_ascii_case("baseline"))
        .map(|requirement| requirement.id.clone())
        .collect();
    if requirement_ids.is_empty() {
        requirement_ids = requirements
            .iter()
            .map(|requirement| requirement.id.clone())
            .collect();
    }
    if ai_task_generation {
        ensure_ai_generated_tasks_for_requirements(
            hub.clone(),
            &vision.project_root,
            &requirement_ids,
        )
        .await?;
    }
    planning
        .execute_requirements(RequirementsExecutionInput {
            requirement_ids,
            start_workflows: false,
            pipeline_id: None,
            include_wont: false,
        })
        .await?;

    Ok(())
}

async fn ensure_tasks_for_unplanned_requirements(
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

async fn promote_backlog_tasks_to_ready(
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

async fn retry_failed_task_workflows(hub: Arc<dyn ServiceHub>) -> Result<usize> {
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

const AI_RECOVERY_TIMEOUT_SECS: u64 = 120;
const AI_RECOVERY_MARKER: &str = "ai-failure-recovery";
const MAX_DECOMPOSE_SUBTASKS: usize = 3;

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct AiRecoveryResponse {
    action: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    subtasks: Vec<AiRecoverySubtask>,
}

#[derive(Debug, Clone, Deserialize)]
struct AiRecoverySubtask {
    title: String,
    #[serde(default)]
    description: String,
}

enum AiRecoveryAction {
    Retry,
    Decompose(Vec<AiRecoverySubtask>),
    SkipPhase,
    Fail,
}

async fn attempt_ai_failure_recovery(
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    phase_id: &str,
    error_message: &str,
    decision_history: &[orchestrator_core::WorkflowDecisionRecord],
) -> AiRecoveryAction {
    let already_attempted = decision_history
        .iter()
        .any(|record| record.phase_id == phase_id && record.reason.contains(AI_RECOVERY_MARKER));
    if already_attempted {
        return AiRecoveryAction::Fail;
    }

    let model = default_primary_model_for_phase("implementation", None).to_string();
    let tool = tool_for_model_id(&model).to_string();

    let prompt = format!(
        r#"A workflow phase has failed. Analyze the error and recommend a recovery action.

## Task
- Title: {title}
- Description: {description}

## Failed Phase
- Phase ID: {phase_id}
- Error: {error}

## Instructions
Return exactly one JSON object with your recommendation:
{{
  "action": "retry|decompose|skip_phase|fail",
  "reason": "Brief explanation of your recommendation",
  "subtasks": [
    {{"title": "Subtask title", "description": "Subtask description"}}
  ]
}}

Rules:
- "retry" — the error is transient or environmental, retrying might succeed
- "decompose" — the task is too complex, break it into smaller subtasks (max 3)
- "skip_phase" — the phase is non-critical and can be skipped safely
- "fail" — the error is fundamental and cannot be recovered
- Only include "subtasks" if action is "decompose"
- Output valid JSON only, no markdown fences"#,
        title = task.title,
        description = task.description.chars().take(1000).collect::<String>(),
        phase_id = phase_id,
        error = error_message.chars().take(500).collect::<String>(),
    );

    let result = run_prompt_against_runner(
        project_root,
        &prompt,
        &model,
        &tool,
        AI_RECOVERY_TIMEOUT_SECS,
    )
    .await;

    let Ok(transcript) = result else {
        return AiRecoveryAction::Fail;
    };

    let parsed = parse_ai_recovery_response(&transcript);
    let Some(response) = parsed else {
        return AiRecoveryAction::Fail;
    };

    match response.action.trim().to_ascii_lowercase().as_str() {
        "retry" => AiRecoveryAction::Retry,
        "decompose" if !response.subtasks.is_empty() => {
            let subtasks: Vec<_> = response
                .subtasks
                .into_iter()
                .filter(|s| !s.title.trim().is_empty())
                .take(MAX_DECOMPOSE_SUBTASKS)
                .collect();
            if subtasks.is_empty() {
                AiRecoveryAction::Fail
            } else {
                AiRecoveryAction::Decompose(subtasks)
            }
        }
        "skip_phase" => AiRecoveryAction::SkipPhase,
        _ => AiRecoveryAction::Fail,
    }
}

fn parse_ai_recovery_response(text: &str) -> Option<AiRecoveryResponse> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Ok(response) = serde_json::from_value::<AiRecoveryResponse>(payload) {
            if !response.action.is_empty() {
                return Some(response);
            }
        }
    }
    if let Ok(response) = serde_json::from_str::<AiRecoveryResponse>(text.trim()) {
        if !response.action.is_empty() {
            return Some(response);
        }
    }
    None
}

const MERGE_CONFLICT_RECOVERY_TIMEOUT_SECS: u64 = 300;
const MERGE_CONFLICT_RECOVERY_RESULT_KIND: &str = "merge_conflict_resolution_result";
const MERGE_CONFLICT_RECOVERY_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/runtime/merge_conflict_recovery.prompt"
));

#[derive(Debug, Clone, Deserialize)]
struct MergeConflictRecoveryResponse {
    kind: String,
    status: String,
    #[serde(default)]
    commit_message: String,
    #[serde(default)]
    reason: String,
}

fn build_merge_conflict_recovery_prompt(
    task: &orchestrator_core::OrchestratorTask,
    context: &git_ops::MergeConflictContext,
) -> String {
    let conflicted_files = if context.conflicted_files.is_empty() {
        "- (none detected by git)".to_string()
    } else {
        context
            .conflicted_files
            .iter()
            .map(|path| format!("- {}", path))
            .collect::<Vec<_>>()
            .join("\n")
    };

    MERGE_CONFLICT_RECOVERY_PROMPT_TEMPLATE
        .replace("__TASK_TITLE__", task.title.trim())
        .replace(
            "__TASK_DESCRIPTION__",
            task.description
                .chars()
                .take(2000)
                .collect::<String>()
                .as_str(),
        )
        .replace("__SOURCE_BRANCH__", context.source_branch.as_str())
        .replace("__TARGET_BRANCH__", context.target_branch.as_str())
        .replace(
            "__MERGE_WORKTREE_PATH__",
            context.merge_worktree_path.as_str(),
        )
        .replace("__CONFLICTED_FILES__", conflicted_files.as_str())
}

async fn run_merge_conflict_recovery_prompt_against_runner(
    project_root: &str,
    execution_cwd: &str,
    prompt: &str,
    model: &str,
    tool: &str,
    timeout_secs: u64,
) -> Result<String> {
    let run_id = RunId(format!("merge-conflict-recovery-{}", Uuid::new_v4()));
    let mut context = serde_json::json!({
        "tool": tool,
        "prompt": prompt,
        "cwd": execution_cwd,
        "project_root": project_root,
        "planning_stage": "merge-conflict-recovery",
        "allowed_tools": ["Read", "Glob", "Grep", "Edit", "Write", "Bash"],
        "timeout_secs": timeout_secs,
    });
    if let Some(runtime_contract) = build_runtime_contract(tool, model, prompt) {
        context["runtime_contract"] = runtime_contract;
    }

    let request = AgentRunRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: run_id.clone(),
        model: ModelId(model.to_string()),
        context,
        timeout_secs: Some(timeout_secs),
    };

    let config_dir = runner_config_dir(Path::new(project_root));
    let stream = connect_runner(&config_dir).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_json_line(&mut write_half, &request).await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut transcript = String::new();
    let mut finished_successfully = false;
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, &run_id) {
            continue;
        }

        match event {
            AgentRunEvent::OutputChunk { text, .. } => {
                transcript.push_str(&text);
                transcript.push('\n');
            }
            AgentRunEvent::Thinking { content, .. } => {
                transcript.push_str(&content);
                transcript.push('\n');
            }
            AgentRunEvent::Error { error, .. } => {
                anyhow::bail!("merge conflict recovery run failed: {error}");
            }
            AgentRunEvent::Finished { exit_code, .. } => {
                if exit_code.unwrap_or_default() != 0 {
                    anyhow::bail!(
                        "merge conflict recovery run exited with non-zero code: {:?}",
                        exit_code
                    );
                }
                finished_successfully = true;
                break;
            }
            _ => {}
        }
    }

    if !finished_successfully {
        anyhow::bail!("runner disconnected before merge conflict recovery completed");
    }

    if transcript.trim().is_empty() {
        anyhow::bail!("merge conflict recovery run produced empty output");
    }

    Ok(transcript)
}

async fn attempt_ai_merge_conflict_recovery(
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    context: &git_ops::MergeConflictContext,
) -> Result<()> {
    let model = default_primary_model_for_phase("implementation", None).to_string();
    let tool = tool_for_model_id(&model).to_string();
    let prompt = build_merge_conflict_recovery_prompt(task, context);
    let transcript = run_merge_conflict_recovery_prompt_against_runner(
        project_root,
        context.merge_worktree_path.as_str(),
        &prompt,
        &model,
        &tool,
        MERGE_CONFLICT_RECOVERY_TIMEOUT_SECS,
    )
    .await?;

    let response = parse_merge_conflict_recovery_response(&transcript)
        .ok_or_else(|| anyhow!("merge conflict recovery output was not parseable JSON"))?;

    let status = merge_conflict_recovery_status(response.status.as_str())
        .ok_or_else(|| anyhow!("merge conflict recovery output has invalid status"))?;

    match status {
        "resolved" => {
            if response.commit_message.trim().is_empty() {
                anyhow::bail!("merge conflict recovery output is missing non-empty commit_message");
            }
            run_cargo_check(context.merge_worktree_path.as_str())?;
            Ok(())
        }
        "failed" => {
            let reason = response.reason.trim();
            if reason.is_empty() {
                anyhow::bail!("merge conflict recovery agent reported failure");
            }
            anyhow::bail!("merge conflict recovery agent reported failure: {reason}");
        }
        _ => anyhow::bail!("merge conflict recovery output has invalid status"),
    }
}

fn merge_conflict_recovery_status(status: &str) -> Option<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "resolved" => Some("resolved"),
        "failed" => Some("failed"),
        _ => None,
    }
}

fn is_valid_merge_conflict_recovery_response(response: &MergeConflictRecoveryResponse) -> bool {
    response
        .kind
        .trim()
        .eq_ignore_ascii_case(MERGE_CONFLICT_RECOVERY_RESULT_KIND)
        && merge_conflict_recovery_status(response.status.as_str()).is_some()
}

fn parse_merge_conflict_recovery_response(text: &str) -> Option<MergeConflictRecoveryResponse> {
    let mut parsed_response = None;
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Ok(response) = serde_json::from_value::<MergeConflictRecoveryResponse>(payload) {
            if is_valid_merge_conflict_recovery_response(&response) {
                parsed_response = Some(response);
            }
        }
    }
    if parsed_response.is_some() {
        return parsed_response;
    }

    if let Ok(response) = serde_json::from_str::<MergeConflictRecoveryResponse>(text.trim()) {
        if is_valid_merge_conflict_recovery_response(&response) {
            return Some(response);
        }
    }
    None
}

fn run_cargo_check(cwd: &str) -> Result<()> {
    let status = ProcessCommand::new("cargo")
        .current_dir(cwd)
        .arg("check")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run cargo check in {}", cwd))?;
    if !status.success() {
        anyhow::bail!("cargo check failed in {}", cwd);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tempfile::TempDir;

    #[tokio::test]
    async fn daemon_agent_assignee_defaults_to_unknown_role_when_phase_metadata_missing() {
        let hub = orchestrator_core::InMemoryServiceHub::new();
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "phase-less-workflow-assignee".to_string(),
                description:
                    "ensure daemon assignment still resolves when workflow phase is absent"
                        .to_string(),
                task_type: Some(TaskType::Feature),
                priority: None,
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        let mut phase_less_workflow = workflow;
        phase_less_workflow.current_phase = None;
        phase_less_workflow.phases.clear();
        phase_less_workflow.current_phase_index = 0;

        let project_root = TempDir::new().expect("temp dir should be created");
        let project_root = project_root.path().to_string_lossy().to_string();
        let (role, model) =
            daemon_agent_assignee_for_workflow_start(&project_root, &phase_less_workflow, &task);
        let runtime_config =
            orchestrator_core::load_agent_runtime_config_or_default(Path::new(&project_root));
        let expected_role = runtime_config
            .phase_agent_id("unknown")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "unknown".to_string());

        assert_eq!(role, expected_role);
        assert!(model
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()));
    }

    #[test]
    fn parse_merge_conflict_recovery_response_parses_json_line_output() {
        let transcript = r#"
thinking...
{"kind":"merge_conflict_resolution_result","status":"resolved","commit_message":"Resolve merge conflict","reason":""}
"#;
        let parsed = parse_merge_conflict_recovery_response(transcript)
            .expect("response should parse from transcript JSON line");
        assert_eq!(parsed.kind, "merge_conflict_resolution_result");
        assert_eq!(parsed.status, "resolved");
        assert_eq!(parsed.commit_message, "Resolve merge conflict");
    }

    #[test]
    fn parse_merge_conflict_recovery_response_rejects_non_json_output() {
        assert!(
            parse_merge_conflict_recovery_response("merge fixed, please continue").is_none(),
            "non-json output should not parse as recovery response"
        );
    }

    #[test]
    fn parse_merge_conflict_recovery_response_uses_latest_valid_payload() {
        let transcript = r#"
{"kind":"merge_conflict_resolution_result","status":"resolved|failed","commit_message":"placeholder","reason":""}
{"kind":"merge_conflict_resolution_result","status":"resolved","commit_message":"Resolve real conflict","reason":""}
"#;
        let parsed = parse_merge_conflict_recovery_response(transcript)
            .expect("response should parse from latest valid JSON line");
        assert_eq!(parsed.status, "resolved");
        assert_eq!(parsed.commit_message, "Resolve real conflict");
    }

    #[test]
    fn parse_merge_conflict_recovery_response_rejects_wrong_kind() {
        let transcript = r#"
{"kind":"phase_result","status":"resolved","commit_message":"not merge conflict result","reason":""}
"#;
        assert!(
            parse_merge_conflict_recovery_response(transcript).is_none(),
            "wrong kind should not parse as merge conflict recovery response"
        );
    }

    #[test]
    fn parse_merge_conflict_recovery_response_rejects_invalid_status_only_payload() {
        let transcript = r#"
{"kind":"merge_conflict_resolution_result","status":"resolved|failed","commit_message":"placeholder","reason":""}
"#;
        assert!(
            parse_merge_conflict_recovery_response(transcript).is_none(),
            "status placeholders should not be treated as valid recovery responses"
        );
    }

    #[test]
    fn build_em_scheduling_prompt_includes_contract_and_inputs() {
        let prompt = build_em_scheduling_prompt(&EmSchedulingPromptInput {
            available_agent_slots: 2,
            tasks: vec![EmSchedulingPromptTask {
                task_id: "TASK-001".to_string(),
                title: "Ship scheduler prompt".to_string(),
                status: "ready".to_string(),
                priority: "high".to_string(),
                risk: "medium".to_string(),
                complexity: "medium".to_string(),
                paused: false,
                cancelled: false,
                blocked_reason: None,
                linked_requirements: vec!["REQ-001".to_string()],
                dependencies: vec![EmSchedulingPromptTaskDependency {
                    task_id: "TASK-000".to_string(),
                    dependency_type: "blocked_by".to_string(),
                }],
            }],
            running_workflows: vec![EmSchedulingPromptWorkflow {
                workflow_id: "wf-1".to_string(),
                task_id: "TASK-100".to_string(),
                status: "running".to_string(),
                current_phase: Some("implementation".to_string()),
                failure_reason: None,
            }],
            recent_decisions: vec![EmSchedulingPromptDecisionRecord {
                workflow_id: "wf-1".to_string(),
                task_id: "TASK-100".to_string(),
                phase_id: "implementation".to_string(),
                decision: "advance".to_string(),
                risk: "low".to_string(),
                confidence: 0.92,
                reason: "phase complete".to_string(),
                timestamp: "2026-02-27T00:00:00Z".to_string(),
            }],
            recent_failures: vec![EmSchedulingPromptFailure {
                workflow_id: "wf-2".to_string(),
                task_id: "TASK-101".to_string(),
                reason: "tests failed".to_string(),
                completed_at: Some("2026-02-26T18:00:00Z".to_string()),
            }],
            requirement_coverage: vec![EmSchedulingRequirementCoverage {
                requirement_id: "REQ-001".to_string(),
                total_tasks: 3,
                ready_tasks: 1,
                in_progress_tasks: 1,
                blocked_tasks: 1,
                done_tasks: 0,
            }],
        });

        assert!(prompt.contains("em_scheduling_decision"));
        assert!(prompt.contains("Available agent slots for this tick: 2"));
        assert!(
            prompt.contains("Task list (statuses, priorities, dependencies, linked requirements):")
        );
        assert!(prompt.contains("Current running workflows and phases:"));
        assert!(prompt.contains("Recent decision history:"));
        assert!(prompt.contains("Recent failure reasons:"));
        assert!(prompt.contains("Requirement coverage snapshot:"));
        assert!(prompt.contains("Task priority and urgency."));
        assert!(prompt.contains("Dependency readiness and unblock impact."));
        assert!(prompt.contains("Risk level and complexity."));
        assert!(prompt.contains("Recent failures and repeated failure patterns."));
        assert!(prompt.contains("Requirement coverage balance"));
        assert!(prompt.contains("Resource constraints from available agent slots."));
        assert!(prompt.contains("Do not propose or perform code changes."));
        assert!(prompt.contains("Output JSON only. No markdown. No commentary."));
    }

    #[test]
    fn parse_em_scheduling_decision_response_parses_json_line_output() {
        let transcript = r#"
thinking...
{"kind":"em_scheduling_decision","queue":[{"task_id":"TASK-001","reason":"highest priority and unblocked"}],"defer":["TASK-002"],"preempt":[]}
"#;

        let parsed = parse_em_scheduling_decision_response(transcript)
            .expect("response should parse from transcript JSON line");
        assert_eq!(parsed.kind, "em_scheduling_decision");
        assert_eq!(parsed.queue.len(), 1);
        assert_eq!(parsed.queue[0].task_id, "TASK-001");
        assert_eq!(parsed.defer, vec!["TASK-002"]);
    }

    #[test]
    fn parse_em_scheduling_decision_response_parses_nested_payload() {
        let transcript = r#"
{"type":"result","result":"{\"kind\":\"em_scheduling_decision\",\"queue\":[{\"task_id\":\"TASK-010\",\"reason\":\"ready and unblocked\"}],\"defer\":[],\"preempt\":[\"TASK-099\"]}"}
"#;

        let parsed =
            parse_em_scheduling_decision_response(transcript).expect("nested payload should parse");
        assert_eq!(parsed.queue[0].task_id, "TASK-010");
        assert_eq!(parsed.preempt, vec!["TASK-099"]);
    }

    #[test]
    fn parse_em_scheduling_decision_response_rejects_kind_case_mismatch() {
        let transcript = r#"
{"kind":"EM_SCHEDULING_DECISION","queue":[{"task_id":"TASK-010","reason":"ready and unblocked"}],"defer":[],"preempt":[]}
"#;
        assert!(
            parse_em_scheduling_decision_response(transcript).is_none(),
            "kind must match em_scheduling_decision exactly"
        );
    }

    #[test]
    fn parse_em_scheduling_decision_response_rejects_missing_required_collections() {
        for transcript in [
            r#"{"kind":"em_scheduling_decision","defer":[],"preempt":[]}"#,
            r#"{"kind":"em_scheduling_decision","queue":[],"preempt":[]}"#,
            r#"{"kind":"em_scheduling_decision","queue":[],"defer":[]}"#,
        ] {
            assert!(
                parse_em_scheduling_decision_response(transcript).is_none(),
                "queue, defer, and preempt are all required"
            );
        }
    }

    #[test]
    fn parse_em_scheduling_decision_response_rejects_malformed_queue_items() {
        for transcript in [
            r#"{"kind":"em_scheduling_decision","queue":[{"task_id":"TASK-010","reason":""}],"defer":[],"preempt":[]}"#,
            r#"{"kind":"em_scheduling_decision","queue":[{"task_id":"","reason":"missing task id"}],"defer":[],"preempt":[]}"#,
        ] {
            assert!(
                parse_em_scheduling_decision_response(transcript).is_none(),
                "queue entries require both task_id and reason"
            );
        }
    }

    #[test]
    fn normalize_em_scheduling_decision_filters_unknown_and_duplicate_ids() {
        let decision = EmSchedulingDecision {
            kind: "em_scheduling_decision".to_string(),
            queue: vec![
                EmSchedulingQueueDecision {
                    task_id: "TASK-001".to_string(),
                    reason: "first".to_string(),
                },
                EmSchedulingQueueDecision {
                    task_id: "TASK-001".to_string(),
                    reason: "duplicate".to_string(),
                },
                EmSchedulingQueueDecision {
                    task_id: "TASK-404".to_string(),
                    reason: "unknown".to_string(),
                },
            ],
            defer: vec!["TASK-002".to_string()],
            preempt: vec!["TASK-003".to_string(), "TASK-003".to_string()],
        };
        let known_task_ids: HashSet<String> = ["TASK-001", "TASK-002", "TASK-003"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let running_task_ids: HashSet<String> =
            ["TASK-003"].into_iter().map(str::to_string).collect();

        let normalized =
            normalize_em_scheduling_decision(decision, &known_task_ids, &running_task_ids, 5);
        assert_eq!(normalized.queue_task_ids, vec!["TASK-001"]);
        assert_eq!(normalized.defer_task_ids, vec!["TASK-002"]);
        assert_eq!(normalized.preempt_task_ids, vec!["TASK-003"]);
    }

    #[test]
    fn normalize_em_scheduling_decision_resolves_cross_list_overlap() {
        let decision = EmSchedulingDecision {
            kind: "em_scheduling_decision".to_string(),
            queue: vec![
                EmSchedulingQueueDecision {
                    task_id: "TASK-001".to_string(),
                    reason: "first".to_string(),
                },
                EmSchedulingQueueDecision {
                    task_id: "TASK-002".to_string(),
                    reason: "deferred".to_string(),
                },
                EmSchedulingQueueDecision {
                    task_id: "TASK-003".to_string(),
                    reason: "running task".to_string(),
                },
            ],
            defer: vec!["TASK-002".to_string(), "TASK-003".to_string()],
            preempt: vec!["TASK-003".to_string(), "TASK-003".to_string()],
        };
        let known_task_ids: HashSet<String> = ["TASK-001", "TASK-002", "TASK-003"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let running_task_ids: HashSet<String> =
            ["TASK-003"].into_iter().map(str::to_string).collect();

        let normalized =
            normalize_em_scheduling_decision(decision, &known_task_ids, &running_task_ids, 1);
        assert_eq!(normalized.queue_task_ids, vec!["TASK-001"]);
        assert_eq!(normalized.defer_task_ids, vec!["TASK-002"]);
        assert_eq!(normalized.preempt_task_ids, vec!["TASK-003"]);
    }

    #[test]
    fn normalize_em_scheduling_decision_enforces_queue_capacity() {
        let decision = EmSchedulingDecision {
            kind: "em_scheduling_decision".to_string(),
            queue: vec![
                EmSchedulingQueueDecision {
                    task_id: "TASK-001".to_string(),
                    reason: "first".to_string(),
                },
                EmSchedulingQueueDecision {
                    task_id: "TASK-002".to_string(),
                    reason: "second".to_string(),
                },
                EmSchedulingQueueDecision {
                    task_id: "TASK-003".to_string(),
                    reason: "third".to_string(),
                },
            ],
            defer: Vec::new(),
            preempt: Vec::new(),
        };
        let known_task_ids: HashSet<String> = ["TASK-001", "TASK-002", "TASK-003"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let running_task_ids: HashSet<String> = HashSet::new();

        let normalized =
            normalize_em_scheduling_decision(decision, &known_task_ids, &running_task_ids, 1);
        assert_eq!(normalized.queue_task_ids, vec!["TASK-001"]);
    }

    #[test]
    fn normalize_em_scheduling_decision_drops_queue_when_no_slots_and_no_preemptions() {
        let decision = EmSchedulingDecision {
            kind: "em_scheduling_decision".to_string(),
            queue: vec![EmSchedulingQueueDecision {
                task_id: "TASK-001".to_string(),
                reason: "ready".to_string(),
            }],
            defer: Vec::new(),
            preempt: Vec::new(),
        };
        let known_task_ids: HashSet<String> =
            ["TASK-001"].into_iter().map(str::to_string).collect();
        let running_task_ids: HashSet<String> = HashSet::new();

        let normalized =
            normalize_em_scheduling_decision(decision, &known_task_ids, &running_task_ids, 0);
        assert!(normalized.queue_task_ids.is_empty());
    }

    #[test]
    fn normalize_em_scheduling_decision_allows_queue_up_to_preempted_capacity() {
        let decision = EmSchedulingDecision {
            kind: "em_scheduling_decision".to_string(),
            queue: vec![
                EmSchedulingQueueDecision {
                    task_id: "TASK-001".to_string(),
                    reason: "replace preempted work".to_string(),
                },
                EmSchedulingQueueDecision {
                    task_id: "TASK-002".to_string(),
                    reason: "exceeds preempted capacity".to_string(),
                },
            ],
            defer: Vec::new(),
            preempt: vec!["TASK-900".to_string()],
        };
        let known_task_ids: HashSet<String> = ["TASK-001", "TASK-002"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let running_task_ids: HashSet<String> =
            ["TASK-900"].into_iter().map(str::to_string).collect();

        let normalized =
            normalize_em_scheduling_decision(decision, &known_task_ids, &running_task_ids, 0);
        assert_eq!(normalized.preempt_task_ids, vec!["TASK-900"]);
        assert_eq!(normalized.queue_task_ids, vec!["TASK-001"]);
    }

    #[test]
    fn normalize_em_scheduling_decision_limits_preempt_to_running_tasks() {
        let decision = EmSchedulingDecision {
            kind: "em_scheduling_decision".to_string(),
            queue: vec![EmSchedulingQueueDecision {
                task_id: "TASK-001".to_string(),
                reason: "ready".to_string(),
            }],
            defer: Vec::new(),
            preempt: vec!["TASK-900".to_string(), "TASK-901".to_string()],
        };
        let known_task_ids: HashSet<String> =
            ["TASK-001"].into_iter().map(str::to_string).collect();
        let running_task_ids: HashSet<String> =
            ["TASK-901"].into_iter().map(str::to_string).collect();

        let normalized =
            normalize_em_scheduling_decision(decision, &known_task_ids, &running_task_ids, 0);
        assert_eq!(normalized.preempt_task_ids, vec!["TASK-901"]);
        assert_eq!(normalized.queue_task_ids, vec!["TASK-001"]);
    }
}

pub(super) async fn project_tick(root: &str, args: &DaemonRunArgs) -> Result<ProjectTickSummary> {
    let root = canonicalize_lossy(root);
    let hub = Arc::new(FileServiceHub::new(&root)?);
    let _ = flush_git_integration_outbox(&root);
    let requirements_before = hub.planning().list_requirements().await.unwrap_or_default();
    let tasks_before = hub.tasks().list().await.unwrap_or_default();
    let daemon = hub.daemon();
    let status = daemon.status().await?;
    let mut started_daemon = false;
    if !matches!(
        status,
        orchestrator_core::DaemonStatus::Running | orchestrator_core::DaemonStatus::Paused
    ) {
        daemon.start().await?;
        started_daemon = true;
    }

    bootstrap_from_vision_if_needed(hub.clone(), args.startup_cleanup, args.ai_task_generation)
        .await?;

    if args.ai_task_generation {
        let _ = ensure_tasks_for_unplanned_requirements(hub.clone(), &root).await;
    }

    let mut cleaned_stale_workflows = 0usize;
    let mut resumed_workflows = 0usize;
    if args.resume_interrupted {
        let (cleaned, resumed) =
            resume_interrupted_workflows_for_project(hub.clone(), &root).await?;
        cleaned_stale_workflows = cleaned;
        resumed_workflows = resumed;
    }

    let reconciled_stale_tasks = if args.reconcile_stale {
        reconcile_stale_in_progress_tasks_for_project(hub.clone(), &root).await?
    } else {
        0
    };
    let reconciled_dependency_tasks =
        reconcile_dependency_gate_tasks_for_project(hub.clone(), &root).await?;
    let reconciled_merge_tasks = reconcile_merge_gate_tasks_for_project(hub.clone(), &root).await?;

    if args.auto_run_ready {
        let _ = retry_failed_task_workflows(hub.clone()).await;
        let _ = promote_backlog_tasks_to_ready(hub.clone(), &root).await;
    }

    let started_ready_workflows = if args.auto_run_ready {
        run_ready_task_workflows_for_project(hub.clone(), &root, args.max_tasks_per_tick).await?
    } else {
        0
    };
    let _ = refresh_runtime_binaries_if_main_advanced(
        hub.clone(),
        &root,
        git_ops::RuntimeBinaryRefreshTrigger::Tick,
    )
    .await;
    let (executed_workflow_phases, failed_workflow_phases, phase_execution_events) =
        execute_running_workflow_phases_for_project(hub.clone(), &root, args.max_tasks_per_tick)
            .await?;

    let health = serde_json::to_value(daemon.health().await?)?;
    let tasks = hub.tasks().list().await?;
    let workflows = hub.workflows().list().await.unwrap_or_default();

    let tasks_total = tasks.len();
    let tasks_ready = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Ready | TaskStatus::Backlog))
        .count();
    let tasks_in_progress = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::InProgress)
        .count();
    let tasks_blocked = tasks.iter().filter(|task| task.status.is_blocked()).count();
    let tasks_done = tasks
        .iter()
        .filter(|task| task.status.is_terminal())
        .count();
    let stale_in_progress =
        stale_in_progress_summary(&tasks, args.stale_threshold_hours, Utc::now());

    let workflows_running = workflows
        .iter()
        .filter(|workflow| {
            matches!(
                workflow.status,
                WorkflowStatus::Running | WorkflowStatus::Paused
            )
        })
        .count();
    let workflows_completed = workflows
        .iter()
        .filter(|workflow| is_terminally_completed_workflow(workflow))
        .count();
    let workflows_failed = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Failed)
        .count();
    let requirements_after = hub.planning().list_requirements().await.unwrap_or_default();
    let requirement_lifecycle_transitions =
        collect_requirement_lifecycle_transitions(&requirements_before, &requirements_after);
    let task_state_transitions =
        collect_task_state_transitions(&tasks_before, &tasks, &workflows, &phase_execution_events);

    Ok(ProjectTickSummary {
        project_root: root,
        started_daemon,
        health,
        tasks_total,
        tasks_ready,
        tasks_in_progress,
        tasks_blocked,
        tasks_done,
        stale_in_progress_count: stale_in_progress.count,
        stale_in_progress_threshold_hours: stale_in_progress.threshold_hours,
        stale_in_progress_task_ids: stale_in_progress.task_ids(),
        workflows_running,
        workflows_completed,
        workflows_failed,
        resumed_workflows,
        cleaned_stale_workflows,
        reconciled_stale_tasks: reconciled_stale_tasks
            .saturating_add(reconciled_dependency_tasks)
            .saturating_add(reconciled_merge_tasks),
        started_ready_workflows,
        executed_workflow_phases,
        failed_workflow_phases,
        phase_execution_events,
        requirement_lifecycle_transitions,
        task_state_transitions,
    })
}
