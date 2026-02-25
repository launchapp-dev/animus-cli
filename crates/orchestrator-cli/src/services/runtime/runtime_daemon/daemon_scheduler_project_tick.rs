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
) {
    match workflow_status {
        WorkflowStatus::Completed => {
            let task = hub.tasks().get(task_id).await;
            let Ok(task) = task else {
                let _ = hub.tasks().set_status(task_id, TaskStatus::Done).await;
                return;
            };

            match post_success_merge_push_and_cleanup(hub.clone(), project_root, &task).await {
                Ok(true) => {
                    let _ = hub.tasks().set_status(task_id, TaskStatus::Done).await;
                    return;
                }
                Ok(false) => {}
                Err(error) => {
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
        .filter(|workflow| workflow.status == WorkflowStatus::Completed)
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
        sync_task_status_for_workflow_result(hub.clone(), root, &updated.task_id, updated.status)
            .await;
    }

    Ok((cleaned, resumed))
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
        .filter(|workflow| workflow.status == WorkflowStatus::Completed)
        .map(|workflow| workflow.task_id.clone())
        .collect();

    let mut started = 0usize;
    let candidates = hub.tasks().list_prioritized().await?;
    for task in candidates {
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
        sync_task_status_for_workflow_result(hub.clone(), project_root, &task.id, workflow.status)
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
            let reason = "requirements validation requested research evidence before execution"
                .to_string();
            let updated = hub
                .workflows()
                .request_research(&workflow.id, reason)
                .await?;
            sync_task_status_for_workflow_result(
                hub.clone(),
                project_root,
                &updated.task_id,
                updated.status,
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

            (scheduled.workflow, scheduled.task, scheduled.phase_id, run_result)
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
                    PhaseExecutionOutcome::Completed { .. } => {
                        enforce_frontend_phase_gate(project_root, &workflow.id, &phase_id, &task)?;
                        let updated = hub.workflows().complete_current_phase(&workflow.id).await?;
                        sync_task_status_for_workflow_result(
                            hub.clone(),
                            project_root,
                            &updated.task_id,
                            updated.status,
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
                let updated = hub
                    .workflows()
                    .fail_current_phase(&workflow.id, error_message)
                    .await?;
                sync_task_status_for_workflow_result(
                    hub.clone(),
                    project_root,
                    &updated.task_id,
                    updated.status,
                )
                .await;
                failed = failed.saturating_add(1);
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

pub(super) async fn project_tick(root: &str, args: &DaemonRunArgs) -> Result<ProjectTickSummary> {
    let root = canonicalize_lossy(root);
    let hub = Arc::new(FileServiceHub::new(&root)?);
    let _ = flush_git_integration_outbox(&root);
    let requirements_before = hub.planning().list_requirements().await.unwrap_or_default();
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

    let started_ready_workflows = if args.auto_run_ready {
        run_ready_task_workflows_for_project(hub.clone(), &root, args.max_tasks_per_tick).await?
    } else {
        0
    };
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
        .filter(|workflow| workflow.status == WorkflowStatus::Completed)
        .count();
    let workflows_failed = workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Failed)
        .count();
    let requirements_after = hub.planning().list_requirements().await.unwrap_or_default();
    let requirement_lifecycle_transitions =
        collect_requirement_lifecycle_transitions(&requirements_before, &requirements_after);

    Ok(ProjectTickSummary {
        project_root: root,
        started_daemon,
        health,
        tasks_total,
        tasks_ready,
        tasks_in_progress,
        tasks_blocked,
        tasks_done,
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
    })
}
