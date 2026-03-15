use crate::phase_executor::PhaseExecutionMetadata;
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

