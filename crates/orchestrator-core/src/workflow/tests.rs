use super::*;
use crate::subject_adapter::SubjectContext;
use crate::types::{
    Assignee, CheckpointReason, Complexity, OrchestratorTask, OrchestratorWorkflow, PhaseDecision,
    PhaseDecisionVerdict, Priority, ResourceRequirements, RiskLevel, Scope, SubjectRef, TaskMetadata, TaskStatus,
    TaskType, WorkflowCheckpointMetadata, WorkflowDecisionAction, WorkflowDecisionRecord, WorkflowDecisionRisk,
    WorkflowMachineEvent, WorkflowMachineState, WorkflowPhaseExecution, WorkflowPhaseStatus, WorkflowRunInput,
    WorkflowStatus, SUBJECT_KIND_TASK,
};
use chrono::Utc;
use std::collections::HashMap;

fn make_workflow(status: WorkflowStatus) -> OrchestratorWorkflow {
    OrchestratorWorkflow {
        id: "WF-test".to_string(),
        task_id: "TASK-1".to_string(),
        workflow_ref: Some("standard-workflow".to_string()),
        subject: SubjectRef::task("TASK-1".to_string()),
        input: None,
        vars: std::collections::HashMap::new(),
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

fn task_subject_context(task: &OrchestratorTask) -> SubjectContext {
    let mut attributes = HashMap::new();
    attributes.insert("task_type".to_string(), task.task_type.as_str().to_string());
    attributes.insert("priority".to_string(), task.priority.as_str().to_string());

    SubjectContext {
        subject_kind: SUBJECT_KIND_TASK.to_string(),
        subject_id: task.id.clone(),
        subject_title: task.title.clone(),
        subject_description: task.description.clone(),
        attributes,
        task: Some(task.clone()),
    }
}

fn skip_decision(reason: &str) -> PhaseDecision {
    PhaseDecision {
        kind: "phase_decision".to_string(),
        phase_id: "requirements".to_string(),
        verdict: PhaseDecisionVerdict::Skip,
        confidence: 0.92,
        risk: WorkflowDecisionRisk::Low,
        reason: reason.to_string(),
        evidence: Vec::new(),
        guardrail_violations: Vec::new(),
        commit_message: None,
        target_phase: None,
    }
}

#[test]
fn state_machine_transitions() {
    let mut machine = WorkflowStateMachine::default();
    machine.apply(WorkflowMachineEvent::Start).unwrap();
    assert_eq!(machine.state(), WorkflowMachineState::EvaluateTransition);

    machine.apply(WorkflowMachineEvent::PhaseStarted).unwrap();
    assert_eq!(machine.state(), WorkflowMachineState::RunPhase);

    machine.apply(WorkflowMachineEvent::PhaseSucceeded).unwrap();
    assert_eq!(machine.state(), WorkflowMachineState::EvaluateGates);

    machine.apply(WorkflowMachineEvent::GatesPassed).unwrap();
    assert_eq!(machine.state(), WorkflowMachineState::ApplyTransition);
}

#[test]
fn state_machine_allows_resume_from_failed() {
    let mut machine = WorkflowStateMachine::new(WorkflowMachineState::Failed);

    machine.apply(WorkflowMachineEvent::ResumeRequested).unwrap();
    assert_eq!(machine.state(), WorkflowMachineState::EvaluateTransition);

    machine.apply(WorkflowMachineEvent::PhaseStarted).unwrap();
    assert_eq!(machine.state(), WorkflowMachineState::RunPhase);
}

#[test]
fn state_machine_enters_merge_conflict_from_completed() {
    let mut machine = WorkflowStateMachine::new(WorkflowMachineState::Completed);
    machine.apply(WorkflowMachineEvent::MergeConflictDetected).unwrap();
    assert_eq!(machine.state(), WorkflowMachineState::MergeConflict);
}

#[test]
fn state_machine_resolves_merge_conflict_to_completed() {
    let mut machine = WorkflowStateMachine::new(WorkflowMachineState::MergeConflict);
    machine.apply(WorkflowMachineEvent::MergeConflictResolved).unwrap();
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
fn lifecycle_skip_already_done_completes_workflow_early() {
    let mut workflow = make_workflow(WorkflowStatus::Running);
    workflow.machine_state = WorkflowMachineState::RunPhase;
    workflow.phases.push(WorkflowPhaseExecution {
        phase_id: "implementation".to_string(),
        status: WorkflowPhaseStatus::Pending,
        started_at: None,
        completed_at: None,
        attempt: 0,
        error_message: None,
    });
    let executor = WorkflowLifecycleExecutor::default();

    executor
        .mark_current_phase_success_with_decision(
            &mut workflow,
            Some(skip_decision("already_done: task already completed upstream")),
        )
        .expect("phase success");

    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Completed);
    assert!(workflow.completed_at.is_some());
    assert_eq!(workflow.current_phase, None);
    assert_eq!(workflow.phases[0].status, WorkflowPhaseStatus::Success);
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Pending);
    assert_eq!(workflow.decision_history.last().map(|record| record.decision), Some(WorkflowDecisionAction::Skip));
}

#[test]
fn lifecycle_skip_duplicate_cancels_workflow_early() {
    let mut workflow = make_workflow(WorkflowStatus::Running);
    workflow.machine_state = WorkflowMachineState::RunPhase;
    workflow.phases.push(WorkflowPhaseExecution {
        phase_id: "implementation".to_string(),
        status: WorkflowPhaseStatus::Pending,
        started_at: None,
        completed_at: None,
        attempt: 0,
        error_message: None,
    });
    let executor = WorkflowLifecycleExecutor::default();

    executor
        .mark_current_phase_success_with_decision(
            &mut workflow,
            Some(skip_decision("duplicate: superseded by TASK-999")),
        )
        .expect("phase success");

    assert_eq!(workflow.status, WorkflowStatus::Cancelled);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Cancelled);
    assert!(workflow.completed_at.is_some());
    assert_eq!(workflow.current_phase, None);
    assert_eq!(workflow.phases[0].status, WorkflowPhaseStatus::Success);
    assert_eq!(workflow.decision_history.last().map(|record| record.decision), Some(WorkflowDecisionAction::Skip));
}

#[test]
fn state_manager_saves_checkpoints() {
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");

    let updated = manager.save_checkpoint(&workflow, CheckpointReason::Start).expect("save checkpoint");

    assert_eq!(updated.checkpoint_metadata.checkpoint_count, 1);
    let checkpoints = manager.list_checkpoints(&workflow.id).expect("list checkpoints");
    assert_eq!(checkpoints, vec![1]);
}

#[test]
fn state_manager_prunes_to_keep_last_per_phase() {
    crate::test_env::stable_test_home();
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

    for _ in 0..3 {
        workflow =
            manager.save_checkpoint(&workflow, CheckpointReason::StatusChange).expect("save requirements checkpoint");
    }

    workflow.current_phase = Some("implementation".to_string());
    workflow.current_phase_index = 1;
    workflow =
        manager.save_checkpoint(&workflow, CheckpointReason::StatusChange).expect("save implementation checkpoint");

    let result = manager.prune_checkpoints(&workflow.id, 2, None, false).expect("prune checkpoints");
    assert_eq!(result.pruned_count, 1);
    assert_eq!(result.pruned_checkpoint_numbers, vec![1]);
    assert_eq!(
        result.pruned_by_phase.get("requirements"),
        Some(&1),
        "prune should remove oldest requirements checkpoint"
    );

    let checkpoints = manager.list_checkpoints(&workflow.id).expect("list checkpoints");
    assert_eq!(checkpoints, vec![2, 3, 4]);
}

#[test]
fn state_manager_prunes_checkpoints_older_than_age() {
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let mut workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");
    for _ in 0..3 {
        workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange).expect("save checkpoint");
    }

    workflow.checkpoint_metadata.checkpoints[0].timestamp = Utc::now() - chrono::Duration::hours(72);
    workflow.checkpoint_metadata.checkpoints[1].timestamp = Utc::now() - chrono::Duration::hours(2);
    workflow.checkpoint_metadata.checkpoints[2].timestamp = Utc::now() - chrono::Duration::hours(1);
    manager.save(&workflow).expect("save workflow with adjusted ages");

    let result = manager.prune_checkpoints(&workflow.id, 10, Some(24), false).expect("prune checkpoints by age");
    assert_eq!(result.pruned_count, 1);
    assert_eq!(result.pruned_checkpoint_numbers, vec![1]);

    let checkpoints = manager.list_checkpoints(&workflow.id).expect("list checkpoints");
    assert_eq!(checkpoints, vec![2, 3]);
}

#[test]
fn state_manager_prunes_legacy_checkpoints_by_inferred_phase() {
    crate::test_env::stable_test_home();
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
        workflow =
            manager.save_checkpoint(&workflow, CheckpointReason::StatusChange).expect("save requirements checkpoint");
    }

    workflow.current_phase = Some("implementation".to_string());
    workflow.current_phase_index = 1;
    for _ in 0..2 {
        workflow =
            manager.save_checkpoint(&workflow, CheckpointReason::StatusChange).expect("save implementation checkpoint");
    }

    for checkpoint in &mut workflow.checkpoint_metadata.checkpoints {
        checkpoint.phase_id = None;
    }
    manager.save(&workflow).expect("save legacy checkpoint metadata");

    let result = manager.prune_checkpoints(&workflow.id, 1, None, false).expect("prune checkpoints");
    assert_eq!(result.pruned_count, 2);
    assert_eq!(result.pruned_checkpoint_numbers, vec![1, 3]);
    assert_eq!(result.pruned_by_phase.get("requirements"), Some(&1));
    assert_eq!(result.pruned_by_phase.get("implementation"), Some(&1));

    let checkpoints = manager.list_checkpoints(&workflow.id).expect("list checkpoints");
    assert_eq!(checkpoints, vec![2, 4]);
}

#[test]
fn state_manager_prune_dry_run_keeps_checkpoint_files_and_metadata() {
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let mut workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");

    for _ in 0..3 {
        workflow = manager.save_checkpoint(&workflow, CheckpointReason::StatusChange).expect("save checkpoint");
    }

    let result = manager.prune_checkpoints(&workflow.id, 1, None, true).expect("dry-run prune checkpoints");
    assert_eq!(result.pruned_count, 2);
    assert_eq!(result.pruned_checkpoint_numbers, vec![1, 2]);

    let checkpoints = manager.list_checkpoints(&workflow.id).expect("list checkpoints");
    assert_eq!(checkpoints, vec![1, 2, 3], "dry-run should not delete files");

    let loaded = manager.load(&workflow.id).expect("load workflow");
    assert_eq!(loaded.checkpoint_metadata.checkpoints.len(), 3, "dry-run should not mutate checkpoint metadata");
}

#[test]
fn resume_manager_detects_resumable_running_workflow() {
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());
    let workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");

    let resume_manager = WorkflowResumeManager::new(temp.path()).expect("resume manager");
    let resumable = resume_manager.get_resumable_workflows().expect("get resumable workflows");
    assert_eq!(resumable.len(), 1);
}

#[test]
fn resume_clears_failure_and_can_complete_after_retry() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-retry".to_string(),
        WorkflowRunInput::for_task("TASK-1".to_string(), Some("standard-workflow".to_string())),
    );

    executor.mark_current_phase_failed(&mut workflow, "first attempt failed".to_string()).expect("phase failure");
    assert_eq!(workflow.status, WorkflowStatus::Failed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Failed);
    assert!(workflow.failure_reason.is_some());

    executor.resume(&mut workflow);
    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.machine_state, WorkflowMachineState::RunPhase);
    assert!(workflow.failure_reason.is_none());
    assert!(workflow.completed_at.is_none());
    assert_eq!(workflow.phases[workflow.current_phase_index].status, WorkflowPhaseStatus::Running);
    assert_eq!(workflow.phases[workflow.current_phase_index].attempt, 2);

    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Completed);
    assert!(workflow.failure_reason.is_none());
}

#[test]
fn lifecycle_marks_completed_workflow_as_merge_conflict() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-merge-conflict".to_string(),
        WorkflowRunInput::for_task("TASK-merge".to_string(), Some("standard-workflow".to_string())),
    );
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Completed);
    assert!(workflow.completed_at.is_some());

    executor.mark_merge_conflict(&mut workflow, "failed to merge source branch into target branch".to_string());
    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.machine_state, WorkflowMachineState::MergeConflict);
    assert_eq!(workflow.failure_reason.as_deref(), Some("failed to merge source branch into target branch"));
    assert!(workflow.completed_at.is_none());
}

#[test]
fn lifecycle_resolves_merge_conflict_and_clears_failure_reason() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-merge-conflict-resolve".to_string(),
        WorkflowRunInput::for_task("TASK-merge-resolve".to_string(), Some("standard-workflow".to_string())),
    );
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    executor.mark_merge_conflict(&mut workflow, "failed to merge source branch into target branch".to_string());
    assert_eq!(workflow.machine_state, WorkflowMachineState::MergeConflict);
    assert!(workflow.failure_reason.is_some());
    assert!(workflow.completed_at.is_none());

    executor.resolve_merge_conflict(&mut workflow);
    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Completed);
    assert!(workflow.failure_reason.is_none());
    assert!(workflow.completed_at.is_some());
}

fn make_rework_decision(target_phase: Option<String>) -> PhaseDecision {
    PhaseDecision {
        kind: "phase_decision".to_string(),
        phase_id: "code-review".to_string(),
        verdict: PhaseDecisionVerdict::Rework,
        confidence: 0.7,
        risk: WorkflowDecisionRisk::Medium,
        reason: "needs rework".to_string(),
        evidence: vec![],
        guardrail_violations: vec![],
        commit_message: None,
        target_phase,
    }
}

fn make_task(task_type: TaskType, priority: Priority) -> OrchestratorTask {
    OrchestratorTask {
        id: "TASK-skip".to_string(),
        title: "Test task".to_string(),
        description: "Test description".to_string(),
        task_type,
        status: TaskStatus::InProgress,
        blocked_reason: None,
        blocked_at: None,
        blocked_phase: None,
        blocked_by: None,
        priority,
        risk: RiskLevel::default(),
        scope: Scope::default(),
        complexity: Complexity::default(),
        impact_area: Vec::new(),
        assignee: Assignee::default(),
        estimated_effort: None,
        linked_requirements: Vec::new(),
        linked_architecture_entities: Vec::new(),
        dependencies: Vec::new(),
        checklist: Vec::new(),
        tags: Vec::new(),
        workflow_metadata: crate::types::WorkflowMetadata::default(),
        worktree_path: None,
        branch_name: None,
        metadata: TaskMetadata {
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: "test".to_string(),
            updated_by: "test".to_string(),
            started_at: None,
            completed_at: None,
            status_changed_at: None,
            version: 1,
        },
        deadline: None,
        paused: false,
        cancelled: false,
        resolution: None,
        resource_requirements: ResourceRequirements::default(),
        consecutive_dispatch_failures: None,
        last_dispatch_failure_at: None,
        dispatch_history: Vec::new(),
    }
}

#[test]
fn rework_routes_to_prior_phase_by_id() {
    use crate::workflow_config::PhaseTransitionConfig;
    use std::collections::HashMap;

    let mut verdict_routing = HashMap::new();
    let mut review_verdicts = HashMap::new();
    review_verdicts.insert(
        "rework".to_string(),
        PhaseTransitionConfig {
            target: String::new(),
            guard: None,
            allow_agent_target: true,
            allowed_targets: vec!["implementation".to_string()],
        },
    );
    verdict_routing.insert("code-review".to_string(), review_verdicts);

    let executor = WorkflowLifecycleExecutor::with_verdict_routing(
        vec!["requirements".to_string(), "implementation".to_string(), "code-review".to_string()],
        verdict_routing,
    );
    let mut workflow = executor.bootstrap(
        "WF-rework-target".to_string(),
        WorkflowRunInput::for_task("TASK-rework".to_string(), Some("standard-workflow".to_string())),
    );
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.current_phase.as_deref(), Some("implementation"));
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.current_phase.as_deref(), Some("code-review"));
    assert_eq!(workflow.current_phase_index, 2);

    let decision = make_rework_decision(Some("implementation".to_string()));
    executor.mark_current_phase_success_with_decision(&mut workflow, Some(decision)).expect("phase success");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase_index, 1);
    assert_eq!(workflow.current_phase.as_deref(), Some("implementation"));
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Running);
    assert!(workflow.phases[1].attempt >= 2);

    let last_decision = workflow.decision_history.last().unwrap();
    assert_eq!(last_decision.decision, WorkflowDecisionAction::Rework);
    assert_eq!(last_decision.target_phase.as_deref(), Some("implementation"));
    assert_eq!(*workflow.rework_counts.get("implementation").unwrap(), 1);
}

#[test]
fn rework_without_target_reruns_current_phase() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string(), "code-review".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-rework-current".to_string(),
        WorkflowRunInput::for_task("TASK-rework-current".to_string(), Some("standard-workflow".to_string())),
    );
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.current_phase.as_deref(), Some("code-review"));
    assert_eq!(workflow.current_phase_index, 1);
    let attempt_before = workflow.phases[1].attempt;

    let decision = make_rework_decision(None);
    executor.mark_current_phase_success_with_decision(&mut workflow, Some(decision)).expect("phase success");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase_index, 1);
    assert_eq!(workflow.current_phase.as_deref(), Some("code-review"));
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Running);
    assert_eq!(workflow.phases[1].attempt, attempt_before + 1);

    let last_decision = workflow.decision_history.last().unwrap();
    assert_eq!(last_decision.decision, WorkflowDecisionAction::Rework);
    assert_eq!(last_decision.target_phase.as_deref(), Some("code-review"));
}

#[test]
fn phase_with_max_attempts_1_escalates_immediately_on_rework() {
    use crate::agent_runtime_config::PhaseRetryConfig;
    use crate::types::{PhaseDecision, PhaseDecisionVerdict, WorkflowDecisionRisk};
    use std::collections::HashMap;

    let mut retry_configs = HashMap::new();
    retry_configs.insert("implementation".to_string(), PhaseRetryConfig { max_attempts: 1, backoff: None });

    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]).with_retry_configs(retry_configs);

    let mut workflow = executor.bootstrap(
        "WF-retry-1".to_string(),
        WorkflowRunInput::for_task("TASK-retry-1".to_string(), Some("standard-workflow".to_string())),
    );

    workflow.rework_counts.insert("implementation".to_string(), 1);
    executor
        .mark_current_phase_success_with_decision(
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
                target_phase: None,
            }),
        )
        .expect("phase success");

    assert_eq!(
        workflow.status,
        WorkflowStatus::Escalated,
        "max_attempts=1 should escalate on first rework exceeding budget"
    );
    assert!(workflow.failure_reason.as_ref().unwrap().contains("rework budget exceeded"));
}

#[test]
fn phase_with_max_attempts_5_allows_more_retries() {
    use crate::agent_runtime_config::PhaseRetryConfig;
    use crate::types::{PhaseDecision, PhaseDecisionVerdict, WorkflowDecisionRisk};
    use std::collections::HashMap;

    let mut retry_configs = HashMap::new();
    retry_configs.insert("implementation".to_string(), PhaseRetryConfig { max_attempts: 5, backoff: None });

    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]).with_retry_configs(retry_configs);

    let mut workflow = executor.bootstrap(
        "WF-retry-5".to_string(),
        WorkflowRunInput::for_task("TASK-retry-5".to_string(), Some("standard-workflow".to_string())),
    );

    for i in 0..4 {
        workflow.rework_counts.insert("implementation".to_string(), i);
        executor
            .mark_current_phase_success_with_decision(
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
                    target_phase: None,
                }),
            )
            .expect("phase success");

        assert_eq!(
            workflow.status,
            WorkflowStatus::Running,
            "should still be running after rework {} with max_attempts=5",
            i + 1
        );
    }

    workflow.rework_counts.insert("implementation".to_string(), 5);
    executor
        .mark_current_phase_success_with_decision(
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
                target_phase: None,
            }),
        )
        .expect("phase success");
    assert_eq!(workflow.status, WorkflowStatus::Escalated, "should escalate after exceeding max_attempts=5");
}

#[test]
fn skip_guarded_phase_skips_when_task_type_matches() {
    let mut guards = HashMap::new();
    guards.insert("testing".to_string(), vec!["task_type == 'docs'".to_string()]);
    let executor = WorkflowLifecycleExecutor::new(vec!["requirements".to_string(), "testing".to_string()])
        .with_skip_guards(guards);

    let mut workflow = executor.bootstrap(
        "WF-skip-1".to_string(),
        WorkflowRunInput::for_task("TASK-skip".to_string(), Some("standard-workflow".to_string())),
    );
    let task = make_task(TaskType::Docs, Priority::Medium);

    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.current_phase.as_deref(), Some("testing"));
    assert_eq!(workflow.status, WorkflowStatus::Running);

    let subject_context = task_subject_context(&task);
    executor.skip_guarded_phases(&mut workflow, &subject_context);

    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Skipped);
    assert!(workflow
        .decision_history
        .iter()
        .any(|r| r.decision == crate::types::WorkflowDecisionAction::Skip && r.phase_id == "testing"));
}

#[test]
fn skip_guarded_phase_does_not_skip_when_guard_does_not_match() {
    let mut guards = HashMap::new();
    guards.insert("testing".to_string(), vec!["task_type == 'docs'".to_string()]);
    let executor = WorkflowLifecycleExecutor::new(vec!["requirements".to_string(), "testing".to_string()])
        .with_skip_guards(guards);

    let mut workflow = executor.bootstrap(
        "WF-skip-2".to_string(),
        WorkflowRunInput::for_task("TASK-skip".to_string(), Some("standard-workflow".to_string())),
    );
    let task = make_task(TaskType::Feature, Priority::High);

    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    let subject_context = task_subject_context(&task);
    executor.skip_guarded_phases(&mut workflow, &subject_context);

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase.as_deref(), Some("testing"));
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Running);
}

#[test]
fn skip_guarded_phase_any_matching_guard_causes_skip() {
    let mut guards = HashMap::new();
    guards.insert("testing".to_string(), vec!["task_type == 'refactor'".to_string(), "priority == 'low'".to_string()]);
    let executor = WorkflowLifecycleExecutor::new(vec!["requirements".to_string(), "testing".to_string()])
        .with_skip_guards(guards);

    let mut workflow = executor.bootstrap(
        "WF-skip-3".to_string(),
        WorkflowRunInput::for_task("TASK-skip".to_string(), Some("standard-workflow".to_string())),
    );
    let task = make_task(TaskType::Feature, Priority::Low);

    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    let subject_context = task_subject_context(&task);
    executor.skip_guarded_phases(&mut workflow, &subject_context);

    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Skipped);
}

#[test]
fn skip_guarded_phase_with_empty_skip_if_runs_normally() {
    let executor = WorkflowLifecycleExecutor::new(vec!["requirements".to_string(), "testing".to_string()]);

    let mut workflow = executor.bootstrap(
        "WF-skip-4".to_string(),
        WorkflowRunInput::for_task("TASK-skip".to_string(), Some("standard-workflow".to_string())),
    );
    let task = make_task(TaskType::Docs, Priority::Medium);

    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    let subject_context = task_subject_context(&task);
    executor.skip_guarded_phases(&mut workflow, &subject_context);

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase.as_deref(), Some("testing"));
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Running);
}

#[test]
fn skip_guard_evaluator_supports_not_equals() {
    let task = make_task(TaskType::Feature, Priority::High);
    let subject_context = task_subject_context(&task);
    assert!(lifecycle_executor::evaluate_skip_guard("task_type != 'docs'", &subject_context));
    assert!(!lifecycle_executor::evaluate_skip_guard("task_type != 'feature'", &subject_context));
}

#[test]
fn skip_guarded_phase_skips_first_phase_on_bootstrap() {
    let mut guards = HashMap::new();
    guards.insert("requirements".to_string(), vec!["task_type == 'chore'".to_string()]);
    let executor = WorkflowLifecycleExecutor::new(vec![
        "requirements".to_string(),
        "implementation".to_string(),
        "testing".to_string(),
    ])
    .with_skip_guards(guards);

    let mut workflow = executor.bootstrap(
        "WF-skip-boot".to_string(),
        WorkflowRunInput::for_task("TASK-skip".to_string(), Some("standard-workflow".to_string())),
    );
    let task = make_task(TaskType::Chore, Priority::Medium);

    let subject_context = task_subject_context(&task);
    executor.skip_guarded_phases(&mut workflow, &subject_context);

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase.as_deref(), Some("implementation"));
    assert_eq!(workflow.phases[0].status, WorkflowPhaseStatus::Skipped);
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Running);
}

#[test]
fn skip_guarded_phases_skips_consecutive_phases() {
    let mut guards = HashMap::new();
    guards.insert("testing".to_string(), vec!["task_type == 'docs'".to_string()]);
    guards.insert("code-review".to_string(), vec!["task_type == 'docs'".to_string()]);
    let executor = WorkflowLifecycleExecutor::new(vec![
        "requirements".to_string(),
        "code-review".to_string(),
        "testing".to_string(),
    ])
    .with_skip_guards(guards);

    let mut workflow = executor.bootstrap(
        "WF-skip-consec".to_string(),
        WorkflowRunInput::for_task("TASK-skip".to_string(), Some("standard-workflow".to_string())),
    );
    let task = make_task(TaskType::Docs, Priority::Medium);

    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    let subject_context = task_subject_context(&task);
    executor.skip_guarded_phases(&mut workflow, &subject_context);

    assert_eq!(workflow.status, WorkflowStatus::Completed);
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Skipped);
    assert_eq!(workflow.phases[2].status, WorkflowPhaseStatus::Skipped);
}

#[test]
fn advance_ignores_agent_target_phase_and_uses_default_order() {
    let executor = WorkflowLifecycleExecutor::new(vec![
        "requirements".to_string(),
        "implementation".to_string(),
        "testing".to_string(),
        "code-review".to_string(),
    ]);
    let mut workflow = executor.bootstrap(
        "WF-advance-target".to_string(),
        WorkflowRunInput::for_task("TASK-advance-target".to_string(), Some("standard-workflow".to_string())),
    );
    assert_eq!(workflow.current_phase.as_deref(), Some("requirements"));

    let decision = PhaseDecision {
        kind: "phase_decision".to_string(),
        phase_id: "requirements".to_string(),
        verdict: PhaseDecisionVerdict::Advance,
        confidence: 0.95,
        risk: WorkflowDecisionRisk::Low,
        reason: "skip implementation, go to testing".to_string(),
        evidence: vec![],
        guardrail_violations: vec![],
        commit_message: None,
        target_phase: Some("testing".to_string()),
    };
    executor.mark_current_phase_success_with_decision(&mut workflow, Some(decision)).expect("phase success");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase_index, 1);
    assert_eq!(workflow.current_phase.as_deref(), Some("implementation"));
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Running);
    assert_eq!(workflow.phases[2].status, WorkflowPhaseStatus::Pending);

    let last_decision = workflow.decision_history.last().unwrap();
    assert_eq!(last_decision.decision, WorkflowDecisionAction::Advance);
    assert_eq!(last_decision.target_phase.as_deref(), Some("implementation"));
}

#[test]
fn rework_with_nonexistent_target_falls_back_to_current_phase() {
    use crate::workflow_config::PhaseTransitionConfig;
    use std::collections::HashMap;

    let mut verdict_routing = HashMap::new();
    let mut review_verdicts = HashMap::new();
    review_verdicts.insert(
        "rework".to_string(),
        PhaseTransitionConfig {
            target: String::new(),
            guard: None,
            allow_agent_target: true,
            allowed_targets: vec!["implementation".to_string()],
        },
    );
    verdict_routing.insert("code-review".to_string(), review_verdicts);

    let executor = WorkflowLifecycleExecutor::with_verdict_routing(
        vec!["implementation".to_string(), "code-review".to_string()],
        verdict_routing,
    );
    let mut workflow = executor.bootstrap(
        "WF-rework-bad-target".to_string(),
        WorkflowRunInput::for_task("TASK-rework-bad".to_string(), Some("standard-workflow".to_string())),
    );
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.current_phase.as_deref(), Some("code-review"));
    assert_eq!(workflow.current_phase_index, 1);

    let decision = make_rework_decision(Some("nonexistent-phase".to_string()));
    executor.mark_current_phase_success_with_decision(&mut workflow, Some(decision)).expect("phase success");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase_index, 1);
    assert_eq!(workflow.current_phase.as_deref(), Some("code-review"));
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Running);
}

#[test]
fn advance_can_follow_agent_selected_target_when_yaml_allows_it() {
    use crate::workflow_config::PhaseTransitionConfig;
    use std::collections::HashMap;

    let mut verdict_routing = HashMap::new();
    let mut requirements_verdicts = HashMap::new();
    requirements_verdicts.insert(
        "advance".to_string(),
        PhaseTransitionConfig {
            target: String::new(),
            guard: None,
            allow_agent_target: true,
            allowed_targets: vec!["testing".to_string()],
        },
    );
    verdict_routing.insert("requirements".to_string(), requirements_verdicts);

    let executor = WorkflowLifecycleExecutor::with_verdict_routing(
        vec![
            "requirements".to_string(),
            "implementation".to_string(),
            "testing".to_string(),
            "code-review".to_string(),
        ],
        verdict_routing,
    );
    let mut workflow = executor.bootstrap(
        "WF-advance-agent-target".to_string(),
        WorkflowRunInput::for_task("TASK-advance-agent-target".to_string(), Some("standard-workflow".to_string())),
    );

    let decision = PhaseDecision {
        kind: "phase_decision".to_string(),
        phase_id: "requirements".to_string(),
        verdict: PhaseDecisionVerdict::Advance,
        confidence: 0.95,
        risk: WorkflowDecisionRisk::Low,
        reason: "testing can run next".to_string(),
        evidence: vec![],
        guardrail_violations: vec![],
        commit_message: None,
        target_phase: Some("testing".to_string()),
    };
    executor.mark_current_phase_success_with_decision(&mut workflow, Some(decision)).expect("phase success");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase_index, 2);
    assert_eq!(workflow.current_phase.as_deref(), Some("testing"));

    let last_decision = workflow.decision_history.last().unwrap();
    assert_eq!(last_decision.decision, WorkflowDecisionAction::Advance);
    assert_eq!(last_decision.target_phase.as_deref(), Some("testing"));
}

#[test]
fn default_max_attempts_is_3_when_no_config() {
    use crate::types::{PhaseDecision, PhaseDecisionVerdict, WorkflowDecisionRisk};

    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);

    let mut workflow = executor.bootstrap(
        "WF-default-retry".to_string(),
        WorkflowRunInput::for_task("TASK-default-retry".to_string(), Some("standard-workflow".to_string())),
    );

    for i in 0..3 {
        workflow.rework_counts.insert("implementation".to_string(), i);
        executor
            .mark_current_phase_success_with_decision(
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
                    target_phase: None,
                }),
            )
            .expect("phase success");
        if i < 2 {
            assert_eq!(
                workflow.status,
                WorkflowStatus::Running,
                "should still be running after rework {} with default max_attempts=3",
                i + 1
            );
        }
    }

    workflow.rework_counts.insert("implementation".to_string(), 3);
    executor
        .mark_current_phase_success_with_decision(
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
                target_phase: None,
            }),
        )
        .expect("phase success");
    assert_eq!(
        workflow.status,
        WorkflowStatus::Escalated,
        "should escalate when rework_count reaches default max_attempts=3"
    );
}

#[test]
fn on_verdict_rework_routes_to_configured_phase() {
    use crate::workflow_config::PhaseTransitionConfig;
    use std::collections::HashMap;

    let mut verdict_routing = HashMap::new();
    let mut code_review_verdicts = HashMap::new();
    code_review_verdicts.insert(
        "rework".to_string(),
        PhaseTransitionConfig {
            target: "requirements".to_string(),
            guard: None,
            allow_agent_target: false,
            allowed_targets: Vec::new(),
        },
    );
    verdict_routing.insert("code-review".to_string(), code_review_verdicts);

    let executor = WorkflowLifecycleExecutor::with_verdict_routing(
        vec!["requirements".to_string(), "implementation".to_string(), "code-review".to_string()],
        verdict_routing,
    );
    let mut workflow = executor.bootstrap(
        "WF-verdict-rework".to_string(),
        WorkflowRunInput::for_task("TASK-verdict-rework".to_string(), Some("standard-workflow".to_string())),
    );
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.current_phase.as_deref(), Some("code-review"));
    assert_eq!(workflow.current_phase_index, 2);

    let decision = make_rework_decision(None);
    executor.mark_current_phase_success_with_decision(&mut workflow, Some(decision)).expect("phase success");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase_index, 0);
    assert_eq!(workflow.current_phase.as_deref(), Some("requirements"));
    assert_eq!(workflow.phases[0].status, WorkflowPhaseStatus::Running);

    let last_decision = workflow.decision_history.last().unwrap();
    assert_eq!(last_decision.decision, WorkflowDecisionAction::Rework);
    assert_eq!(last_decision.target_phase.as_deref(), Some("requirements"));
}

#[test]
fn backoff_calculation() {
    use crate::agent_runtime_config::BackoffConfig;

    let backoff = BackoffConfig { initial_secs: 10, factor: 2.0, max_secs: Some(120) };

    assert_eq!(backoff.delay_for_attempt(0), 0);
    assert_eq!(backoff.delay_for_attempt(1), 10);
    assert_eq!(backoff.delay_for_attempt(2), 20);
    assert_eq!(backoff.delay_for_attempt(3), 40);
    assert_eq!(backoff.delay_for_attempt(4), 80);
    assert_eq!(backoff.delay_for_attempt(5), 120);
    assert_eq!(backoff.delay_for_attempt(6), 120);

    let no_cap = BackoffConfig { initial_secs: 5, factor: 3.0, max_secs: None };
    assert_eq!(no_cap.delay_for_attempt(1), 5);
    assert_eq!(no_cap.delay_for_attempt(2), 15);
    assert_eq!(no_cap.delay_for_attempt(3), 45);

    let linear = BackoffConfig { initial_secs: 30, factor: 1.0, max_secs: None };
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
            backoff: Some(BackoffConfig { initial_secs: 10, factor: 2.0, max_secs: Some(60) }),
        },
    );

    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]).with_retry_configs(retry_configs);

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

#[test]
fn on_verdict_advance_skips_to_configured_phase() {
    use crate::workflow_config::PhaseTransitionConfig;
    use std::collections::HashMap;

    let mut verdict_routing = HashMap::new();
    let mut requirements_verdicts = HashMap::new();
    requirements_verdicts.insert(
        "advance".to_string(),
        PhaseTransitionConfig {
            target: "code-review".to_string(),
            guard: None,
            allow_agent_target: false,
            allowed_targets: Vec::new(),
        },
    );
    verdict_routing.insert("requirements".to_string(), requirements_verdicts);

    let executor = WorkflowLifecycleExecutor::with_verdict_routing(
        vec![
            "requirements".to_string(),
            "implementation".to_string(),
            "code-review".to_string(),
            "testing".to_string(),
        ],
        verdict_routing,
    );
    let mut workflow = executor.bootstrap(
        "WF-verdict-advance".to_string(),
        WorkflowRunInput::for_task("TASK-verdict-advance".to_string(), Some("standard-workflow".to_string())),
    );
    assert_eq!(workflow.current_phase.as_deref(), Some("requirements"));

    executor.mark_current_phase_success(&mut workflow).expect("phase success");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase_index, 2);
    assert_eq!(workflow.current_phase.as_deref(), Some("code-review"));
    assert_eq!(workflow.phases[2].status, WorkflowPhaseStatus::Running);
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Pending,);

    let last_decision = workflow.decision_history.last().unwrap();
    assert_eq!(last_decision.decision, WorkflowDecisionAction::Advance);
    assert_eq!(last_decision.target_phase.as_deref(), Some("code-review"));
}

#[test]
fn no_on_verdict_uses_default_advance_behavior() {
    let executor = WorkflowLifecycleExecutor::new(vec![
        "requirements".to_string(),
        "implementation".to_string(),
        "code-review".to_string(),
    ]);
    let mut workflow = executor.bootstrap(
        "WF-default-advance".to_string(),
        WorkflowRunInput::for_task("TASK-default-advance".to_string(), Some("standard-workflow".to_string())),
    );
    assert_eq!(workflow.current_phase.as_deref(), Some("requirements"));

    executor.mark_current_phase_success(&mut workflow).expect("phase success");

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.current_phase_index, 1);
    assert_eq!(workflow.current_phase.as_deref(), Some("implementation"));
    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Running);

    executor.mark_current_phase_success(&mut workflow).expect("phase success");

    assert_eq!(workflow.current_phase_index, 2);
    assert_eq!(workflow.current_phase.as_deref(), Some("code-review"));
}

#[test]
fn machine_state_to_workflow_status_mapping() {
    assert_eq!(WorkflowMachineState::Idle.to_workflow_status(), WorkflowStatus::Pending);
    assert_eq!(WorkflowMachineState::EvaluateTransition.to_workflow_status(), WorkflowStatus::Running);
    assert_eq!(WorkflowMachineState::RunPhase.to_workflow_status(), WorkflowStatus::Running);
    assert_eq!(WorkflowMachineState::EvaluateGates.to_workflow_status(), WorkflowStatus::Running);
    assert_eq!(WorkflowMachineState::ApplyTransition.to_workflow_status(), WorkflowStatus::Running);
    assert_eq!(WorkflowMachineState::Paused.to_workflow_status(), WorkflowStatus::Paused);
    assert_eq!(WorkflowMachineState::Completed.to_workflow_status(), WorkflowStatus::Completed);
    assert_eq!(WorkflowMachineState::MergeConflict.to_workflow_status(), WorkflowStatus::Running);
    assert_eq!(WorkflowMachineState::Failed.to_workflow_status(), WorkflowStatus::Failed);
    assert_eq!(WorkflowMachineState::HumanEscalated.to_workflow_status(), WorkflowStatus::Escalated);
    assert_eq!(WorkflowMachineState::Cancelled.to_workflow_status(), WorkflowStatus::Cancelled);
}

#[test]
fn sync_status_derives_from_machine_state() {
    let mut workflow = make_workflow(WorkflowStatus::Pending);
    workflow.machine_state = WorkflowMachineState::RunPhase;
    workflow.sync_status();
    assert_eq!(workflow.status, WorkflowStatus::Running);

    workflow.machine_state = WorkflowMachineState::Paused;
    workflow.sync_status();
    assert_eq!(workflow.status, WorkflowStatus::Paused);

    workflow.machine_state = WorkflowMachineState::Completed;
    workflow.sync_status();
    assert_eq!(workflow.status, WorkflowStatus::Completed);

    workflow.machine_state = WorkflowMachineState::Failed;
    workflow.sync_status();
    assert_eq!(workflow.status, WorkflowStatus::Failed);

    workflow.machine_state = WorkflowMachineState::HumanEscalated;
    workflow.sync_status();
    assert_eq!(workflow.status, WorkflowStatus::Escalated);

    workflow.machine_state = WorkflowMachineState::Cancelled;
    workflow.sync_status();
    assert_eq!(workflow.status, WorkflowStatus::Cancelled);
}

#[test]
fn bootstrap_derives_status_from_machine_state() {
    let executor = WorkflowLifecycleExecutor::new(vec!["requirements".to_string(), "implementation".to_string()]);
    let workflow = executor.bootstrap(
        "WF-derive-test".to_string(),
        WorkflowRunInput::for_task("TASK-derive".to_string(), Some("standard-workflow".to_string())),
    );
    assert_eq!(workflow.status, workflow.machine_state.to_workflow_status());
    assert_eq!(workflow.status, WorkflowStatus::Running);
}

#[test]
fn status_stays_in_sync_through_lifecycle_transitions() {
    let executor = WorkflowLifecycleExecutor::new(vec!["requirements".to_string(), "implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-sync-test".to_string(),
        WorkflowRunInput::for_task("TASK-sync".to_string(), Some("standard-workflow".to_string())),
    );
    assert_eq!(workflow.status, workflow.machine_state.to_workflow_status());

    executor.pause(&mut workflow);
    assert_eq!(workflow.status, workflow.machine_state.to_workflow_status());
    assert_eq!(workflow.status, WorkflowStatus::Paused);

    executor.resume(&mut workflow);
    assert_eq!(workflow.status, workflow.machine_state.to_workflow_status());
    assert_eq!(workflow.status, WorkflowStatus::Running);

    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.status, workflow.machine_state.to_workflow_status());

    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.status, workflow.machine_state.to_workflow_status());
    assert_eq!(workflow.status, WorkflowStatus::Completed);
}

#[test]
fn cancel_keeps_status_synced_with_machine_state() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-cancel-sync".to_string(),
        WorkflowRunInput::for_task("TASK-cancel-sync".to_string(), Some("standard-workflow".to_string())),
    );

    executor.cancel(&mut workflow);
    assert_eq!(workflow.status, workflow.machine_state.to_workflow_status());
    assert_eq!(workflow.status, WorkflowStatus::Cancelled);
}

#[test]
fn failed_phase_keeps_status_synced_with_machine_state() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-fail-sync".to_string(),
        WorkflowRunInput::for_task("TASK-fail-sync".to_string(), Some("standard-workflow".to_string())),
    );

    executor.mark_current_phase_failed(&mut workflow, "test error".to_string()).expect("phase failure");
    assert_eq!(workflow.status, workflow.machine_state.to_workflow_status());
}

#[test]
fn load_workflow_ref_index_decodes_compressed_workflow_blobs() {
    use crate::workflow::state_manager::load_workflow_ref_index;
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());
    let workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");
    let index = load_workflow_ref_index(temp.path()).expect("load workflow ref index");
    assert_eq!(index.get("WF-test").map(String::as_str), Some("standard-workflow"));
}

#[test]
fn state_manager_cleanup_deletes_old_terminal_workflows() {
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let mut old_completed = make_workflow(WorkflowStatus::Completed);
    old_completed.id = "WF-old-completed".to_string();
    old_completed.machine_state = WorkflowMachineState::Completed;
    old_completed.completed_at = Some(Utc::now() - chrono::Duration::hours(48));
    manager.save(&old_completed).expect("save old completed workflow");

    let mut old_failed = make_workflow(WorkflowStatus::Failed);
    old_failed.id = "WF-old-failed".to_string();
    old_failed.machine_state = WorkflowMachineState::Failed;
    old_failed.started_at = Utc::now() - chrono::Duration::hours(48);
    manager.save(&old_failed).expect("save old failed workflow");

    let mut running = make_workflow(WorkflowStatus::Running);
    running.id = "WF-running".to_string();
    running.started_at = Utc::now() - chrono::Duration::hours(48);
    manager.save(&running).expect("save running workflow");

    let mut recent_completed = make_workflow(WorkflowStatus::Completed);
    recent_completed.id = "WF-recent-completed".to_string();
    recent_completed.machine_state = WorkflowMachineState::Completed;
    recent_completed.completed_at = Some(Utc::now());
    manager.save(&recent_completed).expect("save recent completed workflow");

    let result = manager.cleanup_terminal_workflows(24).expect("cleanup terminal workflows");
    assert_eq!(result.deleted, 2);

    let remaining: Vec<String> =
        manager.list_all().expect("list all workflows").into_iter().map(|workflow| workflow.id).collect();
    assert!(remaining.contains(&"WF-running".to_string()));
    assert!(remaining.contains(&"WF-recent-completed".to_string()));
    assert!(!remaining.contains(&"WF-old-completed".to_string()));
    assert!(!remaining.contains(&"WF-old-failed".to_string()));
}

#[test]
fn state_manager_cleanup_deletes_terminal_workflows_with_null_timestamps() {
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let mut legacy = make_workflow(WorkflowStatus::Completed);
    legacy.id = "WF-legacy-null".to_string();
    legacy.machine_state = WorkflowMachineState::Completed;
    manager.save(&legacy).expect("save legacy workflow");

    let mut pending = make_workflow(WorkflowStatus::Pending);
    pending.id = "WF-pending-null".to_string();
    manager.save(&pending).expect("save pending workflow");

    let conn = crate::workflow::open_project_db(temp.path()).expect("open db");
    conn.execute("UPDATE workflows SET started_at = NULL, completed_at = NULL", []).expect("null out timestamps");
    drop(conn);

    let result = manager.cleanup_terminal_workflows(24).expect("cleanup terminal workflows");
    assert_eq!(result.deleted, 1, "terminal rows with NULL timestamps should be treated as old");
    let remaining: Vec<String> =
        manager.list_all().expect("list all workflows").into_iter().map(|workflow| workflow.id).collect();
    assert_eq!(remaining, vec!["WF-pending-null".to_string()], "pending rows with NULL timestamps must survive");
}

#[test]
fn save_checkpoint_skips_numbers_of_orphan_checkpoint_rows() {
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");

    let conn = crate::workflow::open_project_db(temp.path()).expect("open db");
    conn.execute(
        "INSERT INTO checkpoints (workflow_id, number, timestamp, reason, phase_id, machine_state, status, snapshot_json)
         VALUES (?1, 3, ?2, 'orphan', 'requirements', 'idle', 'running', 'orphan-snapshot')",
        rusqlite::params![workflow.id, Utc::now().to_rfc3339()],
    )
    .expect("insert orphan checkpoint row");
    drop(conn);

    let updated = manager.save_checkpoint(&workflow, CheckpointReason::Start).expect("save checkpoint");
    assert_eq!(updated.checkpoint_metadata.checkpoint_count, 4, "number allocation must skip persisted rows");
    assert_eq!(manager.list_checkpoints(&workflow.id).expect("list checkpoints"), vec![3, 4]);

    let conn = crate::workflow::open_project_db(temp.path()).expect("reopen db");
    let orphan_reason: String = conn
        .query_row(
            "SELECT reason FROM checkpoints WHERE workflow_id = ?1 AND number = 3",
            rusqlite::params![workflow.id],
            |row| row.get(0),
        )
        .expect("orphan checkpoint row");
    assert_eq!(orphan_reason, "orphan", "orphan snapshot must not be overwritten");
}

#[test]
fn save_checkpoint_rolls_back_checkpoint_row_when_workflow_save_fails() {
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let workflow = make_workflow(WorkflowStatus::Running);
    manager.save(&workflow).expect("save workflow");

    let conn = crate::workflow::open_project_db(temp.path()).expect("open db");
    conn.execute_batch(
        "CREATE TRIGGER fail_workflow_save BEFORE INSERT ON workflows
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    )
    .expect("create failure trigger");
    drop(conn);

    manager.save_checkpoint(&workflow, CheckpointReason::Start).expect_err("workflow save failure should propagate");
    assert!(
        manager.list_checkpoints(&workflow.id).expect("list checkpoints").is_empty(),
        "checkpoint row must roll back when the workflow save fails"
    );

    let conn = crate::workflow::open_project_db(temp.path()).expect("reopen db");
    conn.execute_batch("DROP TRIGGER fail_workflow_save;").expect("drop failure trigger");
    drop(conn);

    let updated = manager.save_checkpoint(&workflow, CheckpointReason::Start).expect("save checkpoint after retry");
    assert_eq!(updated.checkpoint_metadata.checkpoint_count, 1);
    assert_eq!(manager.list_checkpoints(&workflow.id).expect("list checkpoints"), vec![1]);
}

#[test]
fn lifecycle_double_pause_is_noop() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-double-pause".to_string(),
        WorkflowRunInput::for_task("TASK-pause".to_string(), Some("standard-workflow".to_string())),
    );

    executor.pause(&mut workflow);
    assert_eq!(workflow.status, WorkflowStatus::Paused);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Paused);

    executor.pause(&mut workflow);
    assert_eq!(workflow.status, WorkflowStatus::Paused);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Paused);
}

#[test]
fn lifecycle_resume_on_running_is_noop() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-resume-running".to_string(),
        WorkflowRunInput::for_task("TASK-resume-running".to_string(), Some("standard-workflow".to_string())),
    );
    assert_eq!(workflow.status, WorkflowStatus::Running);
    let machine_state_before = workflow.machine_state;
    let attempt_before = workflow.phases[workflow.current_phase_index].attempt;

    executor.resume(&mut workflow);

    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.machine_state, machine_state_before);
    assert_eq!(workflow.phases[workflow.current_phase_index].attempt, attempt_before);
}

#[test]
fn resume_manager_detects_interrupted_failed_and_escalated_workflows() {
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = WorkflowStateManager::new(temp.path());

    let mut failed = make_workflow(WorkflowStatus::Failed);
    failed.id = "WF-failed".to_string();
    failed.machine_state = WorkflowMachineState::Failed;
    manager.save(&failed).expect("save failed workflow");

    let mut escalated = make_workflow(WorkflowStatus::Escalated);
    escalated.id = "WF-escalated".to_string();
    escalated.machine_state = WorkflowMachineState::HumanEscalated;
    manager.save(&escalated).expect("save escalated workflow");

    let mut completed = make_workflow(WorkflowStatus::Completed);
    completed.id = "WF-completed".to_string();
    completed.machine_state = WorkflowMachineState::Completed;
    completed.completed_at = Some(Utc::now());
    manager.save(&completed).expect("save completed workflow");

    let mut resume_manager = WorkflowResumeManager::new(temp.path()).expect("resume manager");
    resume_manager.config.resume_failed = true;

    let interrupted: Vec<String> = resume_manager
        .detect_interrupted_workflows()
        .expect("detect interrupted workflows")
        .into_iter()
        .map(|workflow| workflow.id)
        .collect();
    assert!(interrupted.contains(&"WF-failed".to_string()));
    assert!(interrupted.contains(&"WF-escalated".to_string()));
    assert!(!interrupted.contains(&"WF-completed".to_string()));

    resume_manager.config.resume_failed = false;
    let interrupted: Vec<String> = resume_manager
        .detect_interrupted_workflows()
        .expect("detect interrupted workflows")
        .into_iter()
        .map(|workflow| workflow.id)
        .collect();
    assert!(!interrupted.contains(&"WF-failed".to_string()));
    assert!(interrupted.contains(&"WF-escalated".to_string()));
}

#[test]
fn load_stale_task_summaries_matches_in_progress_status() {
    crate::test_env::stable_test_home();
    let temp = tempfile::tempdir().expect("tempdir");

    let mut task = make_task(TaskType::Feature, Priority::Medium);
    task.id = "TASK-stale".to_string();
    task.metadata.updated_at = Utc::now() - chrono::Duration::hours(48);
    save_task(temp.path(), &task).expect("save stale in-progress task");

    let stale = load_stale_task_summaries(temp.path(), Utc::now() - chrono::Duration::hours(24))
        .expect("load stale task summaries");
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].task_id, "TASK-stale");
}

#[test]
fn merge_conflict_blocks_phase_success_with_actionable_error() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-mc-success".to_string(),
        WorkflowRunInput::for_task("TASK-mc-success".to_string(), Some("standard-workflow".to_string())),
    );
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    executor.mark_merge_conflict(&mut workflow, "merge failed".to_string());
    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.machine_state, WorkflowMachineState::MergeConflict);

    let error =
        executor.mark_current_phase_success(&mut workflow).expect_err("merge conflict should block phase success");
    assert!(error.to_string().contains("merge conflict"), "unexpected error: {error}");
    assert_eq!(workflow.machine_state, WorkflowMachineState::MergeConflict);
    assert_eq!(workflow.failure_reason.as_deref(), Some("merge failed"));
}

#[test]
fn merge_conflict_blocks_phase_failure_with_actionable_error() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-mc-failure".to_string(),
        WorkflowRunInput::for_task("TASK-mc-failure".to_string(), Some("standard-workflow".to_string())),
    );
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    executor.mark_merge_conflict(&mut workflow, "merge failed".to_string());
    assert_eq!(workflow.machine_state, WorkflowMachineState::MergeConflict);

    let error = executor
        .mark_current_phase_failed(&mut workflow, "post-merge failure".to_string())
        .expect_err("merge conflict should block phase failure");
    assert!(error.to_string().contains("merge conflict"), "unexpected error: {error}");
    assert_eq!(workflow.machine_state, WorkflowMachineState::MergeConflict);
    assert_eq!(workflow.failure_reason.as_deref(), Some("merge failed"));
    assert_eq!(workflow.phases[0].status, WorkflowPhaseStatus::Success);
}

#[test]
fn rework_restart_refreshes_phase_started_at() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string(), "code-review".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-rework-started-at".to_string(),
        WorkflowRunInput::for_task("TASK-rework-started-at".to_string(), Some("standard-workflow".to_string())),
    );
    executor.mark_current_phase_success(&mut workflow).expect("phase success");
    assert_eq!(workflow.current_phase.as_deref(), Some("code-review"));
    let original_started_at = workflow.phases[1].started_at.expect("review phase started");

    std::thread::sleep(std::time::Duration::from_millis(5));
    let decision = make_rework_decision(None);
    executor.mark_current_phase_success_with_decision(&mut workflow, Some(decision)).expect("phase success");

    assert_eq!(workflow.phases[1].status, WorkflowPhaseStatus::Running);
    assert!(workflow.phases[1].completed_at.is_none());
    let restarted_at = workflow.phases[1].started_at.expect("review phase restarted");
    assert!(restarted_at > original_started_at, "rework restart should refresh started_at");
}

#[test]
fn phase_failure_while_paused_records_failure_and_resume_restarts() {
    let executor = WorkflowLifecycleExecutor::new(vec!["implementation".to_string()]);
    let mut workflow = executor.bootstrap(
        "WF-paused-failure".to_string(),
        WorkflowRunInput::for_task("TASK-paused-failure".to_string(), Some("standard-workflow".to_string())),
    );
    executor.pause(&mut workflow);
    assert_eq!(workflow.status, WorkflowStatus::Paused);
    let attempt_before = workflow.phases[0].attempt;

    executor.mark_current_phase_failed(&mut workflow, "agent crashed".to_string()).expect("paused failure recorded");
    assert_eq!(workflow.status, WorkflowStatus::Paused);
    assert_eq!(workflow.machine_state, WorkflowMachineState::Paused);
    assert_eq!(workflow.phases[0].status, WorkflowPhaseStatus::Failed);
    assert_eq!(workflow.phases[0].error_message.as_deref(), Some("agent crashed"));

    executor.resume(&mut workflow);
    assert_eq!(workflow.status, WorkflowStatus::Running);
    assert_eq!(workflow.machine_state, WorkflowMachineState::RunPhase);
    assert_eq!(workflow.phases[0].status, WorkflowPhaseStatus::Running);
    assert_eq!(workflow.phases[0].attempt, attempt_before + 1);
    assert!(workflow.phases[0].error_message.is_none());
    assert!(workflow.phases[0].completed_at.is_none());
}
