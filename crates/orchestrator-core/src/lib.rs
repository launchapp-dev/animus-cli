// phase-decision-test
pub mod agent_runtime_config;
pub mod config;
pub mod daemon_config;
pub mod doctor;
pub mod domain_state;
pub mod events;
pub mod model_quality;
pub mod runtime;
pub mod runtime_contract;
pub mod providers;
pub mod services;
pub mod state_machines;
pub mod types;
pub mod workflow;
pub mod workflow_config;

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
pub use doctor::{
    DoctorCheck, DoctorCheckResult, DoctorCheckStatus, DoctorRemediation, DoctorReport,
};
pub use domain_state::{
    compute_entity_review_status, errors_path, handoffs_path, history_path, load_errors,
    load_handoffs, load_history_store, load_qa_approvals, load_qa_results, load_reviews,
    parse_review_decision, parse_review_entity_type, parse_reviewer_role, project_state_dir,
    qa_approvals_path, qa_results_path, read_json_or_default, reviews_path, save_errors,
    save_handoffs, save_history_store, save_qa_approvals, save_qa_results, save_reviews,
    write_json_atomic, write_json_pretty, EntityReviewStatus, ErrorRecord, ErrorStore, HandoffRecord,
    HandoffStore, HistoryExecutionRecord, HistoryStore, QaGateResultRecord, QaPhaseGateResult,
    QaResultsStore, QaReviewApprovalRecord, QaReviewApprovalStore, ReviewDecision, ReviewEntityType,
    ReviewRecord, ReviewStore, ReviewerRole,
};
pub use events::{OrchestratorEvent, OrchestratorEventKind};
pub use runtime::{EventSink, OrchestratorRuntime, RuntimeHandle};
pub use runtime_contract::{
    build_cli_launch_contract, build_runtime_contract, cli_capabilities_for_tool,
    cli_capabilities_from_config, cli_tool_executable, cli_tool_read_only_flag,
    cli_tool_response_schema_flag, CliCapabilities, CliSessionResumeMode, CliSessionResumePlan,
};
pub use services::{
    evaluate_task_priority_policy, plan_task_priority_rebalance, DaemonServiceApi, FileServiceHub,
    InMemoryServiceHub, load_schedule_state, save_schedule_state, PlanningServiceApi,
    PhaseExecutionRequest, PhaseExecutionResult, PhaseExecutor, PhaseVerdict, ProjectServiceApi,
    ReviewServiceApi, ScheduleRunState, ScheduleState, ServiceHub, TaskServiceApi, WorkflowServiceApi,
};
pub use state_machines::{
    load_state_machines_for_project, state_machines_path, write_state_machines_document,
    LoadedStateMachines, MachineSource, RequirementLifecycleEvent, StateMachineMode,
    StateMachinesDocument,
};
pub use types::{
    AgentHandoffRequestInput, AgentHandoffResult, AgentHandoffStatus, ArchitectureEdge,
    ArchitectureEntity, ArchitectureGraph, Assignee, ChecklistItem, CheckpointReason,
    CodebaseInsight, Complexity, ComplexityAssessment, ComplexityTier, DaemonHealth, DaemonStatus,
    DependencyType, DispatchHistoryEntry, HandoffTargetRole, ImpactArea, LogEntry, LogLevel,
    MAX_DISPATCH_HISTORY_ENTRIES, OrchestratorProject,
    OrchestratorTask, OrchestratorWorkflow, PhaseDecision, PhaseDecisionVerdict, PhaseEvidence,
    PhaseEvidenceKind, Priority, ProjectConcurrencyLimits, ProjectConfig, ProjectCreateInput,
    ProjectMetadata, ProjectModelPreferences, ProjectType, RequirementComment, RequirementItem,
    RequirementLinks, RequirementPriority, RequirementPriorityExt, RequirementRange,
    RequirementStatus, RequirementType,
    RequirementsDraftInput, RequirementsDraftResult, RequirementsExecutionInput,
    RequirementsExecutionResult, RequirementsRefineInput, ResourceRequirements, RiskLevel, Scope,
    TaskCreateInput, TaskDensity, TaskDependency, TaskFilter, TaskMetadata,
    TaskPriorityDistribution, TaskPriorityPolicyReport, TaskPriorityRebalanceChange,
    TaskPriorityRebalanceOptions, TaskPriorityRebalancePlan, TaskStatistics, TaskStatus, TaskType,
    TaskUpdateInput, VisionDocument, VisionDraftInput, WorkflowCheckpoint,
    WorkflowCheckpointMetadata, WorkflowDecisionAction, WorkflowDecisionRecord,
    WorkflowDecisionRisk, WorkflowDecisionSource, WorkflowMachineEvent, WorkflowMachineState,
    WorkflowMetadata, WorkflowPhaseExecution, WorkflowPhaseStatus, WorkflowRunInput, WorkflowSubject,
    WorkflowStatus, DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT,
};
pub use workflow::{
    phase_plan_for_pipeline_id, resolve_phase_plan_for_pipeline, ResumabilityStatus, ResumeConfig,
    WorkflowCheckpointPruneResult, WorkflowLifecycleExecutor, WorkflowResumeManager,
    WorkflowStateMachine, WorkflowStateManager, DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE,
    STANDARD_PIPELINE_ID, UI_UX_PIPELINE_ID,
};
pub use workflow_config::{
    builtin_workflow_config, compile_and_write_yaml_workflows, compile_yaml_workflow_files,
    ensure_workflow_config_compiled, ensure_workflow_config_file, expand_pipeline_phases,
    legacy_workflow_config_paths,
    load_workflow_config, load_workflow_config_or_default, load_workflow_config_with_metadata,
    merge_yaml_into_config, parse_yaml_workflow_config, resolve_pipeline_phase_plan,
    resolve_pipeline_skip_guards, resolve_pipeline_verdict_routing, resolve_pipeline_rework_attempts,
    validate_workflow_and_runtime_configs, validate_workflow_config, workflow_config_hash,
    workflow_config_path, write_workflow_config, yaml_workflows_dir, CompileYamlResult,
    LoadedWorkflowConfig, PhaseTransitionConfig, PhaseUiDefinition, PipelineDefinition,
    PipelinePhaseConfig, PipelinePhaseEntry, PipelineVariable, SubPipelineRef,
    resolve_pipeline_variables, expand_variables, WorkflowCheckpointRetentionConfig,
    WorkflowConfig, WorkflowConfigMetadata, WorkflowConfigSource, WorkflowSchedule,
    WORKFLOW_CONFIG_FILE_NAME,
    WORKFLOW_CONFIG_SCHEMA_ID, WORKFLOW_CONFIG_VERSION, YAML_WORKFLOWS_DIR,
};
pub use model_quality::{
    is_model_suppressed_for_phase, load_model_quality_ledger, model_quality_ledger_path,
    record_model_phase_outcome, ModelQualityLedger, ModelQualityRecord,
    MODEL_QUALITY_LEDGER_FILE_NAME,
};

#[cfg(test)]
mod state_machine_parity;
