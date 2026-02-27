use chrono::Utc;

use crate::state_machines::{builtin_compiled_state_machines, CompiledStateMachines};
use crate::types::{
    OrchestratorWorkflow, PhaseDecision, PhaseDecisionVerdict, WorkflowDecisionAction,
    WorkflowDecisionRecord, WorkflowDecisionRisk, WorkflowDecisionSource, WorkflowMachineEvent,
    WorkflowMachineState, WorkflowPhaseExecution, WorkflowPhaseStatus, WorkflowRunInput,
    WorkflowStatus,
};

const MAX_PHASE_REWORKS: u32 = 3;

enum GateEvaluationResult {
    Pass,
    Rework { reason: String },
    Fail { reason: String },
}

use super::phase_plan::{phase_plan_for_pipeline_id, STANDARD_PIPELINE_ID};
use super::state_machine::WorkflowStateMachine;

#[derive(Debug, Clone)]
pub struct WorkflowLifecycleExecutor {
    phase_plan: Vec<String>,
    state_machines: CompiledStateMachines,
}

impl Default for WorkflowLifecycleExecutor {
    fn default() -> Self {
        Self {
            phase_plan: phase_plan_for_pipeline_id(Some(STANDARD_PIPELINE_ID)),
            state_machines: builtin_compiled_state_machines(),
        }
    }
}

impl WorkflowLifecycleExecutor {
    pub fn new(phase_plan: Vec<String>) -> Self {
        Self::with_state_machines(phase_plan, builtin_compiled_state_machines())
    }

    pub fn with_state_machines(
        phase_plan: Vec<String>,
        state_machines: CompiledStateMachines,
    ) -> Self {
        Self {
            phase_plan,
            state_machines,
        }
    }

    fn state_machine(&self, initial: WorkflowMachineState) -> WorkflowStateMachine {
        WorkflowStateMachine::with_definition(initial, self.state_machines.workflow.clone())
    }

    fn machine_metadata(&self) -> (Option<u32>, Option<String>, Option<String>) {
        (
            Some(self.state_machines.metadata.version),
            Some(self.state_machines.metadata.hash.clone()),
            Some(self.state_machines.metadata.source.as_str().to_string()),
        )
    }

    fn decision_record(
        &self,
        phase_id: String,
        decision: WorkflowDecisionAction,
        target_phase: Option<String>,
        reason: String,
        confidence: f32,
        risk: WorkflowDecisionRisk,
    ) -> WorkflowDecisionRecord {
        let (machine_version, machine_hash, machine_source) = self.machine_metadata();
        WorkflowDecisionRecord {
            timestamp: Utc::now(),
            phase_id,
            source: WorkflowDecisionSource::Fallback,
            decision,
            target_phase,
            reason,
            confidence,
            risk,
            guardrail_violations: Vec::new(),
            machine_version,
            machine_hash,
            machine_source,
        }
    }

    pub fn bootstrap(&self, workflow_id: String, input: WorkflowRunInput) -> OrchestratorWorkflow {
        let now = Utc::now();
        let mut phases: Vec<WorkflowPhaseExecution> = self
            .phase_plan
            .iter()
            .map(|phase_id| WorkflowPhaseExecution {
                phase_id: phase_id.clone(),
                status: WorkflowPhaseStatus::Pending,
                started_at: None,
                completed_at: None,
                attempt: 0,
                error_message: None,
            })
            .collect();

        let mut machine = self.state_machine(self.state_machines.workflow.initial_state());
        machine.apply(WorkflowMachineEvent::Start);
        machine.apply(WorkflowMachineEvent::PhaseStarted);

        if let Some(first) = phases.first_mut() {
            first.status = WorkflowPhaseStatus::Running;
            first.started_at = Some(now);
            first.attempt = 1;
        }

        OrchestratorWorkflow {
            id: workflow_id,
            task_id: input.task_id,
            pipeline_id: input.pipeline_id,
            status: WorkflowStatus::Running,
            current_phase_index: 0,
            phases,
            machine_state: machine.state(),
            current_phase: self.phase_plan.first().cloned(),
            started_at: now,
            completed_at: None,
            failure_reason: None,
            checkpoint_metadata: crate::types::WorkflowCheckpointMetadata::default(),
            rework_counts: std::collections::HashMap::new(),
            total_reworks: 0,
            decision_history: Vec::new(),
        }
    }

    pub fn pause(&self, workflow: &mut OrchestratorWorkflow) {
        if matches!(
            workflow.status,
            WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
        ) {
            return;
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine.apply(WorkflowMachineEvent::PauseRequested);
        workflow.machine_state = machine.state();
        workflow.status = WorkflowStatus::Paused;
    }

    pub fn resume(&self, workflow: &mut OrchestratorWorkflow) {
        if matches!(
            workflow.status,
            WorkflowStatus::Completed | WorkflowStatus::Cancelled
        ) {
            return;
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine.apply(WorkflowMachineEvent::ResumeRequested);
        machine.apply(WorkflowMachineEvent::PhaseStarted);
        workflow.machine_state = machine.state();
        workflow.status = WorkflowStatus::Running;
        workflow.completed_at = None;
        workflow.failure_reason = None;

        if let Some(phase) = workflow.phases.get_mut(workflow.current_phase_index) {
            if matches!(
                phase.status,
                WorkflowPhaseStatus::Pending
                    | WorkflowPhaseStatus::Ready
                    | WorkflowPhaseStatus::Failed
            ) {
                phase.status = WorkflowPhaseStatus::Running;
                phase.started_at = Some(Utc::now());
                phase.attempt += 1;
                phase.error_message = None;
            }
        }
    }

    pub fn cancel(&self, workflow: &mut OrchestratorWorkflow) {
        if matches!(
            workflow.status,
            WorkflowStatus::Completed | WorkflowStatus::Cancelled
        ) {
            return;
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine.apply(WorkflowMachineEvent::CancelRequested);
        workflow.machine_state = machine.state();
        workflow.status = WorkflowStatus::Cancelled;
        workflow.completed_at = Some(Utc::now());
    }

    pub fn mark_current_phase_success(&self, workflow: &mut OrchestratorWorkflow) {
        self.mark_current_phase_success_with_decision(workflow, None);
    }

    pub fn mark_current_phase_success_with_decision(
        &self,
        workflow: &mut OrchestratorWorkflow,
        decision: Option<PhaseDecision>,
    ) {
        if !matches!(workflow.status, WorkflowStatus::Running) {
            return;
        }
        workflow.failure_reason = None;
        workflow.completed_at = None;

        let current_phase_id = workflow
            .phases
            .get(workflow.current_phase_index)
            .map(|phase| phase.phase_id.clone())
            .unwrap_or_else(|| "unknown".to_string());

        if let Some(phase) = workflow.phases.get_mut(workflow.current_phase_index) {
            phase.status = WorkflowPhaseStatus::Success;
            phase.completed_at = Some(Utc::now());
            phase.error_message = None;
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine.apply(WorkflowMachineEvent::PhaseSucceeded);

        match self.evaluate_gates(&decision, workflow) {
            GateEvaluationResult::Pass => {
                machine.apply(WorkflowMachineEvent::GatesPassed);
                machine.apply(WorkflowMachineEvent::PolicyDecisionReady);

                let (confidence, risk, source) = match &decision {
                    Some(d) => (d.confidence, d.risk, WorkflowDecisionSource::Llm),
                    None => (
                        1.0,
                        WorkflowDecisionRisk::Low,
                        WorkflowDecisionSource::Fallback,
                    ),
                };
                let (machine_version, machine_hash, machine_source) = self.machine_metadata();
                let guardrail_violations = decision
                    .as_ref()
                    .map(|d| d.guardrail_violations.clone())
                    .unwrap_or_default();

                let next_idx = workflow.current_phase_index + 1;
                if next_idx < workflow.phases.len() {
                    let next_phase_id = workflow.phases[next_idx].phase_id.clone();
                    workflow.current_phase_index = next_idx;
                    if let Some(phase) = workflow.phases.get_mut(next_idx) {
                        phase.status = WorkflowPhaseStatus::Running;
                        phase.started_at = Some(Utc::now());
                        phase.attempt += 1;
                        workflow.current_phase = Some(phase.phase_id.clone());
                    }
                    workflow.decision_history.push(WorkflowDecisionRecord {
                        timestamp: Utc::now(),
                        phase_id: current_phase_id,
                        decision: WorkflowDecisionAction::Advance,
                        target_phase: Some(next_phase_id),
                        reason: decision
                            .as_ref()
                            .map(|d| d.reason.clone())
                            .filter(|r| !r.is_empty())
                            .unwrap_or_else(|| "phase completed successfully".to_string()),
                        confidence,
                        risk,
                        source,
                        guardrail_violations: guardrail_violations.clone(),
                        machine_version,
                        machine_hash: machine_hash.clone(),
                        machine_source: machine_source.clone(),
                    });
                    machine.apply(WorkflowMachineEvent::Start);
                    machine.apply(WorkflowMachineEvent::PhaseStarted);
                    workflow.machine_state = machine.state();
                    workflow.status = WorkflowStatus::Running;
                } else {
                    workflow.decision_history.push(WorkflowDecisionRecord {
                        timestamp: Utc::now(),
                        phase_id: current_phase_id,
                        decision: WorkflowDecisionAction::Advance,
                        target_phase: None,
                        reason: "workflow completed all phases".to_string(),
                        confidence,
                        risk,
                        source,
                        guardrail_violations,
                        machine_version,
                        machine_hash,
                        machine_source,
                    });
                    machine.apply(WorkflowMachineEvent::NoMorePhases);
                    workflow.machine_state = machine.state();
                    workflow.status = WorkflowStatus::Completed;
                    workflow.completed_at = Some(Utc::now());
                    workflow.current_phase = None;
                }
            }
            GateEvaluationResult::Rework { reason } => {
                machine.apply(WorkflowMachineEvent::GatesFailed);
                workflow.machine_state = machine.state();

                let rework_count = workflow
                    .rework_counts
                    .entry(current_phase_id.clone())
                    .or_insert(0);
                *rework_count += 1;

                if let Some(phase) = workflow.phases.get_mut(workflow.current_phase_index) {
                    phase.status = WorkflowPhaseStatus::Running;
                    phase.completed_at = None;
                    phase.attempt += 1;
                }

                let confidence = decision.as_ref().map(|d| d.confidence).unwrap_or(0.5);
                let risk = decision
                    .as_ref()
                    .map(|d| d.risk)
                    .unwrap_or(WorkflowDecisionRisk::Medium);
                let (machine_version, machine_hash, machine_source) = self.machine_metadata();
                workflow.decision_history.push(WorkflowDecisionRecord {
                    timestamp: Utc::now(),
                    phase_id: current_phase_id,
                    decision: WorkflowDecisionAction::Rework,
                    target_phase: None,
                    reason,
                    confidence,
                    risk,
                    source: WorkflowDecisionSource::Llm,
                    guardrail_violations: decision
                        .as_ref()
                        .map(|d| d.guardrail_violations.clone())
                        .unwrap_or_default(),
                    machine_version,
                    machine_hash,
                    machine_source,
                });

                let mut machine = self.state_machine(workflow.machine_state);
                machine.apply(WorkflowMachineEvent::Start);
                machine.apply(WorkflowMachineEvent::PhaseStarted);
                workflow.machine_state = machine.state();
                workflow.status = WorkflowStatus::Running;
            }
            GateEvaluationResult::Fail { reason } => {
                machine.apply(WorkflowMachineEvent::GatesFailed);
                machine.apply(WorkflowMachineEvent::PolicyDecisionFailed);
                machine.apply(WorkflowMachineEvent::ReworkBudgetExceeded);
                workflow.machine_state = machine.state();
                workflow.status = WorkflowStatus::Failed;
                workflow.completed_at = Some(Utc::now());
                workflow.failure_reason = Some(reason.clone());

                let confidence = decision.as_ref().map(|d| d.confidence).unwrap_or(0.5);
                let risk = decision
                    .as_ref()
                    .map(|d| d.risk)
                    .unwrap_or(WorkflowDecisionRisk::High);
                let (machine_version, machine_hash, machine_source) = self.machine_metadata();
                workflow.decision_history.push(WorkflowDecisionRecord {
                    timestamp: Utc::now(),
                    phase_id: current_phase_id,
                    decision: WorkflowDecisionAction::Fail,
                    target_phase: None,
                    reason,
                    confidence,
                    risk,
                    source: WorkflowDecisionSource::Llm,
                    guardrail_violations: decision
                        .as_ref()
                        .map(|d| d.guardrail_violations.clone())
                        .unwrap_or_default(),
                    machine_version,
                    machine_hash,
                    machine_source,
                });
            }
        }
    }

    fn evaluate_gates(
        &self,
        decision: &Option<PhaseDecision>,
        workflow: &OrchestratorWorkflow,
    ) -> GateEvaluationResult {
        let decision = match decision {
            Some(d) => d,
            None => return GateEvaluationResult::Pass,
        };

        match decision.verdict {
            PhaseDecisionVerdict::Fail => {
                return GateEvaluationResult::Fail {
                    reason: if decision.reason.is_empty() {
                        "agent declared phase failed".to_string()
                    } else {
                        decision.reason.clone()
                    },
                };
            }
            PhaseDecisionVerdict::Rework => {
                let phase_id = workflow
                    .phases
                    .get(workflow.current_phase_index)
                    .map(|p| p.phase_id.as_str())
                    .unwrap_or("unknown");
                let rework_count = workflow.rework_counts.get(phase_id).copied().unwrap_or(0);
                if rework_count >= MAX_PHASE_REWORKS {
                    return GateEvaluationResult::Fail {
                        reason: format!(
                            "rework budget exceeded for phase {} ({} reworks): {}",
                            phase_id,
                            rework_count,
                            if decision.reason.is_empty() {
                                "agent requested rework"
                            } else {
                                &decision.reason
                            }
                        ),
                    };
                }
                return GateEvaluationResult::Rework {
                    reason: if decision.reason.is_empty() {
                        "agent requested rework".to_string()
                    } else {
                        decision.reason.clone()
                    },
                };
            }
            PhaseDecisionVerdict::Advance | PhaseDecisionVerdict::Skip => {
                if decision.confidence < 0.5 && matches!(decision.risk, WorkflowDecisionRisk::High)
                {
                    let phase_id = workflow
                        .phases
                        .get(workflow.current_phase_index)
                        .map(|p| p.phase_id.as_str())
                        .unwrap_or("unknown");
                    let rework_count = workflow.rework_counts.get(phase_id).copied().unwrap_or(0);
                    if rework_count < MAX_PHASE_REWORKS {
                        return GateEvaluationResult::Rework {
                            reason: format!(
                                "low confidence ({:.2}) with high risk — requesting rework",
                                decision.confidence
                            ),
                        };
                    }
                }
                if !decision.guardrail_violations.is_empty() {
                    let phase_id = workflow
                        .phases
                        .get(workflow.current_phase_index)
                        .map(|p| p.phase_id.as_str())
                        .unwrap_or("unknown");
                    let rework_count = workflow.rework_counts.get(phase_id).copied().unwrap_or(0);
                    if rework_count < MAX_PHASE_REWORKS {
                        return GateEvaluationResult::Rework {
                            reason: format!(
                                "guardrail violations: {}",
                                decision.guardrail_violations.join("; ")
                            ),
                        };
                    }
                }
                GateEvaluationResult::Pass
            }
            PhaseDecisionVerdict::Unknown => GateEvaluationResult::Pass,
        }
    }

    pub fn mark_current_phase_failed(&self, workflow: &mut OrchestratorWorkflow, error: String) {
        if !matches!(workflow.status, WorkflowStatus::Running) {
            return;
        }

        let current_phase_id = workflow
            .phases
            .get(workflow.current_phase_index)
            .map(|phase| phase.phase_id.clone())
            .unwrap_or_else(|| "unknown".to_string());

        if let Some(phase) = workflow.phases.get_mut(workflow.current_phase_index) {
            phase.status = WorkflowPhaseStatus::Failed;
            phase.completed_at = Some(Utc::now());
            phase.error_message = Some(error.clone());
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine.apply(WorkflowMachineEvent::PhaseFailed);
        machine.apply(WorkflowMachineEvent::GatesFailed);
        machine.apply(WorkflowMachineEvent::PolicyDecisionFailed);
        machine.apply(WorkflowMachineEvent::ReworkBudgetExceeded);
        workflow.machine_state = machine.state();
        workflow.status = WorkflowStatus::Failed;
        workflow.completed_at = Some(Utc::now());
        workflow.failure_reason = Some(error.clone());
        workflow.decision_history.push(self.decision_record(
            current_phase_id,
            WorkflowDecisionAction::Fail,
            None,
            error,
            1.0,
            WorkflowDecisionRisk::High,
        ));
    }

    pub fn mark_merge_conflict(&self, workflow: &mut OrchestratorWorkflow, error: String) {
        if workflow.status != WorkflowStatus::Completed {
            return;
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine.apply(WorkflowMachineEvent::MergeConflictDetected);
        workflow.machine_state = machine.state();
        if workflow.machine_state != WorkflowMachineState::MergeConflict {
            workflow.machine_state = WorkflowMachineState::MergeConflict;
        }
        workflow.failure_reason = Some(error);
        workflow.completed_at = None;
    }

    pub fn resolve_merge_conflict(&self, workflow: &mut OrchestratorWorkflow) {
        if workflow.machine_state != WorkflowMachineState::MergeConflict {
            return;
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine.apply(WorkflowMachineEvent::MergeConflictResolved);
        workflow.machine_state = machine.state();
        if workflow.machine_state != WorkflowMachineState::Completed {
            workflow.machine_state = WorkflowMachineState::Completed;
        }
        workflow.status = WorkflowStatus::Completed;
        workflow.failure_reason = None;
        workflow.completed_at = Some(Utc::now());
    }

    pub fn request_research_phase(
        &self,
        workflow: &mut OrchestratorWorkflow,
        reason: String,
    ) -> bool {
        if !matches!(workflow.status, WorkflowStatus::Running) {
            return false;
        }

        let Some(current_phase_id) = workflow
            .phases
            .get(workflow.current_phase_index)
            .map(|phase| phase.phase_id.clone())
        else {
            return false;
        };

        if current_phase_id == "research" {
            return false;
        }

        if workflow
            .phases
            .get(workflow.current_phase_index)
            .is_some_and(|phase| {
                phase.phase_id == "research"
                    && matches!(
                        phase.status,
                        WorkflowPhaseStatus::Pending
                            | WorkflowPhaseStatus::Ready
                            | WorkflowPhaseStatus::Running
                    )
            })
        {
            return false;
        }

        if let Some(current_phase) = workflow.phases.get_mut(workflow.current_phase_index) {
            if matches!(current_phase.status, WorkflowPhaseStatus::Running) {
                current_phase.status = WorkflowPhaseStatus::Ready;
                current_phase.error_message = Some(reason.clone());
            }
        }

        let research_phase = WorkflowPhaseExecution {
            phase_id: "research".to_string(),
            status: WorkflowPhaseStatus::Running,
            started_at: Some(Utc::now()),
            completed_at: None,
            attempt: 1,
            error_message: None,
        };
        workflow
            .phases
            .insert(workflow.current_phase_index, research_phase);
        workflow.current_phase = Some("research".to_string());
        workflow.machine_state = WorkflowMachineState::RunPhase;
        workflow.status = WorkflowStatus::Running;
        workflow.failure_reason = None;
        workflow.completed_at = None;
        workflow.decision_history.push(self.decision_record(
            current_phase_id,
            WorkflowDecisionAction::Rework,
            Some("research".to_string()),
            reason,
            0.8,
            WorkflowDecisionRisk::Medium,
        ));
        true
    }

    pub fn execute_to_terminal(&self, workflow: &mut OrchestratorWorkflow) {
        while matches!(workflow.status, WorkflowStatus::Running) {
            let Some(phase) = workflow.phases.get(workflow.current_phase_index) else {
                workflow.status = WorkflowStatus::Completed;
                workflow.completed_at = Some(Utc::now());
                workflow.current_phase = None;
                break;
            };

            let fail_phase = std::env::var("AO_FAIL_PHASE").ok();
            if fail_phase.as_deref() == Some(phase.phase_id.as_str()) {
                self.mark_current_phase_failed(
                    workflow,
                    format!("phase {} failed due to AO_FAIL_PHASE", phase.phase_id),
                );
                break;
            }

            self.mark_current_phase_success(workflow);
            if matches!(
                workflow.status,
                WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
            ) {
                break;
            }
        }
    }
}
