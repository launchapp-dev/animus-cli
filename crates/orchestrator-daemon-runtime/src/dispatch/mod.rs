pub mod agent_record;
mod build_runner_command_from_dispatch;
mod completed_process;

#[cfg(test)]
pub(crate) mod test_env {
    //! Shared serialization point for dispatch tests that mutate
    //! process-wide environment variables (`ANIMUS_WORKFLOW_RUNNER_BIN`,
    //! `PATH`). Without a single shared lock, sibling module-level locks
    //! race each other because env state is global per-process.
    use std::sync::{Mutex, OnceLock};

    pub fn lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
}
mod completion_reconciliation_plan;
mod dispatch_execution;
mod dispatch_notice;
mod dispatch_selection_source;
mod dispatch_support;
mod dispatch_workflow_start;
mod dispatch_workflow_start_summary;
pub mod event_pipe;
mod process_manager;
mod ready_dispatch_plan;
pub mod reattach;

pub use build_runner_command_from_dispatch::{build_runner_command, build_runner_command_from_dispatch};
pub use completed_process::CompletedProcess;
pub use completion_reconciliation_plan::{build_completion_reconciliation_plan, CompletionReconciliationPlan};
pub use dispatch_execution::execute_dispatch_plan_via_runner;
pub use dispatch_notice::{DispatchNotice, DispatchNoticeSink};
pub use dispatch_selection_source::DispatchSelectionSource;
pub use dispatch_support::{
    active_workflow_subject_ids, active_workflow_task_ids, is_terminally_completed_workflow, ready_dispatch_limit,
    ready_dispatch_limit_for_options, schedule_headroom, workflow_current_phase_id, TickBudget,
};
pub use dispatch_workflow_start::DispatchWorkflowStart;
pub use dispatch_workflow_start_summary::DispatchWorkflowStartSummary;
#[cfg(unix)]
#[allow(unused_imports)]
pub use event_pipe::SubprocessEventPipe;
pub use process_manager::ProcessManager;
pub use ready_dispatch_plan::PlannedDispatchStart;
