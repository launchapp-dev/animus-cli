use super::*;
#[cfg(test)]
use crate::services::runtime::runtime_daemon::canonicalize_lossy;
use orchestrator_daemon_runtime::ProcessManager;
#[cfg(test)]
use orchestrator_daemon_runtime::{
    run_project_tick_at, ProjectTickRunMode, ProjectTickSummary, ProjectTickTime,
};

#[path = "daemon_task_dispatch.rs"]
pub(super) mod task_dispatch;

#[cfg(test)]
#[path = "daemon_reconciliation_test_support.rs"]
pub(super) mod reconciliation_test_support;

#[path = "daemon_tick_executor.rs"]
mod tick_executor;
#[path = "daemon_tick_wrapper.rs"]
mod tick_wrapper;

use task_dispatch::*;
pub(crate) use tick_executor::slim_project_tick_driver;
#[cfg(test)]
use tick_executor::SlimProjectTickDriver;
#[cfg(test)]
use tick_wrapper::{
    apply_cli_pre_tick, flush_git_outbox_for_project, refresh_runtime_binaries_for_project,
    run_cli_pre_tick,
};

#[cfg(test)]
pub(super) async fn slim_daemon_tick(
    root: &str,
    args: &DaemonRuntimeOptions,
    process_manager: &mut ProcessManager,
    dispatch_paused: bool,
) -> Result<ProjectTickSummary> {
    slim_daemon_tick_at(
        root,
        args,
        process_manager,
        dispatch_paused,
        chrono::Utc::now(),
    )
    .await
}

#[cfg(test)]
pub(super) async fn slim_daemon_tick_at(
    root: &str,
    args: &DaemonRuntimeOptions,
    process_manager: &mut ProcessManager,
    dispatch_paused: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<ProjectTickSummary> {
    let root = canonicalize_lossy(root);
    let active_subject_ids = process_manager.active_subject_ids();
    let pre_tick = run_cli_pre_tick(&root, args, Some(&active_subject_ids)).await?;
    flush_git_outbox_for_project(&root);
    let mode = ProjectTickRunMode {
        active_process_count: process_manager.active_count(),
    };
    let mut driver: SlimProjectTickDriver<'_> = slim_project_tick_driver(process_manager);
    let mut summary = run_project_tick_at(
        &root,
        args,
        mode,
        dispatch_paused,
        &mut driver,
        ProjectTickTime::from_utc(now),
    )
    .await?;
    apply_cli_pre_tick(&mut summary, pre_tick);
    refresh_runtime_binaries_for_project(&root).await?;
    Ok(summary)
}

#[cfg(test)]
#[path = "daemon_scheduler_project_tick_tests.rs"]
mod tests;
