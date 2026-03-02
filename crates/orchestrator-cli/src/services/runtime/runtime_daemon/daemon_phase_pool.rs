use super::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio::sync::{broadcast, mpsc};

#[derive(Debug)]
pub struct ScheduledPhaseRun {
    pub workflow: orchestrator_core::OrchestratorWorkflow,
    pub task: orchestrator_core::OrchestratorTask,
    pub phase_id: String,
    pub phase_attempt: u32,
    pub execution_cwd: String,
}

#[derive(Debug)]
pub struct ReactivePhaseCompletion {
    pub workflow: orchestrator_core::OrchestratorWorkflow,
    pub task: orchestrator_core::OrchestratorTask,
    pub phase_id: String,
    pub run_result: std::result::Result<PhaseExecutionRunResult, String>,
}

#[derive(Debug)]
pub struct ReactivePhasePoolState {
    pub completion_tx: mpsc::UnboundedSender<ReactivePhaseCompletion>,
    pub completion_rx: mpsc::UnboundedReceiver<ReactivePhaseCompletion>,
    pub in_flight_workflow_ids: HashSet<String>,
    pub allow_spawns: bool,
}

impl ReactivePhasePoolState {
    fn new() -> Self {
        let (completion_tx, completion_rx) = mpsc::unbounded_channel();
        Self {
            completion_tx,
            completion_rx,
            in_flight_workflow_ids: HashSet::new(),
            allow_spawns: true,
        }
    }
}

fn reactive_phase_pools() -> &'static Mutex<HashMap<String, ReactivePhasePoolState>> {
    static POOLS: OnceLock<Mutex<HashMap<String, ReactivePhasePoolState>>> = OnceLock::new();
    POOLS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn phase_completion_wake_sender() -> &'static broadcast::Sender<String> {
    static WAKE: OnceLock<broadcast::Sender<String>> = OnceLock::new();
    WAKE.get_or_init(|| {
        let (tx, _rx) = broadcast::channel(256);
        tx
    })
}

pub fn with_reactive_phase_pool_state_mut<T>(
    project_root: &str,
    f: impl FnOnce(&mut ReactivePhasePoolState) -> T,
) -> T {
    let mut pools = reactive_phase_pools()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = pools
        .entry(project_root.to_string())
        .or_insert_with(ReactivePhasePoolState::new);
    f(state)
}

pub fn subscribe_phase_completion_wake() -> broadcast::Receiver<String> {
    phase_completion_wake_sender().subscribe()
}

pub fn pause_running_workflow_phase_spawns(project_root: &str) {
    with_reactive_phase_pool_state_mut(project_root, |state| {
        state.allow_spawns = false;
    });
}

pub fn resume_running_workflow_phase_spawns(project_root: &str) {
    with_reactive_phase_pool_state_mut(project_root, |state| {
        state.allow_spawns = true;
    });
}

pub fn clear_running_workflow_phase_pool(project_root: &str) {
    let mut pools = reactive_phase_pools()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pools.remove(project_root);
}

pub fn has_running_workflow_phase_pool_activity(project_root: &str) -> bool {
    let pools = reactive_phase_pools()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pools
        .get(project_root)
        .map(|state| !state.in_flight_workflow_ids.is_empty())
        .unwrap_or(false)
}

pub async fn execute_running_workflow_phases_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    max_phases_per_tick: usize,
) -> Result<(usize, usize, Vec<PhaseExecutionEvent>)> {
    let mut executed = 0usize;
    let mut failed = 0usize;
    let mut phase_events = Vec::new();
    let mut completions = Vec::new();
    let completion_poll_limit = max_phases_per_tick.max(1);
    with_reactive_phase_pool_state_mut(project_root, |state| {
        for _ in 0..completion_poll_limit {
            match state.completion_rx.try_recv() {
                Ok(completion) => {
                    state.in_flight_workflow_ids.remove(&completion.workflow.id);
                    completions.push(completion);
                }
                Err(mpsc::error::TryRecvError::Empty)
                | Err(mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
    });

    for completion in completions {
        process_phase_execution_completion(
            hub.clone(),
            project_root,
            completion.workflow,
            completion.task,
            completion.phase_id,
            completion.run_result,
            &mut executed,
            &mut failed,
            &mut phase_events,
        )
        .await?;
    }

    if max_phases_per_tick == 0 {
        return Ok((executed, failed, phase_events));
    }

    let (allow_spawns, mut in_flight_workflow_ids, completion_tx) =
        with_reactive_phase_pool_state_mut(project_root, |state| {
            (
                state.allow_spawns,
                state.in_flight_workflow_ids.clone(),
                state.completion_tx.clone(),
            )
        });
    if !allow_spawns {
        return Ok((executed, failed, phase_events));
    }

    let available_slots = max_phases_per_tick.saturating_sub(in_flight_workflow_ids.len());
    if available_slots == 0 {
        return Ok((executed, failed, phase_events));
    }

    let workflows = hub.workflows().list().await.unwrap_or_default();
    let mut processed = 0usize;
    let mut scheduled_runs: Vec<ScheduledPhaseRun> = Vec::new();
    for workflow in workflows {
        if processed >= max_phases_per_tick || scheduled_runs.len() >= available_slots {
            break;
        }
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        if workflow.machine_state == orchestrator_core::WorkflowMachineState::MergeConflict {
            continue;
        }
        if in_flight_workflow_ids.contains(&workflow.id) {
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

        let execution_cwd = git_ops::ensure_task_execution_cwd(hub.clone(), project_root, &task)
            .await
            .unwrap_or_else(|_| project_root.to_string());
        git_ops::rebase_worktree_on_main(project_root, &execution_cwd);
        in_flight_workflow_ids.insert(workflow.id.clone());
        scheduled_runs.push(ScheduledPhaseRun {
            workflow,
            task,
            phase_id,
            phase_attempt,
            execution_cwd,
        });
        processed = processed.saturating_add(1);
    }

    for scheduled in scheduled_runs {
        let project_root_owned = project_root.to_string();
        let wake_sender = phase_completion_wake_sender().clone();
        let completion_tx = completion_tx.clone();
        with_reactive_phase_pool_state_mut(project_root, |state| {
            state
                .in_flight_workflow_ids
                .insert(scheduled.workflow.id.clone());
        });
        tokio::spawn(async move {
            let run_result = run_workflow_phase_with_agent(
                &project_root_owned,
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
                    &project_root_owned,
                    &scheduled.workflow.id,
                    &result,
                )?;
                Ok(result)
            })
            .map_err(|error| error.to_string());

            let _ = completion_tx.send(ReactivePhaseCompletion {
                workflow: scheduled.workflow,
                task: scheduled.task,
                phase_id: scheduled.phase_id,
                run_result,
            });
            let _ = wake_sender.send(project_root_owned);
        });
    }

    Ok((executed, failed, phase_events))
}

async fn process_phase_execution_completion(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    workflow: orchestrator_core::OrchestratorWorkflow,
    task: orchestrator_core::OrchestratorTask,
    phase_id: String,
    run_result: std::result::Result<PhaseExecutionRunResult, String>,
    executed: &mut usize,
    failed: &mut usize,
    phase_events: &mut Vec<PhaseExecutionEvent>,
) -> Result<()> {
    match run_result {
        Ok(result) => {
            phase_events.extend(phase_execution_events_from_signals(
                project_root,
                &workflow,
                &result.metadata,
                &result.signals,
            ));

            let _ = persist_phase_output(
                project_root,
                &workflow.id,
                &phase_id,
                &result.outcome,
            );

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
                    *executed = executed.saturating_add(1);
                }
                PhaseExecutionOutcome::NeedsResearch { reason } => {
                    if phase_id == "research" {
                        let updated = hub
                            .workflows()
                            .fail_current_phase(
                                &workflow.id,
                                format!("research phase requested additional research: {reason}"),
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
                        *failed = failed.saturating_add(1);
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
                            *executed = executed.saturating_add(1);
                        }
                    }
                }
                PhaseExecutionOutcome::ManualPending { .. } => {
                    // Manual mode waits for explicit CLI approval.
                }
            }
        }
        Err(error_message) => {
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
                        agent_runtime_schema:
                            orchestrator_core::agent_runtime_config::AGENT_RUNTIME_CONFIG_SCHEMA_ID
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
                return Ok(());
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
                AiRecoveryAction::Retry => {}
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
                    *executed = executed.saturating_add(1);
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
                                tags: vec!["ai-decomposed".to_string(), "ai-generated".to_string()],
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
                    *failed = failed.saturating_add(1);
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
                    *failed = failed.saturating_add(1);
                }
            }
        }
    }
    Ok(())
}

pub async fn drain_running_workflow_phases_for_project(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    max_phases_per_tick: usize,
) -> Result<(usize, usize, Vec<PhaseExecutionEvent>)> {
    pause_running_workflow_phase_spawns(project_root);
    let poll_limit = max_phases_per_tick.max(1);
    let mut executed_total = 0usize;
    let mut failed_total = 0usize;
    let mut phase_events = Vec::new();

    loop {
        let (executed, failed, mut events) =
            execute_running_workflow_phases_for_project(hub.clone(), project_root, poll_limit)
                .await?;
        executed_total = executed_total.saturating_add(executed);
        failed_total = failed_total.saturating_add(failed);
        phase_events.append(&mut events);

        if !has_running_workflow_phase_pool_activity(project_root) {
            break;
        }

        if executed == 0 && failed == 0 {
            sleep(Duration::from_millis(25)).await;
        }
    }

    clear_running_workflow_phase_pool(project_root);
    Ok((executed_total, failed_total, phase_events))
}

fn phase_execution_events_from_signals(
    project_root: &str,
    workflow: &orchestrator_core::OrchestratorWorkflow,
    metadata: &PhaseExecutionMetadata,
    signals: &[PhaseExecutionSignal],
) -> Vec<PhaseExecutionEvent> {
    crate::services::runtime::workflow_executor::workflow_runner::phase_execution_events_from_signals(
        project_root,
        workflow,
        metadata,
        signals,
    )
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

use crate::services::runtime::workflow_executor::workflow_runner::{
    AiRecoveryAction, AI_RECOVERY_MARKER,
};

async fn attempt_ai_failure_recovery(
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    phase_id: &str,
    error_message: &str,
    decision_history: &[orchestrator_core::WorkflowDecisionRecord],
) -> AiRecoveryAction {
    crate::services::runtime::workflow_executor::workflow_runner::attempt_ai_failure_recovery(
        project_root,
        task,
        phase_id,
        error_message,
        decision_history,
    )
    .await
}

use crate::services::runtime::workflow_executor::workflow_merge_recovery;

pub async fn attempt_ai_merge_conflict_recovery(
    project_root: &str,
    task: &orchestrator_core::OrchestratorTask,
    context: &git_ops::MergeConflictContext,
) -> Result<()> {
    workflow_merge_recovery::attempt_ai_merge_conflict_recovery(project_root, task, context).await
}

#[cfg(test)]
pub fn parse_merge_conflict_recovery_response(
    text: &str,
) -> Option<workflow_merge_recovery::MergeConflictRecoveryResponse> {
    workflow_merge_recovery::parse_merge_conflict_recovery_response(text)
}
