pub mod audit;
pub mod control;
mod daemon;
mod dispatch;
mod log_storage;
pub mod metrics;
mod queue;
pub mod quotas;
mod schedule;
mod subject_dispatch;
mod tick;

pub use audit::{
    audit_log_path, log as log_audit_event, log_event as log_audit, log_plugin_install as log_audit_plugin_install,
    Audit, AuditActor, AuditEvent, AuditEventKind, AUDIT_LOG_MAX_BYTES,
};

pub use daemon::{
    current_workflow_event_emitter, discover_installed_plugins, run_daemon, run_plugin_preflight, DaemonEventLog,
    DaemonEventsPollResponse, DaemonRunEvent, DaemonRunGuard, DaemonRunHooks, DaemonRuntimeOptions, DaemonRuntimeState,
    DiscoveredPluginSummary, PreflightOutcome,
};
pub use dispatch::{
    active_workflow_subject_ids, active_workflow_task_ids, build_completion_reconciliation_plan, build_runner_command,
    build_runner_command_from_dispatch, execute_dispatch_plan_via_runner, is_terminally_completed_workflow,
    ready_dispatch_limit, ready_dispatch_limit_for_options, schedule_headroom, workflow_current_phase_id,
    CompletedProcess, CompletionReconciliationPlan, DispatchNotice, DispatchNoticeSink, DispatchSelectionSource,
    DispatchWorkflowStart, DispatchWorkflowStartSummary, PlannedDispatchStart, ProcessManager, TickBudget,
};
/// v0.5.1 P2 #6.2 round-3: daemon-side reattach client surface, exposed for
/// integration tests and out-of-tree daemons that want to call
/// [`reattach::try_reattach`] directly. The orphan-scan startup path uses it
/// internally; consumers should typically rely on `OrphanAgentReattached` /
/// `OrphanAgentReattachFailed` events instead of driving it by hand.
pub mod reattach {
    pub use crate::dispatch::reattach::*;
}
pub use log_storage::{
    clear_log_storage_handle, current_log_storage_handle, discover_log_storage_backends, install_log_storage_handle,
    log_storage_disable_env_set, resolve_log_storage_dispatch, spawn_log_storage_supervisor, LogStorageDispatch,
    LogStorageHandle, LogStorageResolution, LogStorageSupervisorOutcome, LOG_STORAGE_DISABLE_ENV,
};
pub use protocol::{RunnerEvent, SubjectDispatch, SubjectDispatchExt, SubjectExecutionFact};
pub use queue::{
    dispatch_queue_state_path, load_dispatch_queue_state, mark_dispatch_queue_entry_assigned,
    remove_terminal_dispatch_queue_entry_non_fatal, save_dispatch_queue_state, DispatchQueueEntry,
    DispatchQueueEntryStatus, DispatchQueueState,
};
pub use quotas::{install_runtime_quotas, runtime_quotas, RuntimeQuotas};
pub use schedule::{
    discover_trigger_plugins, ScheduleDispatch, ScheduleDispatchOutcome, TriggerDispatch, TriggerDispatchOutcome,
    TriggerSupervisor, TriggerSupervisorEvent, TriggerSupervisorSink, MAX_RESTART_ATTEMPTS,
};
pub use subject_dispatch::{
    discover_subject_backends, resolve_subject_dispatch, subject_plugins_disable_env_set, SubjectDispatchResolution,
    SubjectPluginDispatch, SUBJECT_PLUGINS_DISABLE_ENV,
};
pub use tick::{
    default_slim_project_tick_driver, run_project_tick, run_project_tick_at, DefaultProjectTickServices,
    DefaultSlimProjectTickDriver, ProjectTickContext, ProjectTickExecutionOutcome, ProjectTickHooks, ProjectTickPlan,
    ProjectTickPreparation, ProjectTickRunMode, ProjectTickSnapshot, ProjectTickSummary, ProjectTickSummaryInput,
    ProjectTickTime, TaskStateChangeEvent, TickSummaryBuilder,
};
