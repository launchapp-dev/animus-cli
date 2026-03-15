use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::types::{RequirementStatus, WorkflowMachineEvent, WorkflowMachineState};

use super::schema::{
    RequirementLifecycleDefinition, RequirementLifecycleEvent, RequirementLifecycleTransitionDefinition,
    StateMachinesDocument, WorkflowMachineDefinition, WorkflowTransitionDefinition,
};
use super::validator::validate_state_machines_document;

#[derive(Debug, Clone)]
pub enum TransitionError {
    NoTransition { from: WorkflowMachineState, event: WorkflowMachineEvent },
    GuardBlocked { from: WorkflowMachineState, event: WorkflowMachineEvent, guard: String },
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTransition { from, event } => {
                write!(f, "no transition from {from:?} on {event:?}")
            }
            Self::GuardBlocked { from, event, guard } => {
                write!(f, "guard '{guard}' blocked transition from {from:?} on {event:?}")
            }
        }
    }
}

impl std::error::Error for TransitionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineSource {
    Json,
    Builtin,
    BuiltinFallback,
}

impl MachineSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Builtin => "builtin",
            Self::BuiltinFallback => "builtin_fallback",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineMetadata {
    pub schema: String,
    pub version: u32,
    pub hash: String,
    pub source: MachineSource,
}

#[derive(Debug, Clone)]
pub struct CompiledStateMachines {
    pub workflow: CompiledWorkflowMachine,
    pub requirements_lifecycle: CompiledRequirementLifecycleMachine,
    pub metadata: MachineMetadata,
    pub document: StateMachinesDocument,
}

#[derive(Debug, Clone)]
pub struct CompiledWorkflowMachine {
    initial_state: WorkflowMachineState,
    terminal_states: HashSet<WorkflowMachineState>,
    transitions: Vec<CompiledWorkflowTransition>,
    metadata: MachineMetadata,
}

#[derive(Debug, Clone)]
struct CompiledWorkflowTransition {
    from: WorkflowMachineState,
    event: WorkflowMachineEvent,
    to: WorkflowMachineState,
    guard: Option<String>,
    action: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompiledRequirementLifecycleMachine {
    initial_state: RequirementStatus,
    terminal_states: HashSet<RequirementStatus>,
    policy_max_rework_rounds: usize,
    transitions: Vec<CompiledRequirementTransition>,
    comment_templates: BTreeMap<String, String>,
    metadata: MachineMetadata,
}

#[derive(Debug, Clone)]
struct CompiledRequirementTransition {
    from: RequirementStatus,
    event: RequirementLifecycleEvent,
    to: RequirementStatus,
    guard: Option<String>,
    action: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkflowTransitionOutcome {
    pub from: WorkflowMachineState,
    pub event: WorkflowMachineEvent,
    pub to: WorkflowMachineState,
    pub matched: bool,
    pub guard_passed: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
pub struct RequirementTransitionOutcome {
    pub from: RequirementStatus,
    pub event: RequirementLifecycleEvent,
    pub to: RequirementStatus,
    pub matched: bool,
    pub guard_passed: Option<bool>,
}

pub struct GuardContext<'a> {
    pub phase_id: &'a str,
    pub rework_counts: &'a HashMap<String, u32>,
    pub max_reworks_for_phase: u32,
}

pub fn evaluate_guard(guard_id: &str, context: &GuardContext) -> bool {
    match guard_id {
        "rework_budget_available" => {
            let count = context.rework_counts.get(context.phase_id).copied().unwrap_or(0);
            count < context.max_reworks_for_phase
        }
        _ => true,
    }
}

pub fn compile_state_machines_document(
    document: StateMachinesDocument,
    source: MachineSource,
) -> Result<CompiledStateMachines> {
    validate_state_machines_document(&document)?;

    let hash = document_hash(&document);
    let metadata = MachineMetadata { schema: document.schema.clone(), version: document.version, hash, source };

    let workflow = compile_workflow_machine(&document.workflow, &metadata);
    let requirements_lifecycle = compile_requirements_machine(&document.requirements_lifecycle, &metadata);

    Ok(CompiledStateMachines { workflow, requirements_lifecycle, metadata, document })
}

impl CompiledWorkflowMachine {
    pub fn initial_state(&self) -> WorkflowMachineState {
        self.initial_state
    }

    pub fn is_terminal(&self, state: WorkflowMachineState) -> bool {
        self.terminal_states.contains(&state)
    }

    pub fn metadata(&self) -> &MachineMetadata {
        &self.metadata
    }

    pub fn apply(
        &self,
        current: WorkflowMachineState,
        event: WorkflowMachineEvent,
        mut guard_evaluator: impl FnMut(&str) -> bool,
    ) -> std::result::Result<WorkflowTransitionOutcome, TransitionError> {
        let mut blocked_guard: Option<String> = None;
        let mut had_guard = false;

        for transition in &self.transitions {
            if transition.from != current || transition.event != event {
                continue;
            }

            if let Some(guard_id) = transition.guard.as_deref() {
                had_guard = true;
                let allowed = guard_evaluator(guard_id);
                if !allowed {
                    if blocked_guard.is_none() {
                        blocked_guard = Some(guard_id.to_string());
                    }
                    continue;
                }
            }

            let guard_passed = if had_guard { Some(true) } else { None };
            return Ok(WorkflowTransitionOutcome {
                from: current,
                event,
                to: transition.to,
                matched: true,
                guard_passed,
            });
        }

        if let Some(guard) = blocked_guard {
            Err(TransitionError::GuardBlocked { from: current, event, guard })
        } else {
            Err(TransitionError::NoTransition { from: current, event })
        }
    }

    pub fn actions_for(
        &self,
        from: WorkflowMachineState,
        event: WorkflowMachineEvent,
        to: WorkflowMachineState,
    ) -> Vec<&str> {
        self.transitions
            .iter()
            .filter(|transition| transition.from == from && transition.event == event && transition.to == to)
            .filter_map(|transition| transition.action.as_deref())
            .collect()
    }
}

impl CompiledRequirementLifecycleMachine {
    pub fn initial_state(&self) -> RequirementStatus {
        self.initial_state
    }

    pub fn is_terminal(&self, state: RequirementStatus) -> bool {
        self.terminal_states.contains(&state)
    }

    pub fn max_rework_rounds(&self) -> usize {
        self.policy_max_rework_rounds.max(1)
    }

    pub fn metadata(&self) -> &MachineMetadata {
        &self.metadata
    }

    pub fn comment_template(&self, key: &str) -> Option<&str> {
        self.comment_templates.get(key).map(String::as_str)
    }

    pub fn apply(
        &self,
        current: RequirementStatus,
        event: RequirementLifecycleEvent,
        mut guard_evaluator: impl FnMut(&str) -> bool,
    ) -> RequirementTransitionOutcome {
        let mut first_guard_result = None;

        for transition in &self.transitions {
            if transition.from != current || transition.event != event {
                continue;
            }

            if let Some(guard_id) = transition.guard.as_deref() {
                let allowed = guard_evaluator(guard_id);
                if first_guard_result.is_none() {
                    first_guard_result = Some(allowed);
                }
                if !allowed {
                    continue;
                }
            }

            return RequirementTransitionOutcome {
                from: current,
                event,
                to: transition.to,
                matched: true,
                guard_passed: first_guard_result,
            };
        }

        RequirementTransitionOutcome {
            from: current,
            event,
            to: current,
            matched: false,
            guard_passed: first_guard_result,
        }
    }

    pub fn actions_for(
        &self,
        from: RequirementStatus,
        event: RequirementLifecycleEvent,
        to: RequirementStatus,
    ) -> Vec<&str> {
        self.transitions
            .iter()
            .filter(|transition| transition.from == from && transition.event == event && transition.to == to)
            .filter_map(|transition| transition.action.as_deref())
            .collect()
    }
}

fn compile_workflow_machine(
    definition: &WorkflowMachineDefinition,
    metadata: &MachineMetadata,
) -> CompiledWorkflowMachine {
    CompiledWorkflowMachine {
        initial_state: definition.initial_state,
        terminal_states: definition.terminal_states.iter().copied().collect(),
        transitions: definition.transitions.iter().map(compile_workflow_transition).collect(),
        metadata: metadata.clone(),
    }
}

fn compile_workflow_transition(transition: &WorkflowTransitionDefinition) -> CompiledWorkflowTransition {
    CompiledWorkflowTransition {
        from: transition.from,
        event: transition.event,
        to: transition.to,
        guard: transition.guard.clone(),
        action: transition.action.clone(),
    }
}

fn compile_requirements_machine(
    definition: &RequirementLifecycleDefinition,
    metadata: &MachineMetadata,
) -> CompiledRequirementLifecycleMachine {
    CompiledRequirementLifecycleMachine {
        initial_state: definition.initial_state,
        terminal_states: definition.terminal_states.iter().copied().collect(),
        policy_max_rework_rounds: definition.policy.max_rework_rounds.max(1),
        transitions: definition.transitions.iter().map(compile_requirement_transition).collect(),
        comment_templates: definition.comment_templates.clone(),
        metadata: metadata.clone(),
    }
}

fn compile_requirement_transition(
    transition: &RequirementLifecycleTransitionDefinition,
) -> CompiledRequirementTransition {
    CompiledRequirementTransition {
        from: transition.from,
        event: transition.event,
        to: transition.to,
        guard: transition.guard.clone(),
        action: transition.action.clone(),
    }
}

fn document_hash(document: &StateMachinesDocument) -> String {
    let bytes = serde_json::to_vec(document).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machines::schema::{builtin_state_machines_document, RequirementLifecycleEvent};

    #[test]
    fn compile_builtin_document() {
        let compiled = compile_state_machines_document(builtin_state_machines_document(), MachineSource::Builtin)
            .expect("compile should succeed");

        assert_eq!(compiled.workflow.initial_state(), WorkflowMachineState::Idle);
        assert_eq!(compiled.requirements_lifecycle.initial_state(), RequirementStatus::Draft);
        assert_eq!(compiled.metadata.source, MachineSource::Builtin);
        assert!(!compiled.metadata.hash.trim().is_empty());
    }

    #[test]
    fn builtin_workflow_machine_marks_merge_conflict_as_non_terminal() {
        let compiled = compile_state_machines_document(builtin_state_machines_document(), MachineSource::Builtin)
            .expect("compile should succeed");

        assert!(!compiled.workflow.is_terminal(WorkflowMachineState::MergeConflict));
        assert!(compiled.workflow.is_terminal(WorkflowMachineState::Completed));
        assert!(compiled.workflow.is_terminal(WorkflowMachineState::Failed));
    }

    #[test]
    fn workflow_apply_uses_ordered_first_match() {
        let compiled = compile_state_machines_document(builtin_state_machines_document(), MachineSource::Builtin)
            .expect("compile should succeed");

        let outcome = compiled
            .workflow
            .apply(WorkflowMachineState::Idle, WorkflowMachineEvent::Start, |_| true)
            .expect("test: Idle + Start should transition");
        assert!(outcome.matched);
        assert_eq!(outcome.to, WorkflowMachineState::EvaluateTransition);
    }

    #[test]
    fn requirement_guard_blocks_transition_when_budget_exceeded() {
        let compiled = compile_state_machines_document(builtin_state_machines_document(), MachineSource::Builtin)
            .expect("compile should succeed");

        let outcome = compiled.requirements_lifecycle.apply(
            RequirementStatus::PoReview,
            RequirementLifecycleEvent::PoFail,
            |_| false,
        );

        assert!(!outcome.matched);
        assert_eq!(outcome.to, RequirementStatus::PoReview);
        assert_eq!(outcome.guard_passed, Some(false));
    }

    #[test]
    fn evaluate_guard_rework_budget_available_when_under_limit() {
        let mut rework_counts = HashMap::new();
        rework_counts.insert("implementation".to_string(), 1);
        let context =
            GuardContext { phase_id: "implementation", rework_counts: &rework_counts, max_reworks_for_phase: 3 };
        assert!(evaluate_guard("rework_budget_available", &context));
    }

    #[test]
    fn evaluate_guard_rework_budget_available_when_at_limit() {
        let mut rework_counts = HashMap::new();
        rework_counts.insert("implementation".to_string(), 3);
        let context =
            GuardContext { phase_id: "implementation", rework_counts: &rework_counts, max_reworks_for_phase: 3 };
        assert!(!evaluate_guard("rework_budget_available", &context));
    }

    #[test]
    fn evaluate_guard_rework_budget_available_when_over_limit() {
        let mut rework_counts = HashMap::new();
        rework_counts.insert("implementation".to_string(), 5);
        let context =
            GuardContext { phase_id: "implementation", rework_counts: &rework_counts, max_reworks_for_phase: 3 };
        assert!(!evaluate_guard("rework_budget_available", &context));
    }

    #[test]
    fn evaluate_guard_rework_budget_available_when_no_reworks_yet() {
        let rework_counts = HashMap::new();
        let context =
            GuardContext { phase_id: "implementation", rework_counts: &rework_counts, max_reworks_for_phase: 3 };
        assert!(evaluate_guard("rework_budget_available", &context));
    }

    #[test]
    fn evaluate_guard_rework_budget_available_with_zero_max() {
        let rework_counts = HashMap::new();
        let context =
            GuardContext { phase_id: "implementation", rework_counts: &rework_counts, max_reworks_for_phase: 0 };
        assert!(!evaluate_guard("rework_budget_available", &context));
    }

    #[test]
    fn evaluate_guard_unknown_guard_passes() {
        let rework_counts = HashMap::new();
        let context =
            GuardContext { phase_id: "implementation", rework_counts: &rework_counts, max_reworks_for_phase: 3 };
        assert!(evaluate_guard("unknown_guard", &context));
    }

    #[test]
    fn evaluate_guard_checks_correct_phase() {
        let mut rework_counts = HashMap::new();
        rework_counts.insert("code-review".to_string(), 5);
        let context =
            GuardContext { phase_id: "implementation", rework_counts: &rework_counts, max_reworks_for_phase: 3 };
        assert!(evaluate_guard("rework_budget_available", &context));
    }

    #[test]
    fn requirement_lifecycle_uses_evaluate_guard_for_rework_budget() {
        let compiled = compile_state_machines_document(builtin_state_machines_document(), MachineSource::Builtin)
            .expect("compile should succeed");

        let mut rework_counts = HashMap::new();
        rework_counts.insert("po_review".to_string(), 0);

        let outcome = compiled.requirements_lifecycle.apply(
            RequirementStatus::PoReview,
            RequirementLifecycleEvent::PoFail,
            |guard_id| {
                let context =
                    GuardContext { phase_id: "po_review", rework_counts: &rework_counts, max_reworks_for_phase: 3 };
                evaluate_guard(guard_id, &context)
            },
        );
        assert!(outcome.matched);
        assert_eq!(outcome.to, RequirementStatus::NeedsRework);
        assert_eq!(outcome.guard_passed, Some(true));
    }

    #[test]
    fn requirement_lifecycle_blocks_rework_when_budget_exceeded() {
        let compiled = compile_state_machines_document(builtin_state_machines_document(), MachineSource::Builtin)
            .expect("compile should succeed");

        let mut rework_counts = HashMap::new();
        rework_counts.insert("po_review".to_string(), 3);

        let outcome = compiled.requirements_lifecycle.apply(
            RequirementStatus::PoReview,
            RequirementLifecycleEvent::PoFail,
            |guard_id| {
                let context =
                    GuardContext { phase_id: "po_review", rework_counts: &rework_counts, max_reworks_for_phase: 3 };
                evaluate_guard(guard_id, &context)
            },
        );
        assert!(!outcome.matched);
        assert_eq!(outcome.to, RequirementStatus::PoReview);
        assert_eq!(outcome.guard_passed, Some(false));
    }

    fn builtin_workflow() -> CompiledWorkflowMachine {
        compile_state_machines_document(builtin_state_machines_document(), MachineSource::Builtin)
            .expect("compile should succeed")
            .workflow
    }

    #[test]
    fn no_transition_idle_phase_succeeded() {
        let wf = builtin_workflow();
        let result = wf.apply(WorkflowMachineState::Idle, WorkflowMachineEvent::PhaseSucceeded, |_| true);
        assert!(matches!(result, Err(TransitionError::NoTransition { .. })));
    }

    #[test]
    fn no_transition_run_phase_start() {
        let wf = builtin_workflow();
        let result = wf.apply(WorkflowMachineState::RunPhase, WorkflowMachineEvent::Start, |_| true);
        assert!(matches!(result, Err(TransitionError::NoTransition { .. })));
    }

    #[test]
    fn no_transition_completed_phase_started() {
        let wf = builtin_workflow();
        let result = wf.apply(WorkflowMachineState::Completed, WorkflowMachineEvent::PhaseStarted, |_| true);
        assert!(matches!(result, Err(TransitionError::NoTransition { .. })));
    }

    #[test]
    fn no_transition_failed_gates_passed() {
        let wf = builtin_workflow();
        let result = wf.apply(WorkflowMachineState::Failed, WorkflowMachineEvent::GatesPassed, |_| true);
        assert!(matches!(result, Err(TransitionError::NoTransition { .. })));
    }

    #[test]
    fn no_transition_cancelled_start() {
        let wf = builtin_workflow();
        let result = wf.apply(WorkflowMachineState::Cancelled, WorkflowMachineEvent::Start, |_| true);
        assert!(matches!(result, Err(TransitionError::NoTransition { .. })));
    }

    #[test]
    fn no_transition_paused_phase_succeeded() {
        let wf = builtin_workflow();
        let result = wf.apply(WorkflowMachineState::Paused, WorkflowMachineEvent::PhaseSucceeded, |_| true);
        assert!(matches!(result, Err(TransitionError::NoTransition { .. })));
    }

    #[test]
    fn valid_idle_start() {
        let wf = builtin_workflow();
        let outcome =
            wf.apply(WorkflowMachineState::Idle, WorkflowMachineEvent::Start, |_| true).expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::EvaluateTransition);
    }

    #[test]
    fn valid_evaluate_transition_phase_started() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::EvaluateTransition, WorkflowMachineEvent::PhaseStarted, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::RunPhase);
    }

    #[test]
    fn valid_run_phase_phase_succeeded() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::RunPhase, WorkflowMachineEvent::PhaseSucceeded, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::EvaluateGates);
    }

    #[test]
    fn valid_evaluate_gates_gates_passed() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::EvaluateGates, WorkflowMachineEvent::GatesPassed, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::ApplyTransition);
    }

    #[test]
    fn valid_apply_transition_no_more_phases() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::ApplyTransition, WorkflowMachineEvent::NoMorePhases, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::Completed);
    }

    #[test]
    fn valid_run_phase_phase_failed() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::RunPhase, WorkflowMachineEvent::PhaseFailed, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::EvaluateGates);
    }

    #[test]
    fn valid_run_phase_phase_skipped() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::RunPhase, WorkflowMachineEvent::PhaseSkipped, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::EvaluateTransition);
    }

    #[test]
    fn valid_pause_from_idle() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::Idle, WorkflowMachineEvent::PauseRequested, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::Paused);
    }

    #[test]
    fn valid_pause_from_run_phase() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::RunPhase, WorkflowMachineEvent::PauseRequested, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::Paused);
    }

    #[test]
    fn valid_pause_from_evaluate_gates() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::EvaluateGates, WorkflowMachineEvent::PauseRequested, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::Paused);
    }

    #[test]
    fn valid_cancel_from_idle() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::Idle, WorkflowMachineEvent::CancelRequested, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::Cancelled);
    }

    #[test]
    fn valid_cancel_from_run_phase() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::RunPhase, WorkflowMachineEvent::CancelRequested, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::Cancelled);
    }

    #[test]
    fn valid_cancel_from_paused() {
        let wf = builtin_workflow();
        let outcome = wf
            .apply(WorkflowMachineState::Paused, WorkflowMachineEvent::CancelRequested, |_| true)
            .expect("should transition");
        assert_eq!(outcome.to, WorkflowMachineState::Cancelled);
    }

    #[test]
    fn guard_blocked_returns_error() {
        let mut doc = builtin_state_machines_document();
        doc.workflow
            .transitions
            .retain(|t| !(t.from == WorkflowMachineState::Idle && t.event == WorkflowMachineEvent::Start));
        doc.workflow.transitions.insert(
            0,
            WorkflowTransitionDefinition {
                from: WorkflowMachineState::Idle,
                event: WorkflowMachineEvent::Start,
                to: WorkflowMachineState::EvaluateTransition,
                guard: Some("rework_budget_available".to_string()),
                action: None,
            },
        );

        let compiled = compile_state_machines_document(doc, MachineSource::Builtin).expect("compile should succeed");

        let result = compiled.workflow.apply(WorkflowMachineState::Idle, WorkflowMachineEvent::Start, |_| false);

        match result {
            Err(TransitionError::GuardBlocked { from, event, guard }) => {
                assert_eq!(from, WorkflowMachineState::Idle);
                assert_eq!(event, WorkflowMachineEvent::Start);
                assert_eq!(guard, "rework_budget_available");
            }
            other => panic!("expected GuardBlocked, got {:?}", other),
        }
    }

    #[test]
    fn guard_blocked_falls_through_to_unguarded() {
        let mut doc = builtin_state_machines_document();
        doc.workflow.transitions.insert(
            0,
            WorkflowTransitionDefinition {
                from: WorkflowMachineState::Idle,
                event: WorkflowMachineEvent::Start,
                to: WorkflowMachineState::Failed,
                guard: Some("rework_budget_available".to_string()),
                action: None,
            },
        );

        let compiled = compile_state_machines_document(doc, MachineSource::Builtin).expect("compile should succeed");

        let outcome = compiled
            .workflow
            .apply(WorkflowMachineState::Idle, WorkflowMachineEvent::Start, |_| false)
            .expect("should fall through to unguarded transition");

        assert_eq!(outcome.to, WorkflowMachineState::EvaluateTransition);
    }

    #[test]
    fn full_lifecycle_happy_path() {
        let wf = builtin_workflow();
        let mut state = WorkflowMachineState::Idle;

        let steps: &[(WorkflowMachineEvent, WorkflowMachineState)] = &[
            (WorkflowMachineEvent::Start, WorkflowMachineState::EvaluateTransition),
            (WorkflowMachineEvent::PhaseStarted, WorkflowMachineState::RunPhase),
            (WorkflowMachineEvent::PhaseSucceeded, WorkflowMachineState::EvaluateGates),
            (WorkflowMachineEvent::GatesPassed, WorkflowMachineState::ApplyTransition),
            (WorkflowMachineEvent::NoMorePhases, WorkflowMachineState::Completed),
        ];

        for (event, expected) in steps {
            let outcome = wf
                .apply(state, *event, |_| true)
                .unwrap_or_else(|e| panic!("step {:?} from {:?} failed: {}", event, state, e));
            assert_eq!(
                outcome.to, *expected,
                "from {:?} on {:?}: expected {:?}, got {:?}",
                state, event, expected, outcome.to
            );
            state = outcome.to;
        }

        assert_eq!(state, WorkflowMachineState::Completed);
    }

    #[test]
    fn full_lifecycle_with_rework() {
        let wf = builtin_workflow();
        let mut state = WorkflowMachineState::Idle;

        let steps: &[(WorkflowMachineEvent, WorkflowMachineState)] = &[
            (WorkflowMachineEvent::Start, WorkflowMachineState::EvaluateTransition),
            (WorkflowMachineEvent::PhaseStarted, WorkflowMachineState::RunPhase),
            (WorkflowMachineEvent::PhaseSucceeded, WorkflowMachineState::EvaluateGates),
            (WorkflowMachineEvent::GatesFailed, WorkflowMachineState::ApplyTransition),
            (WorkflowMachineEvent::RetryPhaseStarted, WorkflowMachineState::RunPhase),
            (WorkflowMachineEvent::PhaseSucceeded, WorkflowMachineState::EvaluateGates),
            (WorkflowMachineEvent::GatesPassed, WorkflowMachineState::ApplyTransition),
            (WorkflowMachineEvent::NoMorePhases, WorkflowMachineState::Completed),
        ];

        for (event, expected) in steps {
            let outcome = wf
                .apply(state, *event, |_| true)
                .unwrap_or_else(|e| panic!("step {:?} from {:?} failed: {}", event, state, e));
            assert_eq!(
                outcome.to, *expected,
                "from {:?} on {:?}: expected {:?}, got {:?}",
                state, event, expected, outcome.to
            );
            state = outcome.to;
        }

        assert_eq!(state, WorkflowMachineState::Completed);
    }

    #[test]
    fn state_unchanged_on_no_transition_error() {
        let wf = builtin_workflow();
        let state_before = WorkflowMachineState::Idle;
        let result = wf.apply(state_before, WorkflowMachineEvent::PhaseSucceeded, |_| true);
        assert!(result.is_err());
        assert_eq!(state_before, WorkflowMachineState::Idle);
    }

    #[test]
    fn state_unchanged_on_guard_blocked_error() {
        let mut doc = builtin_state_machines_document();
        doc.workflow
            .transitions
            .retain(|t| !(t.from == WorkflowMachineState::Idle && t.event == WorkflowMachineEvent::Start));
        doc.workflow.transitions.insert(
            0,
            WorkflowTransitionDefinition {
                from: WorkflowMachineState::Idle,
                event: WorkflowMachineEvent::Start,
                to: WorkflowMachineState::EvaluateTransition,
                guard: Some("rework_budget_available".to_string()),
                action: None,
            },
        );

        let compiled = compile_state_machines_document(doc, MachineSource::Builtin).expect("compile should succeed");

        let state_before = WorkflowMachineState::Idle;
        let result = compiled.workflow.apply(state_before, WorkflowMachineEvent::Start, |_| false);
        assert!(result.is_err());
        assert_eq!(state_before, WorkflowMachineState::Idle);
    }
}
