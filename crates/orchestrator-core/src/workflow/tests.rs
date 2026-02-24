use super::*;
use crate::types::{
    CheckpointReason, OrchestratorWorkflow, WorkflowCheckpointMetadata, WorkflowDecisionRecord,
    WorkflowMachineEvent, WorkflowMachineState, WorkflowPhaseExecution, WorkflowPhaseStatus,
    WorkflowRunInput, WorkflowStatus,
};
use chrono::Utc;

fn make_workflow(status: WorkflowStatus) -> OrchestratorWorkflow {
    OrchestratorWorkflow {
        id: "WF-test".to_string(),
        task_id: "TASK-1".to_string(),
        pipeline_id: Some("standard".to_string()),
        status,
        current_phase_index: 0,
        phases: vec![WorkflowPhaseExecution {
            phase_id: "requirements".to_string(),
            status: WorkflowPhaseStatus::Running,
            started_at: Some(Utc::now()),
            completed_at: None,
            attempt: 1,
            error_message: None,
        }],
        machine_state: WorkflowMachineState::Idle,
        current_phase: Some("requirements".to_string()),
        started_at: Utc::now(),
        completed_at: None,
        failure_reason: None,
        checkpoint_metadata: WorkflowCheckpointMetadata::default(),
        rework_counts: std::collections::HashMap::new(),
        total_reworks: 0,
        decision_history: Vec::<WorkflowDecisionRecord>::new(),
    }
}

#[test]
fn state_machine_transitions() {
    let mut machine = WorkflowStateMachine::default();
    machine.apply(WorkflowMachineEvent::Start);
    assert_eq!(machine.state(), WorkflowMachineState::EvaluateTransition);

    machine.apply(WorkflowMachineEvent::PhaseStarted);
    assert_eq!(machine.state(), WorkflowMachineState::RunPhase);

    machine.apply(WorkflowMachineEvent::PhaseSucceeded);
    assert_eq!(machine.state(), WorkflowMachineState::EvaluateGates);

    machine.apply(WorkflowMachineEvent::GatesPassed);
    assert_eq!(machine.state(), WorkflowMachineState::ApplyTransition);
}

#[test]
fn state_machine_allows_resume_from_failed() {
    let mut machine = WorkflowStateMachine::new(WorkflowMachineState::Failed);

    machine.apply(WorkflowMachineEvent::ResumeRequested);
    assert_eq!(machine.state(), WorkflowMachineState::EvaluateTransition);

    machine.apply(WorkflowMachineEvent::PhaseStarted);
    assert_eq!(machine.state(), WorkflowMachineState::RunPhase);
}

#[test]
fn lifecycle_does_not_pause_completed_workflow() {
    let mut workflow = make_workflow(WorkflowStatus::Completed);
    workflow.machine_state = WorkflowMachineState::Completed;
    let executor = WorkflowLifecycleExecutor::default();

    executor.pause(&mut workflow);

    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Completed);
}

#[test]
fn state_manager_saves_checkpoints() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");

    let updated = manager
        .save_checkpoint(&workflow, CheckpointReason::Start)
        .expect("save checkpoint");

    assert_eq!(updated.checkpoint_metadata.checkpoint_count, 1);
    let checkpoints = manager
        .list_checkpoints(&workflow.id)
        .expect("list checkpoints");
    assert_eq!(checkpoints, vec![1]);
}

#[test]
fn resume_manager_detects_resumable_running_workflow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());
    let workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");

    let resume_manager = WorkflowResumeManager::new(temp.path()).expect("resume manager");
    let resumable = resume_manager
        .get_resumable_workflows()
        .expect("get resumable workflows");
    assert_eq!(resumable.len(), 1);
}

#[test]
fn resume_clears_failure_and_can_complete_after_retry() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-retry".to_string(),
        WorkflowRunInput {
            task_id: "TASK-1".to_string(),
            pipeline_id: Some("standard".to_string()),
        },
    );

    executor.mark_current_phase_failed(&mut workflow, "first attempt failed".to_string());
    assert_eq!(workflow.status, WorkflowStatus::Failed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Failed);
    assert!(workflow.failure_reason.is_some());

    executor.resume(&mut workflow);
    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.machine_state, WorkflowMachineState::RunPhase);
    assert!(workflow.failure_reason.is_none());
    assert!(workflow.completed_at.is_none());
    assert_eq!(
        workflow.phases[workflow.current_phase_index].status,
        WorkflowPhaseStatus::Running
    );
    assert_eq!(workflow.phases[workflow.current_phase_index].attempt, 2);

    executor.mark_current_phase_success(&mut workflow);
    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Completed);
    assert!(workflow.failure_reason.is_none());
}

#[test]
fn request_research_inserts_phase_before_current() {
    let executor = WorkflowLifecycleExecutor::new(vec![
        "requirements".to_string(),
        "implementation".to_string(),
    ]);
    let mut workflow = executor.bootstrap(
        "WF-research".to_string(),
        WorkflowRunInput {
            task_id: "TASK-42".to_string(),
            pipeline_id: Some("standard".to_string()),
        },
    );
    assert_eq!(workflow.current_phase.as_deref(), Some("requirements"));

    let inserted =
        executor.request_research_phase(&mut workflow, "missing product context".to_string());
    assert!(inserted);
    assert_eq!(workflow.current_phase.as_deref(), Some("research"));
    assert_eq!(workflow.current_phase_index, 0);
    assert_eq!(workflow.phases[0].phase_id, "research");
    assert_eq!(workflow.phases[0].status, WorkflowPhaseStatus::Running);
    assert_eq!(workflow.phases[1].phase_id, "requirements");
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Ready);
    assert!(workflow
        .decision_history
        .iter()
        .any(|record| record.target_phase.as_deref() == Some("research")));
}
