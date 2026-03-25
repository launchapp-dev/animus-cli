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
    delete_requirement, delete_task, load_all_requirements, load_all_tasks, load_task,
    migrate_tasks_and_requirements_from_core_state, open_project_db, save_requirement, save_task, CleanupResult,
    WorkflowCheckpointPruneResult, WorkflowStateManager, DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE,
};

#[cfg(test)]
mod tests;
