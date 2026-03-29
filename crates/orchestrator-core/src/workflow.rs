mod lifecycle_executor;
mod phase_plan;
mod resume;
mod state_machine;
mod state_manager;

pub use lifecycle_executor::WorkflowLifecycleExecutor;
pub use phase_plan::{
    phase_plan_for_workflow_ref, resolve_phase_plan_for_workflow_ref, REQUIREMENT_TASK_GENERATION_RUN_WORKFLOW_REF,
    REQUIREMENT_TASK_GENERATION_WORKFLOW_REF, STANDARD_WORKFLOW_REF, UI_UX_WORKFLOW_REF,
};
pub use resume::{ResumabilityStatus, ResumeConfig, WorkflowResumeManager};
pub use state_machine::WorkflowStateMachine;
pub use state_manager::{
    count_tasks_with_status, delete_requirement, delete_task, load_active_workflow_summaries, load_all_requirements,
    load_all_tasks, load_blocked_task_summaries, load_next_task_by_priority, load_recent_failed_workflow_summaries,
    load_requirement, load_requirement_link_summaries_by_ids, load_requirements_by_ids,
    load_stale_task_summaries, load_task, load_task_priority_policy_report, load_task_statistics,
    load_task_titles_by_ids, load_tasks_by_ids, load_workflow_history_summaries,
    migrate_tasks_and_requirements_from_core_state, open_project_db, query_requirement_ids, query_task_ids,
    save_requirement, save_task, BlockedTaskSummary, CleanupResult, RequirementLinkSummary, StaleTaskSummary,
    WorkflowActivitySummary, WorkflowCheckpointPruneResult, WorkflowFailureSummary, WorkflowHistorySummary,
    WorkflowStateManager,
    DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE,
};

#[cfg(test)]
mod tests;
