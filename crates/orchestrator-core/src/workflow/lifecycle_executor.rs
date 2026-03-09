use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::agent_runtime_config::{PhaseRetryConfig, DEFAULT_MAX_REWORK_ATTEMPTS};
use crate::state_machines::{
    builtin_compiled_state_machines, evaluate_guard, CompiledStateMachines, GuardContext,
};
use crate::types::{
    OrchestratorTask, OrchestratorWorkflow, PhaseDecision, PhaseDecisionVerdict,
    WorkflowDecisionAction, WorkflowDecisionRecord, WorkflowDecisionRisk, WorkflowDecisionSource,
    WorkflowMachineEvent, WorkflowMachineState, WorkflowPhaseExecution, WorkflowPhaseStatus,
    WorkflowRunInput, WorkflowStatus,
};
use crate::workflow_config::PhaseTransitionConfig;

enum GateEvaluationResult {
    Pass,
    Rework {
        reason: String,
        target_phase: Option<String>,
    },
    Fail {
        reason: String,
    },
}

pub(crate) struct TransitionEffect {
    pub next_phase_index: Option<usize>,
    pub phase_status: Option<WorkflowPhaseStatus>,
    pub decision_record: Option<WorkflowDecisionRecord>,
    pub workflow_status: Option<WorkflowStatus>,
    pub machine_state: WorkflowMachineState,
    pub failure_reason: Option<Option<String>>,
    pub completed_at: Option<Option<DateTime<Utc>>>,
    pub current_phase: Option<Option<String>>,
    pub rework_increment: Option<String>,
    pub clear_phase_completed_at: bool,
}

fn apply_transition_effects(effect: &TransitionEffect, workflow: &mut OrchestratorWorkflow) {
    workflow.machine_state = effect.machine_state;

    if let Some(ref failure_reason) = effect.failure_reason {
        workflow.failure_reason = failure_reason.clone();
    }

    if let Some(ref completed_at) = effect.completed_at {
        workflow.completed_at = *completed_at;
    }

    if let Some(ref current_phase) = effect.current_phase {
        workflow.current_phase = current_phase.clone();
    }

    if effect.workflow_status.is_some() {
        workflow.sync_status();
    }

    if let Some(ref rework_phase_id) = effect.rework_increment {
        let count = workflow
            .rework_counts
            .entry(rework_phase_id.clone())
            .or_insert(0);
        *count += 1;
    }

    if let Some(next_idx) = effect.next_phase_index {
        workflow.current_phase_index = next_idx;
        if let Some(phase) = workflow.phases.get_mut(next_idx) {
            if let Some(phase_status) = effect.phase_status {
                phase.status = phase_status;
            }
            if effect.clear_phase_completed_at {
                phase.completed_at = None;
            } else {
                phase.started_at = Some(Utc::now());
            }
            phase.attempt += 1;
            workflow.current_phase = Some(phase.phase_id.clone());
        }
    }

    if let Some(ref record) = effect.decision_record {
        workflow.decision_history.push(record.clone());
    }
}

fn find_phase_index(phases: &[WorkflowPhaseExecution], phase_id: &str) -> Option<usize> {
    phases.iter().position(|p| p.phase_id == phase_id)
}

pub type VerdictRouting = HashMap<String, HashMap<String, PhaseTransitionConfig>>;

use super::phase_plan::{phase_plan_for_workflow_ref, STANDARD_WORKFLOW_REF};
use super::state_machine::WorkflowStateMachine;

pub fn evaluate_skip_guard(guard: &str, task: &OrchestratorTask) -> bool {
    let guard = guard.trim();
    if let Some((lhs, rhs)) = guard.split_once("!=") {
        let field = lhs.trim();
        let value = rhs.trim().trim_matches('\'').trim_matches('"');
        match field {
            "task_type" => task.task_type.as_str() != value,
            "priority" => task.priority.as_str() != value,
            _ => false,
        }
    } else if let Some((lhs, rhs)) = guard.split_once("==") {
        let field = lhs.trim();
        let value = rhs.trim().trim_matches('\'').trim_matches('"');
        match field {
            "task_type" => task.task_type.as_str() == value,
            "priority" => task.priority.as_str() == value,
            _ => false,
        }
    } else {
        false
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowLifecycleExecutor {
    phase_plan: Vec<String>,
    state_machines: CompiledStateMachines,
    retry_configs: HashMap<String, PhaseRetryConfig>,
    verdict_routing: VerdictRouting,
    skip_guards: HashMap<String, Vec<String>>,
}

impl Default for WorkflowLifecycleExecutor {
    fn default() -> Self {
        Self {
            phase_plan: phase_plan_for_workflow_ref(Some(STANDARD_WORKFLOW_REF)),
            state_machines: builtin_compiled_state_machines(),
            retry_configs: HashMap::new(),
            verdict_routing: HashMap::new(),
            skip_guards: HashMap::new(),
        }
    }
}

impl WorkflowLifecycleExecutor {
    pub fn new(phase_plan: Vec<String>) -> Self {
        Self::with_state_machines(phase_plan, builtin_compiled_state_machines())
    }

    pub fn with_verdict_routing(phase_plan: Vec<String>, verdict_routing: VerdictRouting) -> Self {
        Self {
            phase_plan,
            state_machines: builtin_compiled_state_machines(),
            retry_configs: HashMap::new(),
            verdict_routing,
            skip_guards: HashMap::new(),
        }
    }

    pub fn with_state_machines(
        phase_plan: Vec<String>,
        state_machines: CompiledStateMachines,
    ) -> Self {
        Self {
            phase_plan,
            state_machines,
            retry_configs: HashMap::new(),
            verdict_routing: HashMap::new(),
            skip_guards: HashMap::new(),
        }
    }

    pub fn with_retry_configs(mut self, configs: HashMap<String, PhaseRetryConfig>) -> Self {
        self.retry_configs = configs;
        self
    }

    pub fn with_skip_guards(mut self, skip_guards: HashMap<String, Vec<String>>) -> Self {
        self.skip_guards = skip_guards;
        self
    }

    fn max_reworks_for_phase(&self, phase_id: &str) -> u32 {
        self.retry_configs
            .get(phase_id)
            .map(|cfg| cfg.max_attempts)
            .unwrap_or(DEFAULT_MAX_REWORK_ATTEMPTS)
    }

    pub fn backoff_delay_for_phase(&self, phase_id: &str, attempt: u32) -> u64 {
        self.retry_configs
            .get(phase_id)
            .and_then(|cfg| cfg.backoff.as_ref())
            .map(|backoff| backoff.delay_for_attempt(attempt))
            .unwrap_or(0)
    }

    fn guard_context_for_phase<'a>(
        &self,
        phase_id: &'a str,
        rework_counts: &'a HashMap<String, u32>,
    ) -> GuardContext<'a> {
        GuardContext {
            phase_id,
            rework_counts,
            max_reworks_for_phase: self.max_reworks_for_phase(phase_id),
        }
    }

    fn is_rework_budget_available(
        &self,
        phase_id: &str,
        rework_counts: &HashMap<String, u32>,
    ) -> bool {
        let context = self.guard_context_for_phase(phase_id, rework_counts);
        evaluate_guard("rework_budget_available", &context)
    }

    fn resolve_verdict_target(&self, current_phase_id: &str, verdict: &str) -> Option<String> {
        self.verdict_routing
            .get(current_phase_id)
            .and_then(|verdicts| verdicts.get(verdict))
            .map(|config| config.target.clone())
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

    pub fn skip_guarded_phases(
        &self,
        workflow: &mut OrchestratorWorkflow,
        task: &OrchestratorTask,
    ) {
        if !matches!(workflow.status, WorkflowStatus::Running) {
            return;
        }

        loop {
            let phase = match workflow.phases.get(workflow.current_phase_index) {
                Some(p) => p,
                None => break,
            };

            if !matches!(
                phase.status,
                WorkflowPhaseStatus::Running
                    | WorkflowPhaseStatus::Pending
                    | WorkflowPhaseStatus::Ready
            ) {
                break;
            }

            let guards = match self.skip_guards.get(&phase.phase_id) {
                Some(g) if !g.is_empty() => g,
                _ => break,
            };

            let matched_guard = guards.iter().find(|guard| evaluate_skip_guard(guard, task));

            let matched_guard = match matched_guard {
                Some(g) => g.clone(),
                None => break,
            };

            let phase_id = phase.phase_id.clone();
            let now = Utc::now();

            if let Some(phase) = workflow.phases.get_mut(workflow.current_phase_index) {
                phase.status = WorkflowPhaseStatus::Skipped;
                phase.completed_at = Some(now);
            }

            let mut machine = self.state_machine(workflow.machine_state);
            machine
                .apply(WorkflowMachineEvent::PhaseSkipped)
                .expect("skip: PhaseSkipped transition");

            let next_phase = workflow
                .phases
                .get(workflow.current_phase_index + 1)
                .map(|p| p.phase_id.clone());

            workflow.decision_history.push(self.decision_record(
                phase_id,
                WorkflowDecisionAction::Skip,
                next_phase.clone(),
                format!("skip_if guard matched: {}", matched_guard),
                1.0,
                WorkflowDecisionRisk::Low,
            ));

            let next_idx = workflow.current_phase_index + 1;
            if next_idx < workflow.phases.len() {
                machine
                    .apply(WorkflowMachineEvent::PhaseStarted)
                    .expect("skip: PhaseStarted after skip");
                workflow.current_phase_index = next_idx;
                if let Some(next) = workflow.phases.get_mut(next_idx) {
                    next.status = WorkflowPhaseStatus::Running;
                    next.started_at = Some(now);
                    next.attempt += 1;
                    workflow.current_phase = Some(next.phase_id.clone());
                }
            } else {
                machine
                    .apply(WorkflowMachineEvent::NoMorePhases)
                    .expect("skip: NoMorePhases after skip");
                workflow.completed_at = Some(now);
                workflow.current_phase = None;
                workflow.machine_state = machine.state();
                workflow.sync_status();
                break;
            }
            workflow.machine_state = machine.state();
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
        machine
            .apply(WorkflowMachineEvent::Start)
            .expect("bootstrap: Idle -> Start");
        machine
            .apply(WorkflowMachineEvent::PhaseStarted)
            .expect("bootstrap: EvaluateTransition -> PhaseStarted");

        if let Some(first) = phases.first_mut() {
            first.status = WorkflowPhaseStatus::Running;
            first.started_at = Some(now);
            first.attempt = 1;
        }

        let ms = machine.state();
        OrchestratorWorkflow {
            id: workflow_id,
            subject: input.subject.clone(),
            task_id: input.task_id,
            workflow_ref: input.workflow_ref,
            input: input.input,
            status: ms.to_workflow_status(),
            current_phase_index: 0,
            phases,
            machine_state: ms,
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
            WorkflowStatus::Completed
                | WorkflowStatus::Failed
                | WorkflowStatus::Escalated
                | WorkflowStatus::Cancelled
        ) {
            return;
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine
            .apply(WorkflowMachineEvent::PauseRequested)
            .expect("pause: PauseRequested transition");
        workflow.machine_state = machine.state();
        workflow.sync_status();
    }

    pub fn resume(&self, workflow: &mut OrchestratorWorkflow) {
        if matches!(
            workflow.status,
            WorkflowStatus::Completed | WorkflowStatus::Cancelled
        ) {
            return;
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine
            .apply(WorkflowMachineEvent::ResumeRequested)
            .expect("resume: ResumeRequested transition");
        machine
            .apply(WorkflowMachineEvent::PhaseStarted)
            .expect("resume: PhaseStarted after resume");
        workflow.machine_state = machine.state();
        workflow.sync_status();
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
        machine
            .apply(WorkflowMachineEvent::CancelRequested)
            .expect("cancel: CancelRequested transition");
        workflow.machine_state = machine.state();
        workflow.sync_status();
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
        machine
            .apply(WorkflowMachineEvent::PhaseSucceeded)
            .expect("success: PhaseSucceeded transition");

        let gate_result = self.evaluate_gates(&decision, workflow);
        let effect = self.resolve_success_transition(
            &gate_result,
            &decision,
            &current_phase_id,
            workflow,
            &mut machine,
        );

        apply_transition_effects(&effect, workflow);
    }

    fn resolve_success_transition(
        &self,
        gate_result: &GateEvaluationResult,
        decision: &Option<PhaseDecision>,
        current_phase_id: &str,
        workflow: &OrchestratorWorkflow,
        machine: &mut WorkflowStateMachine,
    ) -> TransitionEffect {
        if matches!(
            decision.as_ref().map(|value| value.verdict),
            Some(PhaseDecisionVerdict::Skip)
        ) && matches!(gate_result, GateEvaluationResult::Pass)
        {
            return self.build_skip_close_effect(decision, current_phase_id, machine);
        }

        match gate_result {
            GateEvaluationResult::Pass => {
                self.build_advance_effect(decision, current_phase_id, workflow, machine)
            }
            GateEvaluationResult::Rework {
                reason,
                target_phase,
            } => self.build_rework_effect(
                decision,
                current_phase_id,
                reason,
                target_phase,
                workflow,
                machine,
            ),
            GateEvaluationResult::Fail { reason } => {
                self.build_gate_fail_effect(decision, current_phase_id, reason, machine)
            }
        }
    }

    fn build_skip_close_effect(
        &self,
        decision: &Option<PhaseDecision>,
        current_phase_id: &str,
        machine: &mut WorkflowStateMachine,
    ) -> TransitionEffect {
        machine
            .apply(WorkflowMachineEvent::PolicyDecisionReady)
            .expect("skip: PolicyDecisionReady transition");

        let reason = decision
            .as_ref()
            .map(|value| value.reason.trim())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "workflow closed by skip verdict".to_string());
        let skip_as_completed = reason.to_ascii_lowercase().contains("already_done");
        let (machine_version, machine_hash, machine_source) = self.machine_metadata();
        let record = WorkflowDecisionRecord {
            timestamp: Utc::now(),
            phase_id: current_phase_id.to_string(),
            decision: WorkflowDecisionAction::Skip,
            target_phase: None,
            reason: reason.clone(),
            confidence: decision
                .as_ref()
                .map(|value| value.confidence)
                .unwrap_or(1.0),
            risk: decision
                .as_ref()
                .map(|value| value.risk)
                .unwrap_or(WorkflowDecisionRisk::Low),
            source: decision
                .as_ref()
                .map(|_| WorkflowDecisionSource::Llm)
                .unwrap_or(WorkflowDecisionSource::Fallback),
            guardrail_violations: decision
                .as_ref()
                .map(|value| value.guardrail_violations.clone())
                .unwrap_or_default(),
            machine_version,
            machine_hash,
            machine_source,
        };

        if skip_as_completed {
            machine
                .apply(WorkflowMachineEvent::NoMorePhases)
                .expect("skip: NoMorePhases completion");
        } else {
            machine
                .apply(WorkflowMachineEvent::CancelRequested)
                .expect("skip: CancelRequested transition");
        }
        let final_state = machine.state();

        TransitionEffect {
            next_phase_index: None,
            phase_status: None,
            decision_record: Some(record),
            workflow_status: Some(final_state.to_workflow_status()),
            machine_state: final_state,
            failure_reason: None,
            completed_at: Some(Some(Utc::now())),
            current_phase: Some(None),
            rework_increment: None,
            clear_phase_completed_at: false,
        }
    }

    fn build_advance_effect(
        &self,
        decision: &Option<PhaseDecision>,
        current_phase_id: &str,
        workflow: &OrchestratorWorkflow,
        machine: &mut WorkflowStateMachine,
    ) -> TransitionEffect {
        let (confidence, risk, source) = match decision {
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

        let config_target = self.resolve_verdict_target(current_phase_id, "advance");
        let decision_target = decision.as_ref().and_then(|d| d.target_phase.clone());
        let target_phase_id = config_target.or(decision_target);
        if target_phase_id.is_some() {
            machine
                .apply(WorkflowMachineEvent::PhaseTargetSelected)
                .expect("advance: PhaseTargetSelected transition");
        } else {
            machine
                .apply(WorkflowMachineEvent::GatesPassed)
                .expect("advance: GatesPassed transition");
            let _ = machine.apply(WorkflowMachineEvent::PolicyDecisionReady);
        }
        let next_idx = match &target_phase_id {
            Some(id) => find_phase_index(&workflow.phases, id),
            None => {
                let idx = workflow.current_phase_index + 1;
                if idx < workflow.phases.len() {
                    Some(idx)
                } else {
                    None
                }
            }
        };
        if let Some(next_idx) = next_idx {
            let next_phase_id = workflow.phases[next_idx].phase_id.clone();
            let record = WorkflowDecisionRecord {
                timestamp: Utc::now(),
                phase_id: current_phase_id.to_string(),
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
            };
            machine
                .apply(WorkflowMachineEvent::Start)
                .expect("advance: Start next phase cycle");
            machine
                .apply(WorkflowMachineEvent::PhaseStarted)
                .expect("advance: PhaseStarted next phase");
            TransitionEffect {
                next_phase_index: Some(next_idx),
                phase_status: Some(WorkflowPhaseStatus::Running),
                decision_record: Some(record),
                workflow_status: Some(WorkflowStatus::Running),
                machine_state: machine.state(),
                failure_reason: None,
                completed_at: None,
                current_phase: None,
                rework_increment: None,
                clear_phase_completed_at: false,
            }
        } else {
            let record = WorkflowDecisionRecord {
                timestamp: Utc::now(),
                phase_id: current_phase_id.to_string(),
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
            };
            machine
                .apply(WorkflowMachineEvent::NoMorePhases)
                .expect("advance: NoMorePhases completion");
            TransitionEffect {
                next_phase_index: None,
                phase_status: None,
                decision_record: Some(record),
                workflow_status: Some(WorkflowStatus::Completed),
                machine_state: machine.state(),
                failure_reason: None,
                completed_at: Some(Some(Utc::now())),
                current_phase: Some(None),
                rework_increment: None,
                clear_phase_completed_at: false,
            }
        }
    }

    fn build_rework_effect(
        &self,
        decision: &Option<PhaseDecision>,
        current_phase_id: &str,
        reason: &str,
        target_phase: &Option<String>,
        workflow: &OrchestratorWorkflow,
        machine: &mut WorkflowStateMachine,
    ) -> TransitionEffect {
        machine
            .apply(WorkflowMachineEvent::GatesFailed)
            .expect("rework: GatesFailed transition");
        let intermediate_machine_state = machine.state();

        let config_rework_target = self.resolve_verdict_target(current_phase_id, "rework");
        let effective_target = config_rework_target.or(target_phase.clone());
        let rework_target_idx = match &effective_target {
            Some(id) => find_phase_index(&workflow.phases, id),
            None => Some(workflow.current_phase_index),
        };
        let rework_idx = rework_target_idx.unwrap_or(workflow.current_phase_index);

        let rework_phase_id = workflow
            .phases
            .get(rework_idx)
            .map(|p| p.phase_id.clone())
            .unwrap_or_else(|| current_phase_id.to_string());

        let confidence = decision.as_ref().map(|d| d.confidence).unwrap_or(0.5);
        let risk = decision
            .as_ref()
            .map(|d| d.risk)
            .unwrap_or(WorkflowDecisionRisk::Medium);
        let (machine_version, machine_hash, machine_source) = self.machine_metadata();
        let record = WorkflowDecisionRecord {
            timestamp: Utc::now(),
            phase_id: current_phase_id.to_string(),
            decision: WorkflowDecisionAction::Rework,
            target_phase: effective_target.or(Some(rework_phase_id.clone())),
            reason: reason.to_string(),
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
        };

        let mut retry_machine = WorkflowStateMachine::with_definition(
            intermediate_machine_state,
            self.state_machines.workflow.clone(),
        );
        retry_machine
            .apply(WorkflowMachineEvent::RetryPhaseStarted)
            .expect("rework: RetryPhaseStarted transition");

        TransitionEffect {
            next_phase_index: Some(rework_idx),
            phase_status: Some(WorkflowPhaseStatus::Running),
            decision_record: Some(record),
            workflow_status: Some(WorkflowStatus::Running),
            machine_state: retry_machine.state(),
            failure_reason: None,
            completed_at: None,
            current_phase: None,
            rework_increment: Some(rework_phase_id),
            clear_phase_completed_at: true,
        }
    }

    fn build_gate_fail_effect(
        &self,
        decision: &Option<PhaseDecision>,
        current_phase_id: &str,
        reason: &str,
        machine: &mut WorkflowStateMachine,
    ) -> TransitionEffect {
        machine
            .apply(WorkflowMachineEvent::GatesFailed)
            .expect("gate_fail: GatesFailed transition");
        let _ = machine.apply(WorkflowMachineEvent::PolicyDecisionFailed);
        let _ = machine.apply(WorkflowMachineEvent::ReworkBudgetExceeded);
        let final_state = machine.state();
        let is_escalated = final_state == WorkflowMachineState::HumanEscalated;
        let workflow_status = if is_escalated {
            WorkflowStatus::Escalated
        } else {
            WorkflowStatus::Failed
        };

        let confidence = decision.as_ref().map(|d| d.confidence).unwrap_or(0.5);
        let risk = decision
            .as_ref()
            .map(|d| d.risk)
            .unwrap_or(WorkflowDecisionRisk::High);
        let (machine_version, machine_hash, machine_source) = self.machine_metadata();
        let record = WorkflowDecisionRecord {
            timestamp: Utc::now(),
            phase_id: current_phase_id.to_string(),
            decision: WorkflowDecisionAction::Fail,
            target_phase: None,
            reason: reason.to_string(),
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
        };

        TransitionEffect {
            next_phase_index: None,
            phase_status: None,
            decision_record: Some(record),
            workflow_status: Some(workflow_status),
            machine_state: final_state,
            failure_reason: Some(Some(reason.to_string())),
            completed_at: Some(Some(Utc::now())),
            current_phase: None,
            rework_increment: None,
            clear_phase_completed_at: false,
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
                let rework_target = decision.target_phase.as_deref().unwrap_or(phase_id);
                if !self.is_rework_budget_available(rework_target, &workflow.rework_counts) {
                    let rework_count = workflow
                        .rework_counts
                        .get(rework_target)
                        .copied()
                        .unwrap_or(0);
                    let max_reworks = self.max_reworks_for_phase(rework_target);
                    return GateEvaluationResult::Fail {
                        reason: format!(
                            "rework budget exceeded for phase {} ({} reworks, max {}): {}",
                            rework_target,
                            rework_count,
                            max_reworks,
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
                    target_phase: decision.target_phase.clone(),
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
                    if self.is_rework_budget_available(phase_id, &workflow.rework_counts) {
                        return GateEvaluationResult::Rework {
                            reason: format!(
                                "low confidence ({:.2}) with high risk — requesting rework",
                                decision.confidence
                            ),
                            target_phase: None,
                        };
                    }
                }
                if !decision.guardrail_violations.is_empty() {
                    let phase_id = workflow
                        .phases
                        .get(workflow.current_phase_index)
                        .map(|p| p.phase_id.as_str())
                        .unwrap_or("unknown");
                    if self.is_rework_budget_available(phase_id, &workflow.rework_counts) {
                        return GateEvaluationResult::Rework {
                            reason: format!(
                                "guardrail violations: {}",
                                decision.guardrail_violations.join("; ")
                            ),
                            target_phase: None,
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

        let effect = self.resolve_failure_transition(&current_phase_id, &error, workflow);
        apply_transition_effects(&effect, workflow);
    }

    pub fn mark_completed_failed(&self, workflow: &mut OrchestratorWorkflow, error: String) {
        if workflow.status != WorkflowStatus::Completed {
            return;
        }

        let phase_id = workflow
            .phases
            .last()
            .map(|phase| phase.phase_id.clone())
            .unwrap_or_else(|| "post-success".to_string());

        workflow.machine_state = WorkflowMachineState::Failed;
        workflow.sync_status();
        workflow.failure_reason = Some(error.clone());
        workflow.completed_at = Some(Utc::now());
        workflow.decision_history.push(self.decision_record(
            phase_id,
            WorkflowDecisionAction::Fail,
            None,
            error,
            1.0,
            WorkflowDecisionRisk::High,
        ));
    }

    fn resolve_failure_transition(
        &self,
        current_phase_id: &str,
        error: &str,
        workflow: &OrchestratorWorkflow,
    ) -> TransitionEffect {
        let mut machine = self.state_machine(workflow.machine_state);
        machine
            .apply(WorkflowMachineEvent::PhaseFailed)
            .expect("failure: PhaseFailed transition");
        machine
            .apply(WorkflowMachineEvent::GatesFailed)
            .expect("failure: GatesFailed transition");
        let _ = machine.apply(WorkflowMachineEvent::PolicyDecisionFailed);
        let workflow_status = WorkflowStatus::Failed;
        let final_state = WorkflowMachineState::Failed;

        let record = self.decision_record(
            current_phase_id.to_string(),
            WorkflowDecisionAction::Fail,
            None,
            error.to_string(),
            1.0,
            WorkflowDecisionRisk::High,
        );

        TransitionEffect {
            next_phase_index: None,
            phase_status: None,
            decision_record: Some(record),
            workflow_status: Some(workflow_status),
            machine_state: final_state,
            failure_reason: Some(Some(error.to_string())),
            completed_at: Some(Some(Utc::now())),
            current_phase: None,
            rework_increment: None,
            clear_phase_completed_at: false,
        }
    }

    pub fn mark_merge_conflict(&self, workflow: &mut OrchestratorWorkflow, error: String) {
        if workflow.status != WorkflowStatus::Completed {
            return;
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine
            .apply(WorkflowMachineEvent::MergeConflictDetected)
            .expect("merge_conflict: MergeConflictDetected transition");
        workflow.machine_state = machine.state();
        if workflow.machine_state != WorkflowMachineState::MergeConflict {
            workflow.machine_state = WorkflowMachineState::MergeConflict;
        }
        workflow.sync_status();
        workflow.failure_reason = Some(error);
        workflow.completed_at = None;
    }

    pub fn resolve_merge_conflict(&self, workflow: &mut OrchestratorWorkflow) {
        if workflow.machine_state != WorkflowMachineState::MergeConflict {
            return;
        }

        let mut machine = self.state_machine(workflow.machine_state);
        machine
            .apply(WorkflowMachineEvent::MergeConflictResolved)
            .expect("merge_conflict: MergeConflictResolved transition");
        workflow.machine_state = machine.state();
        if workflow.machine_state != WorkflowMachineState::Completed {
            workflow.machine_state = WorkflowMachineState::Completed;
        }
        workflow.sync_status();
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
        workflow.sync_status();
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
                workflow.machine_state = WorkflowMachineState::Completed;
                workflow.sync_status();
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
                WorkflowStatus::Completed
                    | WorkflowStatus::Failed
                    | WorkflowStatus::Escalated
                    | WorkflowStatus::Cancelled
            ) {
                break;
            }
        }
    }
}
