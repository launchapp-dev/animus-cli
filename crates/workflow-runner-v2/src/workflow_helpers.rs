use crate::ipc::{collect_json_payload_lines, run_prompt_against_runner};
use crate::phase_executor::{PhaseExecutionMetadata, PhaseExecutionSignal};
use protocol::{default_primary_model_for_phase, tool_for_model_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseExecutionEvent {
    pub event_type: String,
    pub project_root: String,
    pub workflow_id: String,
    pub task_id: String,
    pub phase_id: String,
    pub phase_mode: String,
    pub metadata: PhaseExecutionMetadata,
    pub payload: Value,
}

pub const AI_RECOVERY_TIMEOUT_SECS: u64 = 120;
pub const AI_RECOVERY_MARKER: &str = "ai-failure-recovery";
pub const MAX_DECOMPOSE_SUBTASKS: usize = 3;

#[derive(Debug, Clone, Deserialize)]
pub struct AiRecoverySubtask {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

pub enum AiRecoveryAction {
    Retry,
    Decompose(Vec<AiRecoverySubtask>),
    SkipPhase,
    Fail,
}

pub fn task_requires_research(task: &orchestrator_core::OrchestratorTask) -> bool {
    if task.workflow_metadata.requires_architecture {
        return true;
    }

    if task.tags.iter().any(|tag| {
        matches!(
            tag.trim().to_ascii_lowercase().as_str(),
            "needs-research" | "research" | "discovery" | "investigation" | "spike"
        )
    }) {
        return true;
    }

    let haystack = format!("{} {}", task.title, task.description).to_ascii_lowercase();
    [
        "research",
        "investigate",
        "evaluate",
        "compare",
        "benchmark",
        "unknown",
        "spike",
        "decision record",
        "validate approach",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
}

pub fn workflow_has_completed_research(workflow: &orchestrator_core::OrchestratorWorkflow) -> bool {
    workflow.phases.iter().any(|phase| {
        phase.phase_id == "research"
            && phase.status == orchestrator_core::WorkflowPhaseStatus::Success
    })
}

pub fn workflow_has_active_research(workflow: &orchestrator_core::OrchestratorWorkflow) -> bool {
    workflow.phases.iter().any(|phase| {
        phase.phase_id == "research"
            && matches!(
                phase.status,
                orchestrator_core::WorkflowPhaseStatus::Pending
                    | orchestrator_core::WorkflowPhaseStatus::Ready
                    | orchestrator_core::WorkflowPhaseStatus::Running
            )
    })
}

pub fn phase_execution_events_from_signals(
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

pub async fn attempt_ai_failure_recovery(
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

    let impl_caps = protocol::PhaseCapabilities::defaults_for_phase("implementation");
    let model = default_primary_model_for_phase(None, &impl_caps).to_string();
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

#[derive(Debug, Clone, Deserialize)]
struct AiRecoveryResponse {
    action: String,
    #[serde(default)]
    subtasks: Vec<AiRecoverySubtask>,
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
