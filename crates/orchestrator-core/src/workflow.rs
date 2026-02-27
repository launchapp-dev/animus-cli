mod lifecycle_executor;
mod phase_plan;
mod resume;
mod state_machine;
mod state_manager;

pub use lifecycle_executor::WorkflowLifecycleExecutor;
pub use phase_plan::{
    phase_plan_for_pipeline_id, resolve_phase_plan_for_pipeline, STANDARD_PIPELINE_ID,
    UI_UX_PIPELINE_ID,
};
pub use resume::{ResumabilityStatus, ResumeConfig, WorkflowResumeManager};
pub use state_machine::WorkflowStateMachine;
pub use state_manager::{
    WorkflowCheckpointPruneResult, WorkflowStateManager,
    DEFAULT_CHECKPOINT_RETENTION_KEEP_LAST_PER_PHASE,
};

#[cfg(test)]
mod tests;
