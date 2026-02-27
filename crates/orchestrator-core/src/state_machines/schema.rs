use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::types::{RequirementStatus, WorkflowMachineEvent, WorkflowMachineState};

pub const STATE_MACHINES_SCHEMA_ID: &str = "ao.state-machines.v1";
pub const STATE_MACHINES_VERSION: u32 = 1;
pub const DEFAULT_REQUIREMENT_MAX_REWORK_ROUNDS: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMachinesDocument {
    pub schema: String,
    pub version: u32,
    pub workflow: WorkflowMachineDefinition,
    pub requirements_lifecycle: RequirementLifecycleDefinition,
}

impl Default for StateMachinesDocument {
    fn default() -> Self {
        builtin_state_machines_document()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowMachineDefinition {
    pub initial_state: WorkflowMachineState,
    #[serde(default)]
    pub terminal_states: Vec<WorkflowMachineState>,
    #[serde(default)]
    pub transitions: Vec<WorkflowTransitionDefinition>,
    #[serde(default)]
    pub guards: Vec<RegistryEntry>,
    #[serde(default)]
    pub actions: Vec<RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTransitionDefinition {
    pub from: WorkflowMachineState,
    pub event: WorkflowMachineEvent,
    pub to: WorkflowMachineState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequirementLifecycleEvent {
    Refine,
    PoPass,
    PoFail,
    EmPass,
    EmFail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementLifecyclePolicy {
    #[serde(default = "default_requirement_max_rework_rounds")]
    pub max_rework_rounds: usize,
}

impl Default for RequirementLifecyclePolicy {
    fn default() -> Self {
        Self {
            max_rework_rounds: default_requirement_max_rework_rounds(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementLifecycleDefinition {
    pub initial_state: RequirementStatus,
    #[serde(default)]
    pub terminal_states: Vec<RequirementStatus>,
    #[serde(default)]
    pub policy: RequirementLifecyclePolicy,
    #[serde(default)]
    pub transitions: Vec<RequirementLifecycleTransitionDefinition>,
    #[serde(default)]
    pub guards: Vec<RegistryEntry>,
    #[serde(default)]
    pub actions: Vec<RegistryEntry>,
    #[serde(default)]
    pub comment_templates: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementLifecycleTransitionDefinition {
    pub from: RequirementStatus,
    pub event: RequirementLifecycleEvent,
    pub to: RequirementStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

fn default_requirement_max_rework_rounds() -> usize {
    DEFAULT_REQUIREMENT_MAX_REWORK_ROUNDS
}

fn default_requirement_comment_templates() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "refine".to_string(),
            "Requirement refined and prepared for PO/EM review pipeline.".to_string(),
        ),
        (
            "po_approved".to_string(),
            "PO review approved requirement scope and outcome alignment.".to_string(),
        ),
        (
            "em_approved".to_string(),
            "EM review approved implementation readiness and quality gates.".to_string(),
        ),
        (
            "approved".to_string(),
            "Requirement approved for task materialization and workflow execution.".to_string(),
        ),
    ])
}

pub fn builtin_state_machines_document() -> StateMachinesDocument {
    StateMachinesDocument {
        schema: STATE_MACHINES_SCHEMA_ID.to_string(),
        version: STATE_MACHINES_VERSION,
        workflow: WorkflowMachineDefinition {
            initial_state: WorkflowMachineState::Idle,
            terminal_states: vec![
                WorkflowMachineState::Completed,
                WorkflowMachineState::Failed,
                WorkflowMachineState::Cancelled,
            ],
            transitions: vec![
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::Idle,
                    event: WorkflowMachineEvent::Start,
                    to: WorkflowMachineState::EvaluateTransition,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::Idle,
                    event: WorkflowMachineEvent::PauseRequested,
                    to: WorkflowMachineState::Paused,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::Idle,
                    event: WorkflowMachineEvent::CancelRequested,
                    to: WorkflowMachineState::Cancelled,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateTransition,
                    event: WorkflowMachineEvent::PhaseStarted,
                    to: WorkflowMachineState::RunPhase,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateTransition,
                    event: WorkflowMachineEvent::NoMorePhases,
                    to: WorkflowMachineState::Completed,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateTransition,
                    event: WorkflowMachineEvent::PauseRequested,
                    to: WorkflowMachineState::Paused,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateTransition,
                    event: WorkflowMachineEvent::CancelRequested,
                    to: WorkflowMachineState::Cancelled,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateTransition,
                    event: WorkflowMachineEvent::ReworkBudgetExceeded,
                    to: WorkflowMachineState::Failed,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::RunPhase,
                    event: WorkflowMachineEvent::PhaseSucceeded,
                    to: WorkflowMachineState::EvaluateGates,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::RunPhase,
                    event: WorkflowMachineEvent::PhaseFailed,
                    to: WorkflowMachineState::EvaluateGates,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::RunPhase,
                    event: WorkflowMachineEvent::PauseRequested,
                    to: WorkflowMachineState::Paused,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::RunPhase,
                    event: WorkflowMachineEvent::CancelRequested,
                    to: WorkflowMachineState::Cancelled,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateGates,
                    event: WorkflowMachineEvent::GatesPassed,
                    to: WorkflowMachineState::ApplyTransition,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateGates,
                    event: WorkflowMachineEvent::GatesFailed,
                    to: WorkflowMachineState::ApplyTransition,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateGates,
                    event: WorkflowMachineEvent::PolicyDecisionReady,
                    to: WorkflowMachineState::ApplyTransition,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateGates,
                    event: WorkflowMachineEvent::PolicyDecisionFailed,
                    to: WorkflowMachineState::ApplyTransition,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateGates,
                    event: WorkflowMachineEvent::PauseRequested,
                    to: WorkflowMachineState::Paused,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateGates,
                    event: WorkflowMachineEvent::CancelRequested,
                    to: WorkflowMachineState::Cancelled,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::EvaluateGates,
                    event: WorkflowMachineEvent::ReworkBudgetExceeded,
                    to: WorkflowMachineState::Failed,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::ApplyTransition,
                    event: WorkflowMachineEvent::Start,
                    to: WorkflowMachineState::EvaluateTransition,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::ApplyTransition,
                    event: WorkflowMachineEvent::NoMorePhases,
                    to: WorkflowMachineState::Completed,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::ApplyTransition,
                    event: WorkflowMachineEvent::PhaseStarted,
                    to: WorkflowMachineState::RunPhase,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::ApplyTransition,
                    event: WorkflowMachineEvent::PauseRequested,
                    to: WorkflowMachineState::Paused,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::ApplyTransition,
                    event: WorkflowMachineEvent::CancelRequested,
                    to: WorkflowMachineState::Cancelled,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::ApplyTransition,
                    event: WorkflowMachineEvent::ReworkBudgetExceeded,
                    to: WorkflowMachineState::Failed,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::Paused,
                    event: WorkflowMachineEvent::ResumeRequested,
                    to: WorkflowMachineState::EvaluateTransition,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::Paused,
                    event: WorkflowMachineEvent::CancelRequested,
                    to: WorkflowMachineState::Cancelled,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::Failed,
                    event: WorkflowMachineEvent::ResumeRequested,
                    to: WorkflowMachineState::EvaluateTransition,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::Failed,
                    event: WorkflowMachineEvent::CancelRequested,
                    to: WorkflowMachineState::Cancelled,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::Completed,
                    event: WorkflowMachineEvent::MergeConflictDetected,
                    to: WorkflowMachineState::MergeConflict,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::MergeConflict,
                    event: WorkflowMachineEvent::CancelRequested,
                    to: WorkflowMachineState::Cancelled,
                    guard: None,
                    action: None,
                },
                WorkflowTransitionDefinition {
                    from: WorkflowMachineState::MergeConflict,
                    event: WorkflowMachineEvent::MergeConflictResolved,
                    to: WorkflowMachineState::Completed,
                    guard: None,
                    action: None,
                },
            ],
            guards: vec![RegistryEntry {
                id: "rework_budget_available".to_string(),
            }],
            actions: vec![
                RegistryEntry {
                    id: "append_decision_record".to_string(),
                },
                RegistryEntry {
                    id: "set_workflow_status_from_state".to_string(),
                },
            ],
        },
        requirements_lifecycle: RequirementLifecycleDefinition {
            initial_state: RequirementStatus::Draft,
            terminal_states: vec![
                RequirementStatus::Approved,
                RequirementStatus::Deprecated,
                RequirementStatus::Implemented,
                RequirementStatus::Done,
            ],
            policy: RequirementLifecyclePolicy::default(),
            transitions: vec![
                RequirementLifecycleTransitionDefinition {
                    from: RequirementStatus::Draft,
                    event: RequirementLifecycleEvent::Refine,
                    to: RequirementStatus::Refined,
                    guard: None,
                    action: Some("add_refine_comment".to_string()),
                },
                RequirementLifecycleTransitionDefinition {
                    from: RequirementStatus::Refined,
                    event: RequirementLifecycleEvent::Refine,
                    to: RequirementStatus::Refined,
                    guard: None,
                    action: Some("add_refine_comment".to_string()),
                },
                RequirementLifecycleTransitionDefinition {
                    from: RequirementStatus::PoReview,
                    event: RequirementLifecycleEvent::Refine,
                    to: RequirementStatus::Refined,
                    guard: None,
                    action: Some("add_refine_comment".to_string()),
                },
                RequirementLifecycleTransitionDefinition {
                    from: RequirementStatus::EmReview,
                    event: RequirementLifecycleEvent::Refine,
                    to: RequirementStatus::Refined,
                    guard: None,
                    action: Some("add_refine_comment".to_string()),
                },
                RequirementLifecycleTransitionDefinition {
                    from: RequirementStatus::NeedsRework,
                    event: RequirementLifecycleEvent::Refine,
                    to: RequirementStatus::Refined,
                    guard: None,
                    action: Some("add_refine_comment".to_string()),
                },
                RequirementLifecycleTransitionDefinition {
                    from: RequirementStatus::Refined,
                    event: RequirementLifecycleEvent::PoPass,
                    to: RequirementStatus::PoReview,
                    guard: None,
                    action: None,
                },
                RequirementLifecycleTransitionDefinition {
                    from: RequirementStatus::PoReview,
                    event: RequirementLifecycleEvent::PoPass,
                    to: RequirementStatus::EmReview,
                    guard: None,
                    action: Some("add_po_approval_comment".to_string()),
                },
                RequirementLifecycleTransitionDefinition {
                    from: RequirementStatus::PoReview,
                    event: RequirementLifecycleEvent::PoFail,
                    to: RequirementStatus::NeedsRework,
                    guard: Some("rework_budget_available".to_string()),
                    action: Some("add_rework_comment".to_string()),
                },
                RequirementLifecycleTransitionDefinition {
                    from: RequirementStatus::EmReview,
                    event: RequirementLifecycleEvent::EmPass,
                    to: RequirementStatus::Approved,
                    guard: None,
                    action: Some("add_em_approval_comment".to_string()),
                },
                RequirementLifecycleTransitionDefinition {
                    from: RequirementStatus::EmReview,
                    event: RequirementLifecycleEvent::EmFail,
                    to: RequirementStatus::NeedsRework,
                    guard: Some("rework_budget_available".to_string()),
                    action: Some("add_rework_comment".to_string()),
                },
            ],
            guards: vec![RegistryEntry {
                id: "rework_budget_available".to_string(),
            }],
            actions: vec![
                RegistryEntry {
                    id: "add_refine_comment".to_string(),
                },
                RegistryEntry {
                    id: "add_po_approval_comment".to_string(),
                },
                RegistryEntry {
                    id: "add_em_approval_comment".to_string(),
                },
                RegistryEntry {
                    id: "add_rework_comment".to_string(),
                },
            ],
            comment_templates: default_requirement_comment_templates(),
        },
    }
}
