use super::*;

const EM_WORK_QUEUE_STATE_FILE: &str = "em-work-queue.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskSelectionSource {
    EmQueue,
    FallbackPicker,
}

impl TaskSelectionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmQueue => "em_queue",
            Self::FallbackPicker => "fallback_picker",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReadyTaskWorkflowStart {
    pub task_id: String,
    pub workflow_id: String,
    pub selection_source: TaskSelectionSource,
}

#[derive(Debug, Clone, Default)]
pub struct ReadyTaskWorkflowStartSummary {
    pub started: usize,
    pub started_workflows: Vec<ReadyTaskWorkflowStart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmWorkQueueEntryStatus {
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
pub struct EmWorkQueueEntry {
    pub task_id: String,
    #[serde(default)]
    pub status: EmWorkQueueEntryStatus,
    #[serde(default)]
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub assigned_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EmWorkQueueState {
    #[serde(default)]
    pub entries: Vec<EmWorkQueueEntry>,
}

pub fn em_work_queue_state_path(project_root: &str) -> Result<PathBuf> {
    Ok(daemon_repo_runtime_root(project_root)?
        .join("scheduler")
        .join(EM_WORK_QUEUE_STATE_FILE))
}

pub fn load_em_work_queue_state(project_root: &str) -> Result<Option<EmWorkQueueState>> {
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

pub fn save_em_work_queue_state(project_root: &str, state: &EmWorkQueueState) -> Result<()> {
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

pub fn mark_em_work_queue_entry_assigned(
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

pub fn remove_terminal_em_work_queue_entry_non_fatal(
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

pub fn ready_task_dispatch_limit(
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

pub fn normalize_optional_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(|candidate| candidate.to_string())
}

pub fn is_terminally_completed_workflow(workflow: &orchestrator_core::OrchestratorWorkflow) -> bool {
    workflow.status == WorkflowStatus::Completed
        && workflow.machine_state == orchestrator_core::WorkflowMachineState::Completed
        && workflow.completed_at.is_some()
}

pub fn workflow_current_phase_id(workflow: &orchestrator_core::OrchestratorWorkflow) -> Option<String> {
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

pub fn routing_complexity_for_task(
    task: &orchestrator_core::OrchestratorTask,
) -> Option<protocol::ModelRoutingComplexity> {
    match task.complexity {
        orchestrator_core::Complexity::Low => Some(protocol::ModelRoutingComplexity::Low),
        orchestrator_core::Complexity::Medium => Some(protocol::ModelRoutingComplexity::Medium),
        orchestrator_core::Complexity::High => Some(protocol::ModelRoutingComplexity::High),
    }
}

pub fn daemon_agent_assignee_for_workflow_start(
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
        Some(project_root),
    );
    let model = execution_targets.first().map(|(_, model)| model.clone());
    (role, model)
}

pub async fn auto_assign_task_to_daemon_agent(
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

pub async fn sync_task_status_for_workflow_result(
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

pub async fn run_ready_task_workflows_for_project(
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
