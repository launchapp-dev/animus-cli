mod dependency;
mod environment_client;
mod journal_client;
mod lifecycle_executor;
mod phase_plan;
mod resume;
mod state_machine;
mod state_manager;

pub use dependency::{
    classify_status, detect_cycles, evaluate_join, is_awaiting_release, resolve_all_joins, resolve_ready_joins,
    should_hold_at_enqueue, validate_declaration, DependencyError, JoinDecision, JoinResolution, RunDependencySpec,
    RunSnapshot, UpstreamFailurePolicy, UpstreamOutcome, DEPENDS_ON_VAR, JOIN_POLICY_VAR,
};

pub use environment_client::{
    shutdown_resident_hosts as shutdown_environment_hosts, EnvironmentClient, EnvironmentNode, ReapReport,
};

pub use journal_client::{
    durable_journal_active, import_local_sqlite_into_plugin, record_wire_event as journal_record_wire_event,
    shutdown_resident_hosts as shutdown_journal_hosts, JournalImportStats, WorkflowRunSummary,
};

pub use lifecycle_executor::WorkflowLifecycleExecutor;
pub use phase_plan::{
    phase_plan_for_workflow_ref, resolve_phase_plan_for_workflow_ref, resolve_phase_plan_for_workflow_ref_for_actor,
    STANDARD_WORKFLOW_REF, UI_UX_WORKFLOW_REF,
};
pub use resume::{ResumabilityStatus, ResumeConfig, WorkflowResumeManager};
pub use state_machine::WorkflowStateMachine;
pub use state_manager::{
    count_tasks_with_status, delete_requirement, delete_task, load_active_workflow_summaries, load_all_requirements,
    load_all_tasks, load_blocked_task_summaries, load_next_task_by_priority, load_recent_failed_workflow_summaries,
    load_requirement, load_requirement_link_summaries_by_ids, load_requirements_by_ids, load_stale_task_summaries,
    load_task, load_task_priority_policy_report, load_task_statistics, load_task_titles_by_ids, load_tasks_by_ids,
    load_workflow_history_summaries, load_workflow_ref_index, migrate_tasks_and_requirements_from_core_state,
    open_project_db, query_requirement_ids, query_task_ids, save_requirement, save_task, BlockedTaskSummary,
    CleanupResult, RequirementLinkSummary, StaleTaskSummary, WorkflowActivitySummary, WorkflowCheckpointPruneResult,
    WorkflowFailureSummary, WorkflowHistorySummary, WorkflowStateManager,
    DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE,
};
pub(crate) use state_manager::{
    delete_requirement_with_conn, delete_task_with_conn, save_requirement_with_conn, save_task_with_conn,
};
pub use state_manager::{
    is_terminal_workflow_run_status, select_workflow_prune_candidates, WorkflowRunDeletion, WorkflowRunPruneCandidate,
    WorkflowRunPruneFilter, WorkflowRunPruneReport,
};

#[cfg(test)]
mod tests;
