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
fn state_machine_enters_merge_conflict_from_completed() {
    let mut machine = WorkflowStateMachine::new(WorkflowMachineState::Completed);
    machine.apply(WorkflowMachineEvent::MergeConflictDetected);
    assert_eq!(machine.state(), WorkflowMachineState::MergeConflict);
}

#[test]
fn state_machine_resolves_merge_conflict_to_completed() {
    let mut machine = WorkflowStateMachine::new(WorkflowMachineState::MergeConflict);
    machine.apply(WorkflowMachineEvent::MergeConflictResolved);
    assert_eq!(machine.state(), WorkflowMachineState::Completed);
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
fn state_manager_prunes_to_keep_last_per_phase() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let mut workflow = make_workflow(WorkflowStatus::Running);
    workflow.phases.push(WorkflowPhaseExecution {
        phase_id: "implementation".to_string(),
        status: WorkflowPhaseStatus::Pending,
        started_at: None,
        completed_at: None,
        attempt: 0,
        error_message: None,
    });
    workflow.current_phase = Some("requirements".to_string());
    workflow.current_phase_index = 0;
    manager.save(&workflow).expect("save workflow");

    for _ in 0..12 {
        workflow = manager
            .save_checkpoint(&workflow, CheckpointReason::StatusChange)
            .expect("save requirements checkpoint");
    }

    workflow.current_phase = Some("implementation".to_string());
    workflow.current_phase_index = 1;
    for _ in 0..3 {
        workflow = manager
            .save_checkpoint(&workflow, CheckpointReason::StatusChange)
            .expect("save implementation checkpoint");
    }

    let result = manager
        .prune_checkpoints(&workflow.id, 10, None, false)
        .expect("prune checkpoints");
    assert_eq!(result.pruned_count, 2);
    assert_eq!(result.pruned_checkpoint_numbers, vec![1, 2]);
    assert_eq!(
        result.pruned_by_phase.get("requirements"),
        Some(&2),
        "prune should remove oldest requirements checkpoints"
    );

    let checkpoints = manager
        .list_checkpoints(&workflow.id)
        .expect("list checkpoints");
    assert_eq!(checkpoints, (3usize..=15usize).collect::<Vec<_>>());
}

#[test]
fn state_manager_prunes_checkpoints_older_than_age() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let mut workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");
    for _ in 0..3 {
        workflow = manager
            .save_checkpoint(&workflow, CheckpointReason::StatusChange)
            .expect("save checkpoint");
    }

    workflow.checkpoint_metadata.checkpoints[0].timestamp =
        Utc::now() - chrono::Duration::hours(72);
    workflow.checkpoint_metadata.checkpoints[1].timestamp = Utc::now() - chrono::Duration::hours(2);
    workflow.checkpoint_metadata.checkpoints[2].timestamp = Utc::now() - chrono::Duration::hours(1);
    manager
        .save(&workflow)
        .expect("save workflow with adjusted ages");

    let result = manager
        .prune_checkpoints(&workflow.id, 10, Some(24), false)
        .expect("prune checkpoints by age");
    assert_eq!(result.pruned_count, 1);
    assert_eq!(result.pruned_checkpoint_numbers, vec![1]);

    let checkpoints = manager
        .list_checkpoints(&workflow.id)
        .expect("list checkpoints");
    assert_eq!(checkpoints, vec![2, 3]);
}

#[test]
fn state_manager_prunes_legacy_checkpoints_by_inferred_phase() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let mut workflow = make_workflow(WorkflowStatus::Running);
    workflow.phases.push(WorkflowPhaseExecution {
        phase_id: "implementation".to_string(),
        status: WorkflowPhaseStatus::Pending,
        started_at: None,
        completed_at: None,
        attempt: 0,
        error_message: None,
    });
    workflow.current_phase = Some("requirements".to_string());
    workflow.current_phase_index = 0;
    manager.save(&workflow).expect("save workflow");

    for _ in 0..2 {
        workflow = manager
            .save_checkpoint(&workflow, CheckpointReason::StatusChange)
            .expect("save requirements checkpoint");
    }

    workflow.current_phase = Some("implementation".to_string());
    workflow.current_phase_index = 1;
    for _ in 0..2 {
        workflow = manager
            .save_checkpoint(&workflow, CheckpointReason::StatusChange)
            .expect("save implementation checkpoint");
    }

    for checkpoint in &mut workflow.checkpoint_metadata.checkpoints {
        checkpoint.phase_id = None;
    }
    manager
        .save(&workflow)
        .expect("save legacy checkpoint metadata");

    let result = manager
        .prune_checkpoints(&workflow.id, 1, None, false)
        .expect("prune checkpoints");
    assert_eq!(result.pruned_count, 2);
    assert_eq!(result.pruned_checkpoint_numbers, vec![1, 3]);
    assert_eq!(result.pruned_by_phase.get("requirements"), Some(&1));
    assert_eq!(result.pruned_by_phase.get("implementation"), Some(&1));

    let checkpoints = manager
        .list_checkpoints(&workflow.id)
        .expect("list checkpoints");
    assert_eq!(checkpoints, vec![2, 4]);
}

#[test]
fn state_manager_prune_dry_run_keeps_checkpoint_files_and_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let mut workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");

    for _ in 0..3 {
        workflow = manager
            .save_checkpoint(&workflow, CheckpointReason::StatusChange)
            .expect("save checkpoint");
    }

    let result = manager
        .prune_checkpoints(&workflow.id, 1, None, true)
        .expect("dry-run prune checkpoints");
    assert_eq!(result.pruned_count, 2);
    assert_eq!(result.pruned_checkpoint_numbers, vec![1, 2]);

    let checkpoints = manager
        .list_checkpoints(&workflow.id)
        .expect("list checkpoints");
    assert_eq!(
        checkpoints,
        vec![1, 2, 3],
        "dry-run should not delete files"
    );

    let loaded = manager.load(&workflow.id).expect("load workflow");
    assert_eq!(
        loaded.checkpoint_metadata.checkpoints.len(),
        3,
        "dry-run should not mutate checkpoint metadata"
    );
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

#[test]
fn lifecycle_marks_completed_workflow_as_merge_conflict() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-merge-conflict".to_string(),
        WorkflowRunInput {
            task_id: "TASK-merge".to_string(),
            pipeline_id: Some("standard".to_string()),
        },
    );
    executor.mark_current_phase_success(&mut workflow);
    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Completed);
    assert!(workflow.completed_at.is_some());

    executor.mark_merge_conflict(
        &mut workflow,
        "failed to merge source branch into target branch".to_string(),
    );
    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::MergeConflict);
    assert_eq!(
        workflow.failure_reason.as_deref(),
        Some("failed to merge source branch into target branch")
    );
    assert!(workflow.completed_at.is_none());
}

#[test]
fn lifecycle_resolves_merge_conflict_and_clears_failure_reason() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-merge-conflict-resolve".to_string(),
        WorkflowRunInput {
            task_id: "TASK-merge-resolve".to_string(),
            pipeline_id: Some("standard".to_string()),
        },
    );
    executor.mark_current_phase_success(&mut workflow);
    executor.mark_merge_conflict(
        &mut workflow,
        "failed to merge source branch into target branch".to_string(),
    );
    assert_eq!(workflow.machine_state, WorkflowMachineState::MergeConflict);
    assert!(workflow.failure_reason.is_some());
    assert!(workflow.completed_at.is_none());

    executor.resolve_merge_conflict(&mut workflow);
    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Completed);
    assert!(workflow.failure_reason.is_none());
    assert!(workflow.completed_at.is_some());
}

#[test]
fn phase_with_max_attempts_1_escalates_immediately_on_rework() {
    use crate::agent_runtime_config::PhaseRetryConfig;
    use crate::types::{PhaseDecision, PhaseDecisionVerdict, WorkflowDecisionRisk};
    use std::collections::HashMap;

    let mut retry_configs = HashMap::new();
    retry_configs.insert(
        "implementation".to_string(),
        PhaseRetryConfig {
            max_attempts: 1,
            backoff: None,
        },
    );

    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()])
        .with_retry_configs(retry_configs);

    let mut workflow = executor.bootstrap(
        "WF-retry-1".to_string(),
        WorkflowRunInput {
            task_id: "TASK-retry-1".to_string(),
            pipeline_id: Some("standard".to_string()),
        },
    );

    workflow
        .rework_counts
        .insert("implementation".to_string(), 1);
    executor.mark_current_phase_success_with_decision(
        &mut workflow,
        Some(PhaseDecision {
            kind: "phase_decision".to_string(),
            phase_id: "implementation".to_string(),
            verdict: PhaseDecisionVerdict::Rework,
            reason: "needs changes".to_string(),
            confidence: 0.5,
            risk: WorkflowDecisionRisk::Medium,
            evidence: Vec::new(),
            guardrail_violations: Vec::new(),
            commit_message: None,
        }),
    );

    assert_eq!(
        workflow.status,
        WorkflowStatus::Failed,
        "max_attempts=1 should escalate to failure on first rework exceeding budget"
    );
    assert!(workflow
        .failure_reason
        .as_ref()
        .unwrap()
        .contains("rework budget exceeded"));
}

#[test]
fn phase_with_max_attempts_5_allows_more_retries() {
    use crate::agent_runtime_config::PhaseRetryConfig;
    use crate::types::{PhaseDecision, PhaseDecisionVerdict, WorkflowDecisionRisk};
    use std::collections::HashMap;

    let mut retry_configs = HashMap::new();
    retry_configs.insert(
        "implementation".to_string(),
        PhaseRetryConfig {
            max_attempts: 5,
            backoff: None,
        },
    );

    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()])
        .with_retry_configs(retry_configs);

    let mut workflow = executor.bootstrap(
        "WF-retry-5".to_string(),
        WorkflowRunInput {
            task_id: "TASK-retry-5".to_string(),
            pipeline_id: Some("standard".to_string()),
        },
    );

    for i in 0..4 {
        workflow
            .rework_counts
            .insert("implementation".to_string(), i);
        executor.mark_current_phase_success_with_decision(
            &mut workflow,
            Some(PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "implementation".to_string(),
                verdict: PhaseDecisionVerdict::Rework,
                reason: format!("rework attempt {}", i + 1),
                confidence: 0.5,
                risk: WorkflowDecisionRisk::Medium,
                evidence: Vec::new(),
                guardrail_violations: Vec::new(),
                commit_message: None,
            }),
        );

        assert_eq!(
            workflow.status,
            WorkflowStatus::Running,
            "should still be running after rework {} with max_attempts=5",
            i + 1
        );
    }

    workflow
        .rework_counts
        .insert("implementation".to_string(), 5);
    executor.mark_current_phase_success_with_decision(
        &mut workflow,
        Some(PhaseDecision {
            kind: "phase_decision".to_string(),
            phase_id: "implementation".to_string(),
            verdict: PhaseDecisionVerdict::Rework,
            reason: "final rework".to_string(),
            confidence: 0.5,
            risk: WorkflowDecisionRisk::Medium,
            evidence: Vec::new(),
            guardrail_violations: Vec::new(),
            commit_message: None,
        }),
    );
    assert_eq!(
        workflow.status,
        WorkflowStatus::Failed,
        "should fail after exceeding max_attempts=5"
    );
}

#[test]
fn default_max_attempts_is_3_when_no_config() {
    use crate::types::{PhaseDecision, PhaseDecisionVerdict, WorkflowDecisionRisk};

    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);

    let mut workflow = executor.bootstrap(
        "WF-default-retry".to_string(),
        WorkflowRunInput {
            task_id: "TASK-default-retry".to_string(),
            pipeline_id: Some("standard".to_string()),
        },
    );

    for i in 0..3 {
        workflow
            .rework_counts
            .insert("implementation".to_string(), i);
        executor.mark_current_phase_success_with_decision(
            &mut workflow,
            Some(PhaseDecision {
                kind: "phase_decision".to_string(),
                phase_id: "implementation".to_string(),
                verdict: PhaseDecisionVerdict::Rework,
                reason: format!("rework {}", i + 1),
                confidence: 0.5,
                risk: WorkflowDecisionRisk::Medium,
                evidence: Vec::new(),
                guardrail_violations: Vec::new(),
                commit_message: None,
            }),
        );
        if i < 2 {
            assert_eq!(
                workflow.status,
                WorkflowStatus::Running,
                "should still be running after rework {} with default max_attempts=3",
                i + 1
            );
        }
    }

    workflow
        .rework_counts
        .insert("implementation".to_string(), 3);
    executor.mark_current_phase_success_with_decision(
        &mut workflow,
        Some(PhaseDecision {
            kind: "phase_decision".to_string(),
            phase_id: "implementation".to_string(),
            verdict: PhaseDecisionVerdict::Rework,
            reason: "exceeds budget".to_string(),
            confidence: 0.5,
            risk: WorkflowDecisionRisk::Medium,
            evidence: Vec::new(),
            guardrail_violations: Vec::new(),
            commit_message: None,
        }),
    );
    assert_eq!(
        workflow.status,
        WorkflowStatus::Failed,
        "should fail when rework_count reaches default max_attempts=3"
    );
}

#[test]
fn backoff_calculation() {
    use crate::agent_runtime_config::BackoffConfig;

    let backoff = BackoffConfig {
        initial_secs: 10,
        factor: 2.0,
        max_secs: Some(120),
    };

    assert_eq!(backoff.delay_for_attempt(0), 0);
    assert_eq!(backoff.delay_for_attempt(1), 10);
    assert_eq!(backoff.delay_for_attempt(2), 20);
    assert_eq!(backoff.delay_for_attempt(3), 40);
    assert_eq!(backoff.delay_for_attempt(4), 80);
    assert_eq!(backoff.delay_for_attempt(5), 120);
    assert_eq!(backoff.delay_for_attempt(6), 120);

    let no_cap = BackoffConfig {
        initial_secs: 5,
        factor: 3.0,
        max_secs: None,
    };
    assert_eq!(no_cap.delay_for_attempt(1), 5);
    assert_eq!(no_cap.delay_for_attempt(2), 15);
    assert_eq!(no_cap.delay_for_attempt(3), 45);

    let linear = BackoffConfig {
        initial_secs: 30,
        factor: 1.0,
        max_secs: None,
    };
    assert_eq!(linear.delay_for_attempt(1), 30);
    assert_eq!(linear.delay_for_attempt(2), 30);
    assert_eq!(linear.delay_for_attempt(10), 30);
}

#[test]
fn executor_backoff_delay_for_phase_returns_correct_values() {
    use crate::agent_runtime_config::{BackoffConfig, PhaseRetryConfig};
    use std::collections::HashMap;

    let mut retry_configs = HashMap::new();
    retry_configs.insert(
        "implementation".to_string(),
        PhaseRetryConfig {
            max_attempts: 3,
            backoff: Some(BackoffConfig {
                initial_secs: 10,
                factor: 2.0,
                max_secs: Some(60),
            }),
        },
    );

    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()])
        .with_retry_configs(retry_configs);

    assert_eq!(executor.backoff_delay_for_phase("implementation", 1), 10);
    assert_eq!(executor.backoff_delay_for_phase("implementation", 2), 20);
    assert_eq!(executor.backoff_delay_for_phase("implementation", 3), 40);
    assert_eq!(executor.backoff_delay_for_phase("implementation", 4), 60);

    assert_eq!(
        executor.backoff_delay_for_phase("requirements", 1),
        0,
        "phase without retry config should have zero delay"
    );
}
