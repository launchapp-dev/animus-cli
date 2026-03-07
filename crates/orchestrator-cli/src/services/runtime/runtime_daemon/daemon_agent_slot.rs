//! DEPRECATED: Agent lifecycle now owned by workflow-runner.
use super::*;
use ::workflow_runner::executor::{
    phase_execution_events_from_signals, AiRecoveryAction, PhaseExecutionEvent,
    PhaseExecutionMetadata, PhaseExecutionRunResult, AI_RECOVERY_MARKER,
};
#[derive(Debug)]
pub struct AgentSlotResult {
    pub executed: usize,
    pub failed: usize,
    pub phase_events: Vec<PhaseExecutionEvent>,
}

impl AgentSlotResult {
    fn empty() -> Self {
        Self {
            executed: 0,
            failed: 0,
            phase_events: Vec::new(),
        }
    }
}

pub async fn process_agent_result(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    workflow: orchestrator_core::OrchestratorWorkflow,
    task: orchestrator_core::OrchestratorTask,
    phase_id: String,
    run_result: std::result::Result<PhaseExecutionRunResult, String>,
) -> Result<AgentSlotResult> {
    let mut result = AgentSlotResult::empty();

    match run_result {
        Ok(run) => {
            result
                .phase_events
                .extend(phase_execution_events_from_signals(
                    project_root,
                    &workflow,
                    &run.metadata,
                    &run.signals,
                ));

            let _ = persist_phase_output(project_root, &workflow.id, &phase_id, &run.outcome);

            match run.outcome {
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
                    result.executed = result.executed.saturating_add(1);
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
                        result.failed = result.failed.saturating_add(1);
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
                            result.executed = result.executed.saturating_add(1);
                        }
                    }
                }
                PhaseExecutionOutcome::ManualPending { .. } => {}
            }
        }
        Err(error_message) => {
            if error_message.contains("contract violation")
                || error_message.contains("schema validation failed")
                || error_message.contains("payload kind mismatch")
            {
                result.phase_events.push(PhaseExecutionEvent {
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
                return Ok(result);
            }

            let recovery = ::workflow_runner::executor::attempt_ai_failure_recovery(
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
                    result.executed = result.executed.saturating_add(1);
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
                    result.failed = result.failed.saturating_add(1);
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
                    result.failed = result.failed.saturating_add(1);
                }
            }
        }
    }
    Ok(result)
}
