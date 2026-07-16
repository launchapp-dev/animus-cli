// phase-decision-test
pub mod agent_runtime_config;
pub mod config;
pub mod daemon_config;
pub mod daemon_tick_metrics;
pub mod doctor;
pub mod domain_state;
/// v0.7 environment plugin kind wire types, re-exported so the follow-on runtime
/// `EnvironmentClient` (prepare / exec / exec_stream / teardown) and the
/// out-of-tree workflow runner can reach `EnvironmentSpec` / `HarnessCommand` /
/// `ExecRequest` / `ExecResponse` / `EnvironmentHandle` from one place. The
/// client itself is NOT implemented yet — this is the type seam only.
pub mod environment {
    pub use animus_environment_protocol::*;
}
pub mod execution_projection;
pub mod flavor;
pub mod model_quality;
pub mod plugin_preflight;
pub mod plugin_registry;
pub mod principal;
pub mod runtime_contract;
pub mod secret_device_store;
pub mod secret_keysource;
pub mod secret_store;
pub mod services;
pub mod state_machines;
pub mod store;
pub mod subject_adapter;
pub mod task_dispatch_policy;
pub mod types;
pub mod workflow;
pub mod workflow_config;
pub mod workflow_events;
pub mod workflow_runner_registry;

pub use agent_runtime_config::{
    agent_runtime_config_path, builtin_agent_runtime_config, ensure_agent_runtime_config_file,
    load_agent_runtime_config, load_agent_runtime_config_or_default, load_agent_runtime_config_or_default_for_actor,
    write_agent_runtime_config, AgentProfile, AgentRuntimeConfig, AgentRuntimeMetadata, AgentRuntimeOverrides,
    AgentRuntimeSource, BackoffConfig, CliToolConfig, CommandCwdMode, Idempotency, LoadedAgentRuntimeConfig,
    PhaseCommandDefinition, PhaseDecisionContract, PhaseExecutionDefinition, PhaseExecutionMode, PhaseManualDefinition,
    PhaseOutputContract, PhaseRetryConfig, DEFAULT_MAX_REWORK_ATTEMPTS,
};
pub use config::RuntimeConfig;
pub use daemon_config::{
    daemon_project_config_path, load_daemon_project_config, resolve_silent_threshold_mins, write_daemon_project_config,
    DaemonProjectConfig, DAEMON_PROJECT_CONFIG_FILE_NAME, DEFAULT_SILENT_THRESHOLD_MINS,
};
pub use daemon_tick_metrics::DaemonTickMetrics;
pub use doctor::{DoctorCheck, DoctorCheckResult, DoctorCheckStatus, DoctorRemediation, DoctorReport};
pub use domain_state::{
    errors_path, handoffs_path, history_path, load_errors, load_handoffs, load_history_store, project_state_dir,
    read_json_or_default, save_errors, save_handoffs, save_history_store, write_json_atomic, write_json_pretty,
    ErrorRecord, ErrorStore, HandoffRecord, HandoffStore, HistoryExecutionRecord, HistoryStore,
};
pub use execution_projection::{
    builtin_execution_projector_registry, execution_fact_subject_kind, hub_task_projection_store,
    project_execution_fact, project_requirement_workflow_status, project_schedule_dispatch_attempt,
    project_schedule_dispatch_missed, project_schedule_execution_fact, project_task_blocked_with_reason,
    project_task_execution_fact, project_task_status, project_task_terminal_workflow_status,
    project_task_workflow_pause_cleared, project_task_workflow_paused, project_task_workflow_start,
    workflow_paused_reason, ExecutionProjector, ExecutionProjectorRegistry, HubTaskProjectionStore,
    TaskProjectionStore, TaskProjectionView, WORKFLOW_PAUSED_REASON_PREFIX, WORKFLOW_RUNNER_BLOCKED_PREFIX,
};
pub use flavor::{
    list_available_flavor_names, load_flavor, locate_flavor_manifest, FlavorDefaults, FlavorManifest,
    FlavorRoleSection, DEFAULT_FLAVOR_ID, FLAVOR_SCHEMA_V1,
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
pub use plugin_preflight::{
    queue_underpin_warning, summarize_discovered_plugins, summarize_discovered_plugins_with_lock,
    workflow_runner_underpin_warning, AutoInstalledPlugin, InstalledPluginSummary, MissingPlugin, PluginInstaller,
    PluginPreflightRunner, PluginPreflightSpec, PreflightResult, RequiredRole, DEFAULT_PROVIDER_REPO,
    DEFAULT_REQUIREMENT_BACKEND_REPO, DEFAULT_TASK_BACKEND_REPO, QUEUE_PRECISE_WAKE_FLOOR, WORKFLOW_RUNNER_SKILL_FLOOR,
};
pub use plugin_registry::{
    default_provider_repo_spec, default_subject_backend_repo, default_subject_repo_for_kind, format_repo_spec,
    resolve_curated_plugin_by_basename, resolve_tag_for_slug, DEFAULT_OAI_AGENT_PLUGINS, DEFAULT_PROVIDER_PLUGINS,
    DEFAULT_QUEUE_PLUGINS, DEFAULT_SUBJECT_PLUGINS, DEFAULT_TRANSPORT_PLUGINS, DEFAULT_WORKFLOW_RUNNER_PLUGINS,
};
pub use principal::{
    bootstrap_principals_file_if_absent, bootstrap_principals_file_if_absent_for, check_principal_can,
    current_os_username, default_principals_path, load_principals_file, resolve_principal_by_id,
    resolve_principal_for_os_user, role_allows_method, PermissionDecision, Principal, PrincipalEntry, PrincipalKind,
    PrincipalsError, PrincipalsFile, PrincipalsPolicy, RbacConfig, RbacMode,
};
pub use runtime_contract::{
    build_cli_launch_contract, build_runtime_contract, cli_capabilities_for_tool, cli_capabilities_from_config,
    cli_tool_executable, cli_tool_read_only_flag, cli_tool_response_schema_flag, CliCapabilities, CliSessionResumeMode,
    CliSessionResumePlan,
};
pub use secret_device_store::{build_backend, build_secret_store, DeviceEncryptedSecretStore};
pub use secret_keysource::{KeySource, KeySourceConfig, KeySourceKind};
pub use secret_store::{
    enforce_injection_cap, index_path as secrets_index_path, keychain_service_name,
    validate_key as validate_secret_key, KeyringSecretStore, MockSecretStore, SecretStore, SecretStoreError,
    SecretStoreResult, INDEX_FILE_NAME, KEYCHAIN_SERVICE_PREFIX, MAX_INJECTED_ENV_BYTES, SECRETS_DIR_NAME,
};
pub use services::{
    evaluate_task_priority_policy, load_daemon_health_snapshot, load_daemon_status_snapshot_fast, load_schedule_state,
    load_trigger_state, lock_trigger_state, plan_task_priority_rebalance, save_schedule_state, save_trigger_state,
    set_daemon_health_cache_disabled, summarize_tasks, DaemonServiceApi, DaemonStatusSnapshot, FileServiceHub,
    InMemoryServiceHub, PhaseExecutionRequest, PhaseExecutionResult, PhaseExecutor, PhaseVerdict, PlanningServiceApi,
    ReviewServiceApi, ScheduleRunState, ScheduleState, ServiceHub, TaskServiceApi, TriggerRunState, TriggerState,
    WebhookEvent, WorkflowServiceApi,
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
    count_tasks_with_status, delete_requirement, delete_task, load_active_workflow_summaries, load_all_requirements,
    load_all_tasks, load_blocked_task_summaries, load_next_task_by_priority, load_recent_failed_workflow_summaries,
    load_requirement_link_summaries_by_ids, load_requirements_by_ids, load_stale_task_summaries, load_task,
    load_task_priority_policy_report, load_task_statistics, load_task_titles_by_ids, load_tasks_by_ids,
    load_workflow_history_summaries, load_workflow_ref_index, migrate_tasks_and_requirements_from_core_state,
    open_project_db, phase_plan_for_workflow_ref, query_requirement_ids, query_task_ids,
    resolve_phase_plan_for_workflow_ref, resolve_phase_plan_for_workflow_ref_for_actor, save_requirement, save_task,
    BlockedTaskSummary, CleanupResult, RequirementLinkSummary, ResumabilityStatus, ResumeConfig, StaleTaskSummary,
    WorkflowActivitySummary, WorkflowCheckpointPruneResult, WorkflowFailureSummary, WorkflowHistorySummary,
    WorkflowLifecycleExecutor, WorkflowResumeManager, WorkflowStateMachine, WorkflowStateManager,
    DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE, STANDARD_WORKFLOW_REF, UI_UX_WORKFLOW_REF,
};
pub use workflow::{
    durable_journal_active, is_terminal_workflow_run_status, select_workflow_prune_candidates, WorkflowRunDeletion,
    WorkflowRunPruneCandidate, WorkflowRunPruneFilter, WorkflowRunPruneReport,
};
pub use workflow::{shutdown_environment_hosts, EnvironmentClient, EnvironmentJournalEvent};
pub use workflow_config::{
    builtin_workflow_config, compile_yaml_workflow_files, ensure_workflow_config_compiled, ensure_workflow_config_file,
    expand_variables, expand_workflow_phases, generated_workflow_phase_is_defined, legacy_workflow_config_paths,
    load_workflow_config, load_workflow_config_or_default, load_workflow_config_or_default_for_actor,
    load_workflow_config_with_metadata, merge_yaml_into_config, missing_project_skill_reference_warnings,
    missing_skill_reference_warnings_for_sources, missing_skill_yaml_warnings, parse_yaml_workflow_config,
    remove_agent_profile, remove_generated_workflow_phase, remove_workflow_definition, resolve_workflow_phase_plan,
    resolve_workflow_rework_attempts, resolve_workflow_skip_guards, resolve_workflow_variables,
    resolve_workflow_verdict_routing, unenforced_project_yaml_warnings, unenforced_yaml_field_warnings,
    upsert_agent_profile, upsert_generated_workflow_phase, upsert_generated_workflow_pipeline,
    upsert_workflow_definition, validate_and_compile_yaml_workflows, validate_workflow_and_runtime_configs,
    validate_workflow_and_runtime_configs_with_project_root, validate_workflow_config, workflow_config_hash,
    workflow_config_path, write_full_workflow_config, write_workflow_config, yaml_workflows_dir, CompileYamlResult,
    FileWatcherTriggerConfig, LoadedWorkflowConfig, PhaseMcpBinding, PhaseTransitionConfig, PhaseUiDefinition,
    SkillReferenceWarning, SubWorkflowRef, TriggerType, UnenforcedFieldWarning, WebhookTriggerConfig,
    WorkflowCheckpointRetentionConfig, WorkflowConfig, WorkflowConfigMetadata, WorkflowConfigSource,
    WorkflowDefinition, WorkflowPhaseConfig, WorkflowPhaseEntry, WorkflowSchedule, WorkflowTrigger, WorkflowVariable,
    WORKFLOW_CONFIG_FILE_NAME, WORKFLOW_CONFIG_SCHEMA_ID, WORKFLOW_CONFIG_VERSION, YAML_WORKFLOWS_DIR,
};
pub use workflow_config::{YamlDiagnostic, YamlExcerpt};
pub use workflow_events::{dispatch_workflow_event, workflow_task_id, WorkflowEvent, WorkflowEventOutcome};
pub use workflow_runner_registry::{
    active_workflow_runner_ids, register_workflow_runner_pid, unregister_workflow_runner_pid,
};

#[cfg(test)]
mod state_machine_parity;

#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::{Mutex, OnceLock};

    /// Pins HOME and the Animus config-dir env vars once per process, all
    /// inside a single `OnceLock` init so the one-time mutation cannot land
    /// between two env reads of the same test. Tests that resolve
    /// `protocol::scoped_state_root` or `protocol::Config::global_config_dir`
    /// must call this before touching that state.
    pub fn stable_test_home() -> &'static std::path::Path {
        static HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
        HOME.get_or_init(|| {
            let config_dir =
                std::env::temp_dir().join(format!("ao-orchestrator-core-test-config-{}", std::process::id()));
            let home_dir = config_dir.join("home");
            std::fs::create_dir_all(&home_dir).expect("create shared test home dir");
            std::env::set_var("ANIMUS_CONFIG_DIR", &config_dir);
            std::env::set_var("AGENT_ORCHESTRATOR_CONFIG_DIR", &config_dir);
            std::env::set_var("HOME", &home_dir);
            home_dir
        })
    }

    /// Serializes every test that reads or writes the machine-installed
    /// packs dir (`~/.animus/packs` under the stable test HOME). The pack
    /// fixture tests in `workflow::phase_plan` install fixtures there, and
    /// any test that depends on that dir's contents through
    /// `crate::resolve_pack_registry` observes the same shared global state.
    pub fn pack_fixture_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
}
