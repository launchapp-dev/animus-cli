pub mod audit;
pub mod config;
pub mod control;
mod daemon;
mod dispatch;
mod log_storage;
pub mod metrics;
pub mod quotas;
mod schedule;
mod subject_dispatch;
mod tick;

pub use audit::{
    audit_log_path, log as log_audit_event, log_event as log_audit, log_plugin_install as log_audit_plugin_install,
    Audit, AuditActor, AuditEvent, AuditEventKind, AUDIT_LOG_MAX_BYTES,
};

pub use daemon::{
    current_scheduler_nudge, current_workflow_event_emitter, discover_installed_plugins,
    discover_installed_plugins_with_flavor_error, lock_drift_warnings, nudge_scheduler_local, queue_warnings,
    run_daemon, run_plugin_preflight, workflow_runner_warnings, DaemonEventLog, DaemonEventsPollResponse,
    DaemonRunEvent, DaemonRunGuard, DaemonRunHooks, DaemonRuntimeOptions, DaemonRuntimeState, DiscoveredPluginSummary,
    PreflightOutcome,
};
pub use dispatch::{
    active_workflow_subject_ids, active_workflow_task_ids, build_completion_reconciliation_plan, build_runner_command,
    build_runner_command_from_dispatch, build_runner_command_with_resume, dispatch_capacity_for_options,
    execute_dispatch_plan_via_runner, is_terminally_completed_workflow, ready_dispatch_limit, schedule_headroom,
    workflow_current_phase_id, CompletedProcess, CompletedProcessReconciliation, CompletionReconciliationPlan,
    DispatchNotice, DispatchNoticeSink, DispatchSelectionSource, DispatchWorkflowStart, DispatchWorkflowStartSummary,
    PlannedDispatchStart, ProcessManager, TickBudget, WorkflowConcurrencyCapReached, WorkflowFailureEvent,
};
/// v0.5.1 P2 #6.2 round-3: daemon-side reattach client surface, exposed for
/// integration tests and out-of-tree daemons that want to call
/// [`reattach::try_reattach`] directly. The orphan-scan startup path uses it
/// internally; consumers should typically rely on `OrphanAgentReattached` /
/// `OrphanAgentReattachFailed` events instead of driving it by hand.
pub mod reattach {
    pub use crate::dispatch::reattach::*;
}
/// Daemon-spawned runner records (`runs/_pending/agents/*.json`), exposed so
/// out-of-process callers (e.g. `animus workflow resume`) can detect a live
/// daemon-owned runner for a subject before spawning a second one.
pub mod agent_record {
    pub use crate::dispatch::agent_record::*;
}
pub use log_storage::{
    clear_log_storage_handle, current_log_storage_handle, discover_log_storage_backends, install_log_storage_handle,
    log_storage_disable_env_set, resolve_log_storage_dispatch, spawn_log_storage_supervisor, LogStorageDispatch,
    LogStorageHandle, LogStorageResolution, LogStorageSupervisorOutcome, LOG_STORAGE_DISABLE_ENV,
};
pub use protocol::{RunnerEvent, SubjectDispatch, SubjectDispatchExt, SubjectExecutionFact};
pub use quotas::{install_runtime_quotas, live_plugin_process_count, runtime_quotas, RuntimeQuotas};
pub use schedule::{
    discover_trigger_plugins, ScheduleDispatch, ScheduleDispatchOutcome, TriggerDispatch, TriggerDispatchOutcome,
    TriggerSupervisor, TriggerSupervisorEvent, TriggerSupervisorSink, MAX_RESTART_ATTEMPTS,
};
pub use subject_dispatch::{
    discover_subject_backends, resolve_subject_dispatch, subject_plugins_disable_env_set, SubjectDispatchResolution,
    SubjectPluginDispatch, SUBJECT_PLUGINS_DISABLE_ENV,
};
pub use tick::{
    default_slim_project_tick_driver, run_project_tick, run_project_tick_at, BudgetBreachEvent,
    DefaultProjectTickServices, DefaultSlimProjectTickDriver, ProjectTickContext, ProjectTickExecutionOutcome,
    ProjectTickHooks, ProjectTickPlan, ProjectTickPreparation, ProjectTickRunMode, ProjectTickSnapshot,
    ProjectTickSummary, ProjectTickSummaryInput, ProjectTickTime, TaskStateChangeEvent, TickSummaryBuilder,
};

#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::OnceLock;

    /// Returns the per-process test home directory and pins HOME to it on
    /// first call. Tests that resolve `protocol::scoped_state_root` (or any
    /// other `$HOME`-derived path) must call this before touching scoped
    /// state so the one-time HOME pin cannot land mid-test.
    pub fn stable_test_home() -> &'static std::path::Path {
        static HOME: OnceLock<std::path::PathBuf> = OnceLock::new();
        HOME.get_or_init(|| {
            let home_dir =
                std::env::temp_dir().join(format!("ao-daemon-rt-test-config-{}", std::process::id())).join("home");
            std::fs::create_dir_all(&home_dir).expect("create shared daemon-runtime test home");
            std::env::set_var("HOME", &home_dir);
            home_dir
        })
    }
}
