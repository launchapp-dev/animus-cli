// phase-decision-test
pub mod agent_runtime_config;
pub mod config;
pub mod daemon_config;
pub mod daemon_tick_metrics;
pub mod doctor;
pub mod domain_state;
pub mod execution_projection;
pub mod model_quality;
pub mod providers;
pub mod runtime_contract;
pub mod services;
pub mod state_machines;
pub mod task_dispatch_policy;
pub mod types;
pub mod workflow;
pub mod workflow_config;
pub mod workflow_events;
pub mod workflow_runner_registry;

pub use agent_runtime_config::{
    agent_runtime_config_path, builtin_agent_runtime_config, ensure_agent_runtime_config_file,
    load_agent_runtime_config, load_agent_runtime_config_or_default, write_agent_runtime_config, AgentProfile,
    AgentRuntimeConfig, AgentRuntimeMetadata, AgentRuntimeOverrides, AgentRuntimeSource, BackoffConfig, CliToolConfig,
    CommandCwdMode, LoadedAgentRuntimeConfig, PhaseCommandDefinition, PhaseDecisionContract, PhaseExecutionDefinition,
    PhaseExecutionMode, PhaseManualDefinition, PhaseOutputContract, PhaseRetryConfig, DEFAULT_MAX_REWORK_ATTEMPTS,
};
pub use config::RuntimeConfig;
pub use daemon_config::{
    daemon_project_config_path, load_daemon_project_config, update_daemon_project_config, write_daemon_project_config,
    DaemonProjectConfig, DaemonProjectConfigPatch, DAEMON_PROJECT_CONFIG_FILE_NAME,
};
pub use daemon_tick_metrics::DaemonTickMetrics;
pub use doctor::{DoctorCheck, DoctorCheckResult, DoctorCheckStatus, DoctorRemediation, DoctorReport};
pub use domain_state::{
    compute_entity_review_status, errors_path, handoffs_path, history_path, load_errors, load_handoffs,
    load_history_store, load_qa_approvals, load_qa_results, load_reviews, parse_review_decision,
    parse_review_entity_type, parse_reviewer_role, project_state_dir, qa_approvals_path, qa_results_path,
    read_json_or_default, reviews_path, save_errors, save_handoffs, save_history_store, save_qa_approvals,
    save_qa_results, save_reviews, write_json_atomic, write_json_pretty, EntityReviewStatus, ErrorRecord, ErrorStore,
    HandoffRecord, HandoffStore, HistoryExecutionRecord, HistoryStore, QaGateResultRecord, QaPhaseGateResult,
    QaResultsStore, QaReviewApprovalRecord, QaReviewApprovalStore, ReviewDecision, ReviewEntityType, ReviewRecord,
    ReviewStore, ReviewerRole,
};
pub use execution_projection::{
    builtin_execution_projector_registry, execution_fact_subject_kind, project_execution_fact,
    project_requirement_workflow_status, project_schedule_dispatch_attempt, project_schedule_execution_fact,
    project_task_blocked_with_reason, project_task_execution_fact, project_task_status,
    project_task_terminal_workflow_status, project_task_workflow_start, ExecutionProjector, ExecutionProjectorRegistry,
    WORKFLOW_RUNNER_BLOCKED_PREFIX,
};
pub use model_quality::{
    is_model_suppressed_for_phase, load_model_quality_ledger, model_quality_ledger_path, record_model_phase_outcome,
    ModelQualityLedger, ModelQualityRecord, MODEL_QUALITY_LEDGER_FILE_NAME,
};
pub use orchestrator_config::{
    activate_pack_mcp_overlay, apply_pack_mcp_overlay, check_pack_runtime_requirements,
    ensure_pack_runtime_requirements, load_pack_agent_runtime_overlay, load_pack_manifest,
    load_pack_manifest_from_file, load_pack_mcp_overlay, load_pack_workflow_overlay, machine_installed_packs_dir,
    pack_manifest_path, parse_pack_manifest, project_pack_overrides_dir, resolve_pack_registry, validate_pack_manifest,
    validate_pack_manifest_assets, ExternalRuntimeKind, LoadedPackManifest, PackCompatibility, PackDependency,
    PackKind, PackManifest, PackMcp, PackMcpOverlay, PackNativeModule, PackOwnership, PackOwnershipMode,
    PackPermissions, PackRegistrySource, PackRuntime, PackRuntimeCheck, PackRuntimeCheckStatus, PackRuntimeReport,
    PackRuntimeRequirement, PackSchedules, PackSecrets, PackSubjects, PackWorkflows, ResolvedPackRegistry,
    ResolvedPackRegistryEntry, MACHINE_PACKS_DIR_NAME, PACK_MANIFEST_FILE_NAME, PACK_MANIFEST_SCHEMA_ID,
    PROJECT_PACKS_DIR_NAME,
};
pub use runtime_contract::{
    build_cli_launch_contract, build_runtime_contract, cli_capabilities_for_tool, cli_capabilities_from_config,
    cli_tool_executable, cli_tool_read_only_flag, cli_tool_response_schema_flag, CliCapabilities, CliSessionResumeMode,
    CliSessionResumePlan,
};
pub use services::{
    evaluate_task_priority_policy, load_schedule_state, plan_task_priority_rebalance, save_schedule_state,
    stop_agent_runner_process, DaemonServiceApi, FileServiceHub, InMemoryServiceHub, PhaseExecutionRequest,
    PhaseExecutionResult, PhaseExecutor, PhaseVerdict, PlanningServiceApi, ProjectServiceApi, ReviewServiceApi,
    ScheduleRunState, ScheduleState, ServiceHub, TaskServiceApi, WorkflowServiceApi,
};
pub use state_machines::{
    load_state_machines_for_project, state_machines_path, write_state_machines_document, LoadedStateMachines,
    MachineSource, RequirementLifecycleEvent, StateMachineMode, StateMachinesDocument,
};
pub use task_dispatch_policy::{routing_complexity_for_task, should_skip_task_dispatch, workflow_ref_for_task};
pub use types::{
    AgentHandoffRequestInput, AgentHandoffResult, AgentHandoffStatus, ArchitectureEdge, ArchitectureEntity,
    ArchitectureGraph, Assignee, ChecklistItem, CheckpointReason, CodebaseInsight, Complexity, ComplexityAssessment,
    ComplexityTier, DaemonHealth, DaemonStatus, DependencyType, DispatchHistoryEntry, HandoffTargetRole, ImpactArea,
    ListPage, ListPageRequest, LogEntry, LogLevel, OrchestratorProject, OrchestratorTask, OrchestratorWorkflow,
    PhaseDecision, PhaseDecisionVerdict, PhaseEvidence, PhaseEvidenceKind, Priority, ProjectConcurrencyLimits,
    ProjectConfig, ProjectCreateInput, ProjectMetadata, ProjectModelPreferences, ProjectType, RequirementComment,
    RequirementFilter, RequirementItem, RequirementLinks, RequirementPriority, RequirementPriorityExt,
    RequirementQuery, RequirementQuerySort, RequirementRange, RequirementStatus, RequirementType,
    RequirementsDraftInput, RequirementsDraftResult, RequirementsExecutionInput, RequirementsExecutionResult,
    RequirementsRefineInput, ResourceRequirements, RiskLevel, Scope, SubjectDispatch, SubjectRef, TaskCreateInput,
    TaskDensity, TaskDependency, TaskFilter, TaskMetadata, TaskPriorityDistribution, TaskPriorityPolicyReport,
    TaskPriorityRebalanceChange, TaskPriorityRebalanceOptions, TaskPriorityRebalancePlan, TaskQuery, TaskQuerySort,
    TaskStatistics, TaskStatus, TaskType, TaskUpdateInput, VisionDocument, VisionDraftInput, WorkflowCheckpoint,
    WorkflowCheckpointMetadata, WorkflowDecisionAction, WorkflowDecisionRecord, WorkflowDecisionRisk,
    WorkflowDecisionSource, WorkflowFilter, WorkflowMachineEvent, WorkflowMachineState, WorkflowMetadata,
    WorkflowPhaseExecution, WorkflowPhaseStatus, WorkflowQuery, WorkflowQuerySort, WorkflowRunInput, WorkflowStatus,
    WorkflowSubject, DEFAULT_HIGH_PRIORITY_BUDGET_PERCENT, MAX_DISPATCH_HISTORY_ENTRIES, SUBJECT_KIND_CUSTOM,
    SUBJECT_KIND_REQUIREMENT, SUBJECT_KIND_TASK,
};
pub use workflow::{
    delete_requirement, delete_task, load_all_requirements, load_all_tasks, load_task,
    migrate_tasks_and_requirements_from_core_state, open_project_db, phase_plan_for_workflow_ref,
    resolve_phase_plan_for_workflow_ref, save_requirement, save_task, CleanupResult, ResumabilityStatus, ResumeConfig,
    WorkflowCheckpointPruneResult, WorkflowLifecycleExecutor, WorkflowResumeManager, WorkflowStateMachine,
    WorkflowStateManager, DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE,
    REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF, REQUIREMENT_TASK_GENERATION_WORKFLOW_REF, STANDARD_WORKFLOW_REF,
    UI_UX_WORKFLOW_REF,
};
pub use workflow_config::{
    builtin_workflow_config, compile_yaml_workflow_files, ensure_workflow_config_compiled, ensure_workflow_config_file,
    expand_variables, expand_workflow_phases, legacy_workflow_config_paths, load_workflow_config,
    load_workflow_config_or_default, load_workflow_config_with_metadata, merge_yaml_into_config,
    parse_yaml_workflow_config, resolve_workflow_phase_plan, resolve_workflow_rework_attempts,
    resolve_workflow_skip_guards, resolve_workflow_variables, resolve_workflow_verdict_routing,
    validate_and_compile_yaml_workflows, validate_workflow_and_runtime_configs,
    validate_workflow_and_runtime_configs_with_project_root, validate_workflow_config, workflow_config_hash,
    workflow_config_path, write_workflow_config, yaml_workflows_dir, CompileYamlResult, LoadedWorkflowConfig,
    PhaseMcpBinding, PhaseTransitionConfig, PhaseUiDefinition, SubWorkflowRef, WorkflowCheckpointRetentionConfig,
    WorkflowConfig, WorkflowConfigMetadata, WorkflowConfigSource, WorkflowDefinition, WorkflowPhaseConfig,
    WorkflowPhaseEntry, WorkflowSchedule, WorkflowVariable, WORKFLOW_CONFIG_FILE_NAME, WORKFLOW_CONFIG_SCHEMA_ID,
    WORKFLOW_CONFIG_VERSION, YAML_WORKFLOWS_DIR,
};
pub use workflow_events::{dispatch_workflow_event, WorkflowEvent, WorkflowEventOutcome};
pub use workflow_runner_registry::{
    active_workflow_runner_ids, register_workflow_runner_pid, unregister_workflow_runner_pid,
};

#[cfg(test)]
mod state_machine_parity;

#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::OnceLock;

    pub fn stable_test_home() -> &'static std::path::Path {
        static HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
        HOME.get_or_init(|| {
            let home_dir = std::env::temp_dir()
                .join(format!("ao-orchestrator-core-test-config-{}", std::process::id()))
                .join("home");
            std::fs::create_dir_all(&home_dir).expect("create shared test home dir");
            std::env::set_var("HOME", &home_dir);
            home_dir
        })
    }
}
