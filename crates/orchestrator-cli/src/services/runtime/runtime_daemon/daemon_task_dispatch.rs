use super::*;
pub use orchestrator_daemon_runtime::{
    active_workflow_task_ids, is_terminally_completed_workflow, plan_ready_task_dispatch,
    routing_complexity_for_task, should_skip_dispatch, workflow_current_phase_id, EmWorkQueueEntry,
    EmWorkQueueEntryStatus, EmWorkQueueState, ReadyTaskWorkflowStart,
    ReadyTaskWorkflowStartSummary, TaskSelectionSource,
};

const EM_WORK_QUEUE_STATE_FILE: &str = "em-work-queue.json";
const MAX_DISPATCH_RETRIES: u32 = 3;

pub fn em_work_queue_state_path(project_root: &str) -> Result<PathBuf> {
    Ok(git_ops::daemon_repo_runtime_root(project_root)?
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
            "{}: failed to remove terminal EM queue entry for task {}: {}",
            protocol::ACTOR_DAEMON,
            task_id,
            error
        );
    }
}

fn resolve_workflow_pipeline_definition<'a>(
    config: &'a orchestrator_core::WorkflowConfig,
    pipeline_id: &str,
) -> Option<&'a orchestrator_core::workflow_config::PipelineDefinition> {
    config
        .pipelines
        .iter()
        .find(|pipeline| pipeline.id.eq_ignore_ascii_case(pipeline_id))
}

fn effective_post_success_git_config(
    project_root: &str,
    workflow: Option<&orchestrator_core::OrchestratorWorkflow>,
) -> git_ops::PostSuccessGitConfig {
    let mut cfg = git_ops::load_post_success_git_config(project_root);

    let workflow = match workflow {
        Some(workflow) => workflow,
        None => return cfg,
    };
    let workflow_config = match orchestrator_core::load_workflow_config(Path::new(project_root)) {
        Ok(config) => config,
        Err(_) => return cfg,
    };

    let requested_pipeline_id = workflow
        .pipeline_id
        .as_deref()
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .unwrap_or_else(|| workflow_config.default_pipeline_id.as_str());
    let Some(pipeline) =
        resolve_workflow_pipeline_definition(&workflow_config, requested_pipeline_id)
            .or_else(|| {
                resolve_workflow_pipeline_definition(
                    &workflow_config,
                    workflow_config.default_pipeline_id.as_str(),
                )
            })
            .or_else(|| {
                resolve_workflow_pipeline_definition(
                    &workflow_config,
                    orchestrator_core::STANDARD_PIPELINE_ID,
                )
            })
    else {
        return cfg;
    };

    let Some(merge_cfg) = pipeline
        .post_success
        .as_ref()
        .and_then(|post_success| post_success.merge.as_ref())
    else {
        eprintln!(
            "warning: using daemon post-success merge flags (`--auto-merge`, `--auto-pr`) because workflow pipeline `{}` is missing `post_success.merge`; prefer configuring it in `.ao/workflows.yaml` (or `.ao/state/workflow-config.v2.json`) for deprecation-safe behavior",
            pipeline.id
        );
        return cfg;
    };

    cfg.auto_merge_enabled = merge_cfg.auto_merge;
    cfg.auto_pr_enabled = merge_cfg.create_pr;
    if let Some(target_branch) = Some(merge_cfg.target_branch.trim()).filter(|v| !v.is_empty()) {
        cfg.auto_merge_target_branch = target_branch.to_string();
    }
    cfg
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
    let caps = runtime_config.phase_capabilities(&phase_id);
    let execution_targets = PhaseTargetPlanner::build_phase_execution_targets(
        &phase_id,
        runtime_config.phase_model_override(&phase_id),
        runtime_config.phase_tool_override(&phase_id),
        fallback_models.as_slice(),
        routing_complexity_for_task(task),
        Some(project_root),
        &caps,
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
        .assign_agent(&task.id, role, model, protocol::ACTOR_DAEMON.to_string())
        .await?;
    Ok(())
}

async fn record_dispatch_history_entry(
    hub: Arc<dyn ServiceHub>,
    task_id: &str,
    workflow_id: &str,
    outcome: &str,
) {
    let workflow = hub.workflows().get(workflow_id).await.ok();
    let now = Utc::now();
    let started_at = workflow
        .as_ref()
        .map(|w| w.started_at.to_rfc3339())
        .unwrap_or_else(|| now.to_rfc3339());
    let ended_at = now.to_rfc3339();
    let duration_secs = workflow.as_ref().map(|w| {
        let start = w.started_at;
        (now - start).num_milliseconds() as f64 / 1000.0
    });
    let failed_phase = workflow.as_ref().and_then(|w| w.current_phase.clone());
    let failure_reason = workflow.as_ref().and_then(|w| w.failure_reason.clone());

    let entry = orchestrator_core::DispatchHistoryEntry {
        workflow_id: workflow_id.to_string(),
        started_at,
        ended_at: Some(ended_at),
        duration_secs,
        outcome: outcome.to_string(),
        failed_phase: if outcome != "completed" {
            failed_phase
        } else {
            None
        },
        failure_reason: if outcome != "completed" {
            failure_reason
        } else {
            None
        },
    };

    let Ok(mut task) = hub.tasks().get(task_id).await else {
        return;
    };
    task.dispatch_history.push(entry);
    let max = orchestrator_core::MAX_DISPATCH_HISTORY_ENTRIES;
    if task.dispatch_history.len() > max {
        let drain_count = task.dispatch_history.len() - max;
        task.dispatch_history.drain(..drain_count);
    }
    let _ = hub.tasks().replace(task).await;
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
            if let Some(wf_id) = workflow_id {
                record_dispatch_history_entry(hub.clone(), task_id, wf_id, "completed").await;
            }
            let workflow = if let Some(wf_id) = workflow_id {
                hub.workflows().get(wf_id).await.ok()
            } else {
                None
            };
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
            let task = hub.tasks().get(task_id).await;
            let Ok(task) = task else {
                let _ = hub
                    .tasks()
                    .set_status(task_id, TaskStatus::Done, false)
                    .await;
                return;
            };
            let cfg = effective_post_success_git_config(project_root, workflow.as_ref());

            match git_ops::post_success_merge_push_and_cleanup(
                hub.clone(),
                project_root,
                &task,
                &cfg,
            )
            .await
            {
                Ok(git_ops::PostMergeOutcome::Completed) => {
                    let _ = hub
                        .tasks()
                        .set_status(task_id, TaskStatus::Done, false)
                        .await;
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
                        git_ops::cleanup_merge_conflict_worktree(project_root, &context);
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

                    match git_ops::finalize_merge_conflict_resolution(
                        hub.clone(),
                        project_root,
                        &task,
                        &cfg,
                        &context,
                    )
                    .await
                    {
                        Ok(()) => {
                            if let Some(workflow_id) = workflow_id {
                                let _ = hub.workflows().resolve_merge_conflict(workflow_id).await;
                            }
                            let _ = hub
                                .tasks()
                                .set_status(task_id, TaskStatus::Done, false)
                                .await;
                        }
                        Err(error) => {
                            git_ops::cleanup_merge_conflict_worktree(project_root, &context);
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
                let _ = hub
                    .tasks()
                    .set_status(task_id, TaskStatus::Done, false)
                    .await;
                return;
            };

            match git_ops::is_branch_merged(project_root, branch_name) {
                Ok(Some(true)) | Ok(None) => {
                    let _ = hub
                        .tasks()
                        .set_status(task_id, TaskStatus::Done, false)
                        .await;
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
            if let Some(wf_id) = workflow_id {
                record_dispatch_history_entry(hub.clone(), task_id, wf_id, "failed").await;
            }
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
            if let Ok(mut task) = hub.tasks().get(task_id).await {
                let count = task
                    .consecutive_dispatch_failures
                    .unwrap_or(0)
                    .saturating_add(1);
                task.consecutive_dispatch_failures = Some(count);
                task.last_dispatch_failure_at = Some(Utc::now().to_rfc3339());
                if count >= MAX_DISPATCH_RETRIES {
                    let reason =
                        format!("auto-blocked after {} consecutive dispatch failures", count);
                    let _ = set_task_blocked_with_reason(hub.clone(), &task, reason, None).await;
                } else {
                    let _ = hub.tasks().replace(task).await;
                    let _ = hub
                        .tasks()
                        .set_status(task_id, TaskStatus::Blocked, false)
                        .await;
                }
            } else {
                let _ = hub
                    .tasks()
                    .set_status(task_id, TaskStatus::Blocked, false)
                    .await;
            }
        }
        WorkflowStatus::Escalated => {
            if let Some(wf_id) = workflow_id {
                record_dispatch_history_entry(hub.clone(), task_id, wf_id, "escalated").await;
            }
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
            let _ = hub
                .tasks()
                .set_status(task_id, TaskStatus::Blocked, false)
                .await;
        }
        WorkflowStatus::Cancelled => {
            if let Some(wf_id) = workflow_id {
                record_dispatch_history_entry(hub.clone(), task_id, wf_id, "cancelled").await;
            }
            remove_terminal_em_work_queue_entry_non_fatal(project_root, task_id, workflow_id);
            let _ = hub
                .tasks()
                .set_status(task_id, TaskStatus::Cancelled, false)
                .await;
        }
        WorkflowStatus::Paused | WorkflowStatus::Running | WorkflowStatus::Pending => {
            let _ = hub
                .tasks()
                .set_status(task_id, TaskStatus::InProgress, false)
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
    let candidates = hub.tasks().list_prioritized().await?;
    let task_lookup: std::collections::HashMap<String, orchestrator_core::OrchestratorTask> =
        candidates
            .iter()
            .cloned()
            .map(|task| (task.id.clone(), task))
            .collect();
    let queue_state = match load_em_work_queue_state(project_root) {
        Ok(state) => state,
        Err(error) => {
            eprintln!(
                "{}: failed to load EM work queue state: {}",
                protocol::ACTOR_DAEMON,
                error
            );
            None
        }
    };
    let plan = plan_ready_task_dispatch(&candidates, &workflows, queue_state.as_ref());

    for task_id in &plan.completed_task_ids {
        let _ = hub
            .tasks()
            .set_status(task_id, TaskStatus::Done, false)
            .await;
    }

    let mut started_workflows = Vec::new();
    for planned_start in plan.ordered_starts {
        if started_workflows.len() >= max_tasks_per_tick {
            break;
        }

        let Some(task) = task_lookup.get(planned_start.task_id.as_str()).cloned() else {
            continue;
        };
        let dependency_issues =
            dependency_gate_issues_for_task(hub.clone(), project_root, &task).await;
        if !dependency_issues.is_empty() {
            let reason = dependency_blocked_reason(&dependency_issues);
            let _ = set_task_blocked_with_reason(hub.clone(), &task, reason, None).await;
            continue;
        }

        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(
                task.id.clone(),
                Some(pipeline_for_task(&task)),
            ))
            .await?;
        if planned_start.selection_source == TaskSelectionSource::EmQueue {
            if let Err(error) =
                mark_em_work_queue_entry_assigned(project_root, &task.id, workflow.id.as_str())
            {
                eprintln!(
                    "{}: failed to mark EM queue entry assigned for task {}: {}",
                    protocol::ACTOR_DAEMON,
                    task.id,
                    error
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
        started_workflows.push(ReadyTaskWorkflowStart {
            task_id: task.id.clone(),
            workflow_id: workflow.id.clone(),
            selection_source: planned_start.selection_source,
        });
    }

    Ok(ReadyTaskWorkflowStartSummary {
        started: started_workflows.len(),
        started_workflows,
    })
}
