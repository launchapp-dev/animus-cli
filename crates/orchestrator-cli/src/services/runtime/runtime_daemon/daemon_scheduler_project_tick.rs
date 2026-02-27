use super::*;
use crate::services::runtime::stale_in_progress_summary;

const EM_WORK_QUEUE_STATE_FILE: &str = "em-work-queue.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskSelectionSource {
    EmQueue,
    FallbackPicker,
}

impl TaskSelectionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::EmQueue => "em_queue",
            Self::FallbackPicker => "fallback_picker",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ReadyTaskWorkflowStart {
    task_id: String,
    workflow_id: String,
    selection_source: TaskSelectionSource,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ReadyTaskWorkflowStartSummary {
    pub(super) started: usize,
    pub(super) started_workflows: Vec<ReadyTaskWorkflowStart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EmWorkQueueEntryStatus {
    Pending,
    Assigned,
    #[serde(other)]
    Unknown,
}

impl Default for EmWorkQueueEntryStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmWorkQueueEntry {
    task_id: String,
    #[serde(default)]
    status: EmWorkQueueEntryStatus,
    #[serde(default)]
    workflow_id: Option<String>,
    #[serde(default)]
    assigned_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EmWorkQueueState {
    #[serde(default)]
    entries: Vec<EmWorkQueueEntry>,
}

fn em_work_queue_state_path(project_root: &str) -> Result<PathBuf> {
    Ok(daemon_repo_runtime_root(project_root)?
        .join("scheduler")
        .join(EM_WORK_QUEUE_STATE_FILE))
}

fn load_em_work_queue_state(project_root: &str) -> Result<Option<EmWorkQueueState>> {
    let path = em_work_queue_state_path(project_root)?;
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read EM work queue state file at {}",
            path.display()
        )
    })?;
    if content.trim().is_empty() {
        return Ok(Some(EmWorkQueueState::default()));
    }

    serde_json::from_str::<EmWorkQueueState>(&content)
        .map(Some)
        .or_else(|_| {
            serde_json::from_str::<Vec<EmWorkQueueEntry>>(&content)
                .map(|entries| Some(EmWorkQueueState { entries }))
        })
        .with_context(|| {
            format!(
                "failed to parse EM work queue state file at {}",
                path.display()
            )
        })
}

fn save_em_work_queue_state(project_root: &str, state: &EmWorkQueueState) -> Result<()> {
    let path = em_work_queue_state_path(project_root)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if state.entries.is_empty() {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }

    let payload = serde_json::to_string_pretty(state)?;
    let tmp_path = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(EM_WORK_QUEUE_STATE_FILE),
        Uuid::new_v4()
    ));
    fs::write(&tmp_path, payload)?;
    fs::rename(&tmp_path, &path)?;
    Ok(())
}

fn mark_em_work_queue_entry_assigned(
    project_root: &str,
    task_id: &str,
    workflow_id: &str,
) -> Result<bool> {
    let Some(mut state) = load_em_work_queue_state(project_root)? else {
        return Ok(false);
    };

    let mut updated = false;
    for entry in &mut state.entries {
        if entry.task_id != task_id {
            continue;
        }
        if entry.status != EmWorkQueueEntryStatus::Pending {
            continue;
        }
        entry.status = EmWorkQueueEntryStatus::Assigned;
        entry.workflow_id = Some(workflow_id.to_string());
        entry.assigned_at = Some(Utc::now().to_rfc3339());
        updated = true;
        break;
    }

    if updated {
        save_em_work_queue_state(project_root, &state)?;
    }

    Ok(updated)
}

fn remove_terminal_em_work_queue_entry(
    project_root: &str,
    task_id: &str,
    workflow_id: Option<&str>,
) -> Result<usize> {
    let Some(mut state) = load_em_work_queue_state(project_root)? else {
        return Ok(0);
    };

    let before = state.entries.len();
    state.entries.retain(|entry| {
        if entry.task_id != task_id {
            return true;
        }
        if entry.status != EmWorkQueueEntryStatus::Assigned {
            return true;
        }
        if let Some(workflow_id) = workflow_id {
            if entry
                .workflow_id
                .as_deref()
                .is_some_and(|entry_workflow_id| entry_workflow_id != workflow_id)
            {
                return true;
            }
        }
        false
    });
    let removed = before.saturating_sub(state.entries.len());
    if removed > 0 {
        save_em_work_queue_state(project_root, &state)?;
    }
    Ok(removed)
}

fn remove_terminal_em_work_queue_entry_non_fatal(
    project_root: &str,
    task_id: &str,
    workflow_id: Option<&str>,
) {
    if let Err(error) = remove_terminal_em_work_queue_entry(project_root, task_id, workflow_id) {
        eprintln!(
            "ao-daemon: failed to remove terminal EM queue entry for task {}: {}",
            task_id, error
        );
    }
}

fn ready_task_dispatch_limit(
    max_tasks_per_tick: usize,
    health: &orchestrator_core::DaemonHealth,
) -> usize {
    if max_tasks_per_tick == 0 {
        return 0;
    }
    match health.max_agents {
        Some(max_agents) => {
            let available_agent_slots = max_agents.saturating_sub(health.active_agents);
            max_tasks_per_tick.min(available_agent_slots)
        }
        None => max_tasks_per_tick,
    }
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
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
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
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
            let _ = hub.tasks().set_status(task_id, TaskStatus::Blocked).await;
        }
        WorkflowStatus::Cancelled => {
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
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

pub(super) async fn run_ready_task_workflows_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    max_tasks_per_tick: usize,
) -> Result<ReadyTaskWorkflowStartSummary> {
    if max_tasks_per_tick == 0 {
        return Ok(ReadyTaskWorkflowStartSummary::default());
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

    let candidates = hub.tasks().list_prioritized().await?;
    let task_lookup: std::collections::HashMap<String, orchestrator_core::OrchestratorTask> =
        candidates
            .iter()
            .cloned()
            .map(|task| (task.id.clone(), task))
            .collect();

    let mut selected_for_start: Vec<(orchestrator_core::OrchestratorTask, TaskSelectionSource)> =
        Vec::new();
    let mut selected_task_ids: HashSet<String> = HashSet::new();

    match load_em_work_queue_state(project_root) {
        Ok(Some(queue_state)) => {
            for entry in queue_state.entries {
                if selected_for_start.len() >= max_tasks_per_tick {
                    break;
                }
                if entry.status != EmWorkQueueEntryStatus::Pending {
                    continue;
                }

                let Some(task) = task_lookup.get(entry.task_id.as_str()).cloned() else {
                    continue;
                };
                if !selected_task_ids.insert(task.id.clone()) {
                    continue;
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

                selected_for_start.push((task, TaskSelectionSource::EmQueue));
            }
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("ao-daemon: failed to load EM work queue state: {}", error);
        }
    }

    if selected_for_start.is_empty() {
        for task in candidates {
            if selected_for_start.len() >= max_tasks_per_tick {
                break;
            }
            if !selected_task_ids.insert(task.id.clone()) {
                continue;
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

            selected_for_start.push((task, TaskSelectionSource::FallbackPicker));
        }
    }

    let mut started_workflows = Vec::new();
    for (task, selection_source) in selected_for_start {
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: Some(pipeline_for_task(&task)),
            })
            .await?;
        if selection_source == TaskSelectionSource::EmQueue {
            if let Err(error) =
                mark_em_work_queue_entry_assigned(project_root, &task.id, workflow.id.as_str())
            {
                eprintln!(
                    "ao-daemon: failed to mark EM queue entry assigned for task {}: {}",
                    task.id, error
                );
            }
        }
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
        started_workflows.push(ReadyTaskWorkflowStart {
            task_id: task.id.clone(),
            workflow_id: workflow.id.clone(),
            selection_source,
        });
    }

    Ok(ReadyTaskWorkflowStartSummary {
        started: started_workflows.len(),
        started_workflows,
    })
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
    use orchestrator_core::Priority;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

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

    #[tokio::test]
    async fn run_ready_prefers_em_queue_and_marks_selected_entry_assigned() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let fallback_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "fallback-high-priority".to_string(),
                description: "should be skipped when queue has dispatchable item".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Critical),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("fallback task should be created");
        hub.tasks()
            .set_status(&fallback_task.id, TaskStatus::Ready)
            .await
            .expect("fallback task should be ready");

        let queue_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "queue-low-priority".to_string(),
                description: "should be selected from queue first".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Low),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("queue task should be created");
        hub.tasks()
            .set_status(&queue_task.id, TaskStatus::Ready)
            .await
            .expect("queue task should be ready");

        save_em_work_queue_state(
            &project_root_str,
            &EmWorkQueueState {
                entries: vec![
                    EmWorkQueueEntry {
                        task_id: queue_task.id.clone(),
                        status: EmWorkQueueEntryStatus::Pending,
                        workflow_id: None,
                        assigned_at: None,
                    },
                    EmWorkQueueEntry {
                        task_id: fallback_task.id.clone(),
                        status: EmWorkQueueEntryStatus::Pending,
                        workflow_id: None,
                        assigned_at: None,
                    },
                ],
            },
        )
        .expect("queue state should be stored");

        let summary = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            1,
        )
        .await
        .expect("ready runner should succeed");

        assert_eq!(summary.started, 1);
        assert_eq!(summary.started_workflows.len(), 1);
        let started = &summary.started_workflows[0];
        assert_eq!(started.task_id, queue_task.id);
        assert_eq!(started.selection_source, TaskSelectionSource::EmQueue);

        let queue_state = load_em_work_queue_state(&project_root_str)
            .expect("queue should load")
            .expect("queue should exist");
        let queue_entry = queue_state
            .entries
            .iter()
            .find(|entry| entry.task_id == queue_task.id)
            .expect("queue task entry should remain present");
        assert_eq!(queue_entry.status, EmWorkQueueEntryStatus::Assigned);
        assert_eq!(
            queue_entry.workflow_id.as_deref(),
            Some(started.workflow_id.as_str())
        );

        let fallback_entry = queue_state
            .entries
            .iter()
            .find(|entry| entry.task_id == fallback_task.id)
            .expect("fallback queue entry should remain present");
        assert_eq!(fallback_entry.status, EmWorkQueueEntryStatus::Pending);
    }

    #[tokio::test]
    async fn run_ready_falls_back_when_queue_has_no_dispatchable_entries() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let queue_only_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "queue-backlog".to_string(),
                description: "queue entry points at non-ready task".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::High),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("queue task should be created");

        let fallback_ready_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "fallback-ready".to_string(),
                description: "ready task should start via fallback picker".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("fallback task should be created");
        hub.tasks()
            .set_status(&fallback_ready_task.id, TaskStatus::Ready)
            .await
            .expect("fallback task should be ready");

        save_em_work_queue_state(
            &project_root_str,
            &EmWorkQueueState {
                entries: vec![EmWorkQueueEntry {
                    task_id: queue_only_task.id.clone(),
                    status: EmWorkQueueEntryStatus::Pending,
                    workflow_id: None,
                    assigned_at: None,
                }],
            },
        )
        .expect("queue state should be stored");

        let summary = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            1,
        )
        .await
        .expect("ready runner should succeed");
        assert_eq!(summary.started, 1);
        assert_eq!(summary.started_workflows.len(), 1);
        let started = &summary.started_workflows[0];
        assert_eq!(started.task_id, fallback_ready_task.id);
        assert_eq!(
            started.selection_source,
            TaskSelectionSource::FallbackPicker
        );
    }

    #[tokio::test]
    async fn run_ready_falls_back_when_queue_state_is_invalid_json() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let fallback_ready_task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "fallback-ready-invalid-queue".to_string(),
                description: "ready task should still run when queue decode fails".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("fallback task should be created");
        hub.tasks()
            .set_status(&fallback_ready_task.id, TaskStatus::Ready)
            .await
            .expect("fallback task should be ready");

        let queue_path = em_work_queue_state_path(&project_root_str).expect("queue path");
        if let Some(parent) = queue_path.parent() {
            fs::create_dir_all(parent).expect("queue parent should be created");
        }
        fs::write(&queue_path, "{ invalid json").expect("invalid queue payload should be written");

        let summary = run_ready_task_workflows_for_project(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            1,
        )
        .await
        .expect("ready runner should continue when queue state is invalid");
        assert_eq!(summary.started, 1);
        assert_eq!(summary.started_workflows.len(), 1);
        assert_eq!(
            summary.started_workflows[0].selection_source,
            TaskSelectionSource::FallbackPicker
        );
    }

    #[tokio::test]
    async fn sync_task_status_for_workflow_result_removes_assigned_queue_entries_on_failure() {
        let _lock = env_lock().lock().expect("env lock should be available");
        let home = TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", Some(home.path().to_string_lossy().as_ref()));

        let hub = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let project_root = TempDir::new().expect("project temp dir");
        let project_root_str = project_root.path().to_string_lossy().to_string();

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "queue-terminal-cleanup".to_string(),
                description: "assigned queue entry should be removed after failure".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        hub.tasks()
            .set_status(&task.id, TaskStatus::InProgress)
            .await
            .expect("task should be in-progress");

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput {
                task_id: task.id.clone(),
                pipeline_id: None,
            })
            .await
            .expect("workflow should start");

        save_em_work_queue_state(
            &project_root_str,
            &EmWorkQueueState {
                entries: vec![EmWorkQueueEntry {
                    task_id: task.id.clone(),
                    status: EmWorkQueueEntryStatus::Assigned,
                    workflow_id: Some(workflow.id.clone()),
                    assigned_at: Some(Utc::now().to_rfc3339()),
                }],
            },
        )
        .expect("queue state should be written");

        sync_task_status_for_workflow_result(
            hub.clone() as Arc<dyn ServiceHub>,
            &project_root_str,
            task.id.as_str(),
            WorkflowStatus::Failed,
            Some(workflow.id.as_str()),
        )
        .await;

        let queue_state =
            load_em_work_queue_state(&project_root_str).expect("queue should load after cleanup");
        assert!(
            queue_state.is_none()
                || queue_state
                    .as_ref()
                    .is_some_and(|state| state.entries.is_empty())
        );
    }

    #[test]
    fn ready_task_dispatch_limit_honors_available_agent_capacity() {
        let uncapped = orchestrator_core::DaemonHealth {
            healthy: true,
            status: orchestrator_core::DaemonStatus::Running,
            runner_connected: true,
            runner_pid: None,
            active_agents: 1,
            max_agents: None,
            project_root: None,
        };
        assert_eq!(ready_task_dispatch_limit(4, &uncapped), 4);

        let capped = orchestrator_core::DaemonHealth {
            healthy: true,
            status: orchestrator_core::DaemonStatus::Running,
            runner_connected: true,
            runner_pid: None,
            active_agents: 3,
            max_agents: Some(5),
            project_root: None,
        };
        assert_eq!(ready_task_dispatch_limit(10, &capped), 2);
        assert_eq!(ready_task_dispatch_limit(1, &capped), 1);

        let saturated = orchestrator_core::DaemonHealth {
            healthy: true,
            status: orchestrator_core::DaemonStatus::Running,
            runner_connected: true,
            runner_pid: None,
            active_agents: 5,
            max_agents: Some(5),
            project_root: None,
        };
        assert_eq!(ready_task_dispatch_limit(3, &saturated), 0);
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

    let ready_dispatch_limit = if args.auto_run_ready {
        match daemon.health().await {
            Ok(health) => ready_task_dispatch_limit(args.max_tasks_per_tick, &health),
            Err(_) => args.max_tasks_per_tick,
        }
    } else {
        0
    };
    let ready_workflow_starts = if args.auto_run_ready {
        run_ready_task_workflows_for_project(hub.clone(), &root, ready_dispatch_limit).await?
    } else {
        ReadyTaskWorkflowStartSummary::default()
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
    let task_state_transitions = collect_task_state_transitions(
        &tasks_before,
        &tasks,
        &workflows,
        &phase_execution_events,
        &ready_workflow_starts.started_workflows,
    );

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
        started_ready_workflows: ready_workflow_starts.started,
        executed_workflow_phases,
        failed_workflow_phases,
        phase_execution_events,
        requirement_lifecycle_transitions,
        task_state_transitions,
    })
}
