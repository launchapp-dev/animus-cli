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
    CleanupResult, WorkflowCheckpointPruneResult, WorkflowStateManager,
    DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE,
    open_project_db, save_task, load_task, load_all_tasks, delete_task,
    save_requirement, load_all_requirements, delete_requirement,
    migrate_tasks_and_requirements_from_core_state,
};

#[cfg(test)]
mod tests;
