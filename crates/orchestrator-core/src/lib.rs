// phase-decision-test
pub mod agent_runtime_config;
pub mod config;
pub mod daemon_config;
pub mod daemon_tick_metrics;
pub mod doctor;
pub mod domain_state;
pub mod events;
pub mod execution_projection;
pub mod model_quality;
pub mod providers;
pub mod runtime;
pub mod runtime_contract;
pub mod services;
pub mod state_machines;
pub mod task_dispatch_policy;
pub mod task_gate;
pub mod types;
pub mod workflow;
pub mod workflow_config;
pub mod workflow_runner_registry;

pub use agent_runtime_config::{
    agent_runtime_config_path, builtin_agent_runtime_config, ensure_agent_runtime_config_file,
    load_agent_runtime_config, load_agent_runtime_config_or_default, write_agent_runtime_config,
    AgentProfile, AgentRuntimeConfig, AgentRuntimeMetadata, AgentRuntimeOverrides,
    AgentRuntimeSource, BackoffConfig, CliToolConfig, CommandCwdMode, LoadedAgentRuntimeConfig,
    PhaseCommandDefinition, PhaseDecisionContract, PhaseExecutionDefinition, PhaseExecutionMode,
    PhaseManualDefinition, PhaseOutputContract, PhaseRetryConfig, DEFAULT_MAX_REWORK_ATTEMPTS,
};
pub use config::RuntimeConfig;
pub use daemon_config::{
    daemon_project_config_path, load_daemon_project_config, update_daemon_project_config,
    write_daemon_project_config, DaemonProjectConfig, DaemonProjectConfigPatch,
    DAEMON_PROJECT_CONFIG_FILE_NAME,
};
pub use daemon_tick_metrics::DaemonTickMetrics;
pub use doctor::{
    DoctorCheck, DoctorCheckResult, DoctorCheckStatus, DoctorRemediation, DoctorReport,
};
pub use domain_state::{
    compute_entity_review_status, errors_path, handoffs_path, history_path, load_errors,
    load_handoffs, load_history_store, load_qa_approvals, load_qa_results, load_reviews,
    parse_review_decision, parse_review_entity_type, parse_reviewer_role, project_state_dir,
    qa_approvals_path, qa_results_path, read_json_or_default, reviews_path, save_errors,
    save_handoffs, save_history_store, save_qa_approvals, save_qa_results, save_reviews,
    write_json_atomic, write_json_pretty, EntityReviewStatus, ErrorRecord, ErrorStore,
    HandoffRecord, HandoffStore, HistoryExecutionRecord, HistoryStore, QaGateResultRecord,
    QaPhaseGateResult, QaResultsStore, QaReviewApprovalRecord, QaReviewApprovalStore,
    ReviewDecision, ReviewEntityType, ReviewRecord, ReviewStore, ReviewerRole,
};
pub use events::{OrchestratorEvent, OrchestratorEventKind};
pub use execution_projection::{
    project_requirement_workflow_status, project_schedule_completion_status,
    project_schedule_dispatch_attempt, project_schedule_execution_fact,
    project_task_blocked_with_reason, project_task_dispatch_failure, project_task_execution_fact,
    project_task_status, project_task_terminal_workflow_status, project_task_workflow_start,
};
pub use model_quality::{
    is_model_suppressed_for_phase, load_model_quality_ledger, model_quality_ledger_path,
    record_model_phase_outcome, ModelQualityLedger, ModelQualityRecord,
    MODEL_QUALITY_LEDGER_FILE_NAME,
};
pub use runtime::{EventSink, OrchestratorRuntime, RuntimeHandle};
pub use runtime_contract::{
    build_cli_launch_contract, build_runtime_contract, cli_capabilities_for_tool,
    cli_capabilities_from_config, cli_tool_executable, cli_tool_read_only_flag,
    cli_tool_response_schema_flag, CliCapabilities, CliSessionResumeMode, CliSessionResumePlan,
};
pub use services::{
    evaluate_task_priority_policy, load_schedule_state, plan_task_priority_rebalance,
    save_schedule_state, DaemonServiceApi, FileServiceHub, InMemoryServiceHub,
    PhaseExecutionRequest, PhaseExecutionResult, PhaseExecutor, PhaseVerdict, PlanningServiceApi,
    ProjectServiceApi, ReviewServiceApi, ScheduleRunState, ScheduleState, ServiceHub,
    TaskServiceApi, WorkflowServiceApi,
};
pub use state_machines::{
    load_state_machines_for_project, state_machines_path, write_state_machines_document,
    LoadedStateMachines, MachineSource, RequirementLifecycleEvent, StateMachineMode,
    StateMachinesDocument,
};
pub use task_dispatch_policy::{
    routing_complexity_for_task, should_skip_task_dispatch, workflow_ref_for_task,
};
pub use task_gate::{
    dependency_blocked_reason, dependency_gate_issues_for_task, is_dependency_gate_block,
    is_merge_gate_block, merge_blocked_reason, promote_backlog_tasks_to_ready,
    retry_failed_task_workflows, DEPENDENCY_GATE_PREFIX, MERGE_GATE_PREFIX,
};
pub use types::{
    AgentHandoffRequestInput, AgentHandoffResult, AgentHandoffStatus, ArchitectureEdge,
    ArchitectureEntity, ArchitectureGraph, Assignee, ChecklistItem, CheckpointReason,
    CodebaseInsight, Complexity, ComplexityAssessment, ComplexityTier, DaemonHealth, DaemonStatus,
    DependencyType, DispatchHistoryEntry, HandoffTargetRole, ImpactArea, LogEntry, LogLevel,
    OrchestratorProject, OrchestratorTask, OrchestratorWorkflow, PhaseDecision,
    PhaseDecisionVerdict, PhaseEvidence, PhaseEvidenceKind, Priority, ProjectConcurrencyLimits,
    ProjectConfig, ProjectCreateInput, ProjectMetadata, ProjectModelPreferences, ProjectType,
    RequirementComment, RequirementItem, RequirementLinks, RequirementPriority,
    RequirementPriorityExt, RequirementRange, RequirementStatus, RequirementType,
    RequirementsDraftInput, RequirementsDraftResult, RequirementsExecutionInput,
    RequirementsExecutionResult, RequirementsRefineInput, ResourceRequirements, RiskLevel, Scope,
    SubjectDispatch, TaskCreateInput, TaskDensity, TaskDependency, TaskFilter, TaskMetadata,
    TaskPriorityDistribution, TaskPriorityPolicyReport, TaskPriorityRebalanceChange,
    TaskPriorityRebalanceOptions, TaskPriorityRebalancePlan, TaskStatistics, TaskStatus, TaskType,
    TaskUpdateInput, VisionDocument, VisionDraftInput, WorkflowCheckpoint,
    WorkflowCheckpointMetadata, WorkflowDecisionAction, WorkflowDecisionRecord,
    WorkflowDecisionRisk, WorkflowDecisionSource, WorkflowMachineEvent, WorkflowMachineState,
    WorkflowMetadata, WorkflowPhaseExecution, WorkflowPhaseStatus, WorkflowRunInput,
    WorkflowStatus, WorkflowSubject, DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT,
    MAX_DISPATCH_HISTORY_ENTRIES,
};
pub use workflow::{
    phase_plan_for_workflow_ref, resolve_phase_plan_for_workflow_ref, ResumabilityStatus,
    ResumeConfig, WorkflowCheckpointPruneResult, WorkflowLifecycleExecutor, WorkflowResumeManager,
    WorkflowStateMachine, WorkflowStateManager, DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE,
    REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF,
    STANDARD_WORKFLOW_REF, UI_UX_WORKFLOW_REF,
};
pub use workflow_config::{
    builtin_workflow_config, compile_and_write_yaml_workflows, compile_yaml_workflow_files,
    ensure_workflow_config_compiled, ensure_workflow_config_file, expand_variables,
    expand_workflow_phases, legacy_workflow_config_paths, load_workflow_config,
    load_workflow_config_or_default, load_workflow_config_with_metadata, merge_yaml_into_config,
    parse_yaml_workflow_config, resolve_workflow_phase_plan, resolve_workflow_rework_attempts,
    resolve_workflow_skip_guards, resolve_workflow_variables, resolve_workflow_verdict_routing,
    validate_workflow_and_runtime_configs, validate_workflow_config, workflow_config_hash,
    workflow_config_path, write_workflow_config, yaml_workflows_dir, CompileYamlResult,
    LoadedWorkflowConfig, PhaseTransitionConfig, PhaseUiDefinition, SubWorkflowRef,
    WorkflowCheckpointRetentionConfig, WorkflowConfig, WorkflowConfigMetadata,
    WorkflowConfigSource, WorkflowDefinition, WorkflowPhaseConfig, WorkflowPhaseEntry,
    WorkflowSchedule, WorkflowVariable, WORKFLOW_CONFIG_FILE_NAME, WORKFLOW_CONFIG_SCHEMA_ID,
    WORKFLOW_CONFIG_VERSION, YAML_WORKFLOWS_DIR,
};
pub use workflow_runner_registry::{
    active_workflow_runner_ids, register_workflow_runner_pid, unregister_workflow_runner_pid,
};

#[cfg(test)]
mod state_machine_parity;
