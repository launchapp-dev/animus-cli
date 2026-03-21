use super::*;
use crate::services::runtime::execution_fact_projection::reconcile_completed_processes;
use crate::services::runtime::runtime_daemon::daemon_reconciliation::{
    reconcile_manual_phase_timeouts, reconcile_runner_blocked_tasks, recover_orphaned_running_workflows,
};
use anyhow::Result;
use orchestrator_core::{load_daemon_project_config, services::ServiceHub};
use orchestrator_daemon_runtime::{
    default_slim_project_tick_driver, CompletedProcess, DefaultProjectTickServices, DefaultSlimProjectTickDriver,
    DispatchNotice, DispatchWorkflowStartSummary, ProcessManager, ProjectTickSnapshot,
};
use orchestrator_git_ops::{auto_prune_completed_task_worktrees_after_merge, PostSuccessGitConfig};
use std::sync::Arc;

pub(crate) struct CliProjectTickServices;

impl CliProjectTickServices {
    fn new(_args: &DaemonRuntimeOptions) -> Self {
        Self
    }
}

#[async_trait::async_trait(?Send)]
impl DefaultProjectTickServices for CliProjectTickServices {
    async fn capture_snapshot(&mut self, root: &str) -> Result<ProjectTickSnapshot> {
        let hub: Arc<dyn ServiceHub> = Arc::new(orchestrator_core::FileServiceHub::new(root)?);
        let requirements_before = hub.planning().list_requirements().await?;
        let tasks_before = hub.tasks().list().await?;
        let daemon = hub.daemon();
        let daemon_health = daemon.health().await.ok();

        Ok(ProjectTickSnapshot { requirements_before, tasks_before, started_daemon: false, daemon_health })
    }

    async fn reconcile_completed_processes(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        completed_processes: Vec<CompletedProcess>,
    ) -> Result<(usize, usize)> {
        Ok(reconcile_completed_processes(hub, root, completed_processes).await)
    }

    async fn reconcile_zombie_workflows(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        root: &str,
        active_subject_ids: &std::collections::HashSet<String>,
    ) -> Result<usize> {
        Ok(recover_orphaned_running_workflows(hub, root, active_subject_ids).await)
    }

    async fn reconcile_manual_timeouts(&mut self, hub: Arc<dyn ServiceHub>, root: &str) -> Result<usize> {
        let project_path = std::path::Path::new(root);
        if let Ok(daemon_cfg) = load_daemon_project_config(project_path) {
            let should_prune = daemon_cfg.auto_prune_worktrees_after_merge
                || daemon_cfg.worktree_disk_threshold_mb.is_some_and(|threshold_mb| {
                    orchestrator_git_ops::managed_worktrees_disk_bytes(root) > threshold_mb * 1024 * 1024
                });
            let git_cfg = PostSuccessGitConfig {
                auto_merge_enabled: daemon_cfg.auto_merge_enabled,
                auto_pr_enabled: daemon_cfg.auto_pr_enabled,
                auto_commit_before_merge: daemon_cfg.auto_commit_before_merge,
                auto_merge_target_branch: daemon_cfg.auto_merge_target_branch,
                auto_merge_no_ff: daemon_cfg.auto_merge_no_ff,
                auto_push_remote: daemon_cfg.auto_push_remote,
                auto_cleanup_worktree_enabled: daemon_cfg.auto_cleanup_worktree_enabled,
                auto_prune_worktrees_after_merge: should_prune,
            };
            let _ = auto_prune_completed_task_worktrees_after_merge(hub.clone(), root, &git_cfg).await;
        }
        reconcile_manual_phase_timeouts(hub, root).await
    }

    async fn reconcile_runner_blocked_tasks(&mut self, hub: Arc<dyn ServiceHub>, root: &str) -> Result<usize> {
        reconcile_runner_blocked_tasks(hub, root).await
    }

    async fn dispatch_ready_tasks(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        root: &str,
        limit: usize,
        process_manager: Option<&mut ProcessManager>,
    ) -> Result<DispatchWorkflowStartSummary> {
        match process_manager {
            Some(process_manager) => dispatch_queued_entries_via_runner(root, process_manager, limit),
            None => Ok(DispatchWorkflowStartSummary::default()),
        }
    }

    fn dispatch_notice(&mut self, notice: DispatchNotice) {
        match notice {
            DispatchNotice::ScheduleDispatched { schedule_id, dispatch } => {
                eprintln!(
                    "{}: schedule '{}' fired workflow '{}'",
                    protocol::ACTOR_DAEMON,
                    schedule_id,
                    dispatch.workflow_ref
                );
            }
            DispatchNotice::ScheduleDispatchFailed { schedule_id, dispatch, error } => {
                eprintln!(
                    "{}: schedule '{}' workflow '{}' dispatch failed: {}",
                    protocol::ACTOR_DAEMON,
                    schedule_id,
                    dispatch.workflow_ref,
                    error
                );
            }
            DispatchNotice::QueueAssignmentFailed { dispatch, error } => {
                eprintln!(
                    "{}: failed to mark dispatch queue entry assigned for subject {}: {}",
                    protocol::ACTOR_DAEMON,
                    dispatch.subject_key(),
                    error
                );
            }
            DispatchNotice::Failed { dispatch, error } => {
                eprintln!(
                    "{}: failed to start workflow runner for subject {}: {}",
                    protocol::ACTOR_DAEMON,
                    dispatch.subject_key(),
                    error
                );
            }
            DispatchNotice::Started { .. } => {}
        }
    }
}

pub(crate) type SlimProjectTickDriver<'a> = DefaultSlimProjectTickDriver<'a, CliProjectTickServices>;

pub(crate) fn slim_project_tick_driver<'a>(
    args: &DaemonRuntimeOptions,
    process_manager: &'a mut ProcessManager,
) -> SlimProjectTickDriver<'a> {
    default_slim_project_tick_driver(CliProjectTickServices::new(args), process_manager)
}
