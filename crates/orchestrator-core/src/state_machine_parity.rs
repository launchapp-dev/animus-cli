use crate::types::{WorkflowMachineEvent, WorkflowMachineState};
use crate::workflow::WorkflowStateMachine;

#[test]
fn workflow_transition_matrix_matches_legacy_behavior() {
    let states = [
        WorkflowMachineState::Idle,
        WorkflowMachineState::EvaluateTransition,
        WorkflowMachineState::RunPhase,
        WorkflowMachineState::EvaluateGates,
        WorkflowMachineState::ApplyTransition,
        WorkflowMachineState::Paused,
        WorkflowMachineState::Completed,
        WorkflowMachineState::Failed,
        WorkflowMachineState::Cancelled,
    ];
    let events = [
        WorkflowMachineEvent::Start,
        WorkflowMachineEvent::PhaseStarted,
        WorkflowMachineEvent::PhaseSucceeded,
        WorkflowMachineEvent::PhaseFailed,
        WorkflowMachineEvent::GatesPassed,
        WorkflowMachineEvent::GatesFailed,
        WorkflowMachineEvent::PolicyDecisionReady,
        WorkflowMachineEvent::PolicyDecisionFailed,
        WorkflowMachineEvent::PauseRequested,
        WorkflowMachineEvent::ResumeRequested,
        WorkflowMachineEvent::CancelRequested,
        WorkflowMachineEvent::ReworkBudgetExceeded,
        WorkflowMachineEvent::NoMorePhases,
    ];

    for state in states {
        for event in events {
            let mut machine = WorkflowStateMachine::new(state);
            let actual = machine.apply(event);
            let expected = legacy_transition(state, event);
            assert_eq!(
                actual, expected,
                "transition mismatch: state={state:?}, event={event:?}"
            );
        }
    }
}

#[test]
fn workflow_failed_state_can_resume_for_retry() {
    let mut machine = WorkflowStateMachine::new(WorkflowMachineState::Failed);
    let state = machine.apply(WorkflowMachineEvent::ResumeRequested);
    assert_eq!(state, WorkflowMachineState::EvaluateTransition);
}

fn legacy_transition(
    state: WorkflowMachineState,
    event: WorkflowMachineEvent,
) -> WorkflowMachineState {
    match state {
        WorkflowMachineState::Idle => match event {
            WorkflowMachineEvent::Start => WorkflowMachineState::EvaluateTransition,
            WorkflowMachineEvent::PauseRequested => WorkflowMachineState::Paused,
            WorkflowMachineEvent::CancelRequested => WorkflowMachineState::Cancelled,
            _ => WorkflowMachineState::Idle,
        },
        WorkflowMachineState::EvaluateTransition => match event {
            WorkflowMachineEvent::PhaseStarted => WorkflowMachineState::RunPhase,
            WorkflowMachineEvent::NoMorePhases => WorkflowMachineState::Completed,
            WorkflowMachineEvent::PauseRequested => WorkflowMachineState::Paused,
            WorkflowMachineEvent::CancelRequested => WorkflowMachineState::Cancelled,
            WorkflowMachineEvent::ReworkBudgetExceeded => WorkflowMachineState::Failed,
            _ => WorkflowMachineState::EvaluateTransition,
        },
        WorkflowMachineState::RunPhase => match event {
            WorkflowMachineEvent::PhaseSucceeded | WorkflowMachineEvent::PhaseFailed => {
                WorkflowMachineState::EvaluateGates
            }
            WorkflowMachineEvent::PauseRequested => WorkflowMachineState::Paused,
            WorkflowMachineEvent::CancelRequested => WorkflowMachineState::Cancelled,
            _ => WorkflowMachineState::RunPhase,
        },
        WorkflowMachineState::EvaluateGates => match event {
            WorkflowMachineEvent::GatesPassed
            | WorkflowMachineEvent::GatesFailed
            | WorkflowMachineEvent::PolicyDecisionReady
            | WorkflowMachineEvent::PolicyDecisionFailed => WorkflowMachineState::ApplyTransition,
            WorkflowMachineEvent::PauseRequested => WorkflowMachineState::Paused,
            WorkflowMachineEvent::CancelRequested => WorkflowMachineState::Cancelled,
            WorkflowMachineEvent::ReworkBudgetExceeded => WorkflowMachineState::Failed,
            _ => WorkflowMachineState::EvaluateGates,
        },
        WorkflowMachineState::ApplyTransition => match event {
            WorkflowMachineEvent::Start => WorkflowMachineState::EvaluateTransition,
            WorkflowMachineEvent::NoMorePhases => WorkflowMachineState::Completed,
            WorkflowMachineEvent::PhaseStarted => WorkflowMachineState::RunPhase,
            WorkflowMachineEvent::PauseRequested => WorkflowMachineState::Paused,
            WorkflowMachineEvent::CancelRequested => WorkflowMachineState::Cancelled,
            WorkflowMachineEvent::ReworkBudgetExceeded => WorkflowMachineState::Failed,
            _ => WorkflowMachineState::ApplyTransition,
        },
        WorkflowMachineState::Paused => match event {
            WorkflowMachineEvent::ResumeRequested => WorkflowMachineState::EvaluateTransition,
            WorkflowMachineEvent::CancelRequested => WorkflowMachineState::Cancelled,
            _ => WorkflowMachineState::Paused,
        },
        WorkflowMachineState::Completed => WorkflowMachineState::Completed,
        WorkflowMachineState::Failed => match event {
            WorkflowMachineEvent::ResumeRequested => WorkflowMachineState::EvaluateTransition,
            WorkflowMachineEvent::CancelRequested => WorkflowMachineState::Cancelled,
            _ => WorkflowMachineState::Failed,
        },
        WorkflowMachineState::Cancelled => WorkflowMachineState::Cancelled,
    }
}
