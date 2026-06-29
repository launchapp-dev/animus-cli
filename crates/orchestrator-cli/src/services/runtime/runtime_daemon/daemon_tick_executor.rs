use super::*;
use crate::services::runtime::execution_fact_projection::reconcile_completed_processes;
use crate::services::runtime::runtime_daemon::daemon_reconciliation::{
    reconcile_manual_phase_timeouts, recover_orphaned_running_workflows,
};
use anyhow::Result;
use orchestrator_core::services::ServiceHub;
use orchestrator_core::{TaskStatus, WorkflowStateManager, WorkflowStatus};
use orchestrator_daemon_runtime::{
    default_slim_project_tick_driver, BudgetBreachEvent, CompletedProcess, CompletedProcessReconciliation,
    DefaultProjectTickServices, DefaultSlimProjectTickDriver, DispatchNotice, DispatchWorkflowStartSummary,
    ProcessManager, ProjectTickSnapshot,
};
use orchestrator_logging::Logger;
use std::sync::Arc;

pub(crate) struct CliProjectTickServices {
    logger: Arc<Logger>,
}

impl CliProjectTickServices {
    fn new(_args: &DaemonRuntimeOptions, logger: Arc<Logger>) -> Self {
        Self { logger }
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
    ) -> Result<CompletedProcessReconciliation> {
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
        reconcile_manual_phase_timeouts(hub, root).await
    }

    /// Housekeeping-cadence budget-cap sweep: rescan run spend, evaluate
    /// declared caps, and act on newly crossed ones (per-run decision
    /// record + scoped record + pause). Enforcement failures must never
    /// take the tick down — they are logged and the sweep retries on the
    /// next heartbeat.
    async fn enforce_budget_caps(&mut self, hub: Arc<dyn ServiceHub>, root: &str) -> Result<Vec<BudgetBreachEvent>> {
        let logger = self.logger.clone();
        // Record the leg status every sweep — even when the kill-switch
        // skips enforcement — so `daemon health` / `animus status` can show
        // `budget_enforcement: {enabled, last_sweep_at}` without reading the
        // daemon's process env. A persistence failure is non-fatal.
        let enabled = crate::services::cost::budget_enforcement_enabled();
        if let Err(error) = crate::services::cost::save_budget_enforcement_status(std::path::Path::new(root), enabled) {
            self.logger.warn("budget", format!("failed to record budget-enforcement status: {error}")).emit();
        }
        if !enabled {
            // Kill-switch active: skip the enforcement leg entirely (no
            // scan, no pause, no notify). Logged once per sweep at debug
            // cadence via the warn channel for operator visibility.
            return Ok(Vec::new());
        }
        let mut warn = |message: String| {
            logger.warn("budget", message).emit();
        };
        match crate::services::cost::enforcement::run_budget_enforcement(hub, root, &mut warn).await {
            Ok(events) => {
                for event in &events {
                    self.logger
                        .warn(
                            "budget",
                            format!(
                                "budget breach: workflow run {} crossed {} {} ({} > {}) — on_exceed={}, action={}",
                                event.workflow_run_id,
                                event.limit_kind,
                                event.limit_field,
                                event.actual,
                                event.budget,
                                event.on_exceed,
                                event.action
                            ),
                        )
                        .emit();
                }
                Ok(events)
            }
            Err(error) => {
                self.logger.error("budget", "budget-cap sweep failed").err(error.to_string()).emit();
                Ok(Vec::new())
            }
        }
    }

    /// Suppress ALL new dispatch (schedules, triggers, ready tasks, queue
    /// drain) when the fleet daily spend cap is latched. Honor the
    /// enforcement kill-switch: when budget enforcement is disabled the
    /// latch is never reconciled, so a stale latch must not strand dispatch.
    fn dispatch_suppressed(&self, root: &str) -> bool {
        crate::services::cost::budget_enforcement_enabled()
            && crate::services::cost::daily_cap::is_dispatch_paused(std::path::Path::new(root))
    }

    async fn reconcile_stale_in_progress_tasks(
        &mut self,
        hub: Arc<dyn ServiceHub>,
        _root: &str,
        active_subject_ids: &std::collections::HashSet<String>,
        stale_threshold_hours: u64,
    ) -> Result<usize> {
        let grace_secs = i64::try_from(stale_threshold_hours.saturating_mul(3600)).unwrap_or(i64::MAX);
        reconcile_stale_in_progress_tasks_for_hub(hub, active_subject_ids, grace_secs).await
    }

    async fn cleanup_stale_workflows(
        &mut self,
        _hub: Arc<dyn ServiceHub>,
        root: &str,
        max_age_hours: u64,
    ) -> Result<usize> {
        let manager = WorkflowStateManager::new(root);
        let deleted = match manager.cleanup_terminal_workflows(max_age_hours) {
            Ok(result) => {
                if result.deleted > 0 {
                    self.logger
                        .info(
                            "cleanup",
                            format!("cleaned up {} stale workflows (older than {}h)", result.deleted, max_age_hours),
                        )
                        .emit();
                }
                result.deleted
            }
            Err(e) => {
                self.logger.error("cleanup", "workflow cleanup failed").err(e.to_string()).emit();
                0
            }
        };
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["worktree", "prune"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        Ok(deleted)
    }

    async fn dispatch_ready_tasks(
        &mut self,
        root: &str,
        queue_drain_limit: usize,
        process_manager: Option<&mut ProcessManager>,
    ) -> Result<DispatchWorkflowStartSummary> {
        let Some(process_manager) = process_manager else {
            return Ok(DispatchWorkflowStartSummary::default());
        };
        // Queue-only dispatch model: the daemon executes ONLY what has been
        // explicitly enqueued, leasing up to the available agent-slot
        // capacity (`queue_drain_limit`, zeroed while the pool is draining).
        // It does NOT scan the subject backend for Ready tasks — moving a
        // task from a subject backend into the queue is the end user's
        // responsibility (an agent, a script, or a configured trigger calls
        // `animus queue enqueue`). Cron `schedules:` still dispatch via their
        // own leg.
        let summary = if queue_drain_limit > 0 {
            dispatch_queued_entries_via_runner(root, process_manager, queue_drain_limit).await?
        } else {
            DispatchWorkflowStartSummary::default()
        };

        Ok(summary)
    }

    fn dispatch_notice(&mut self, notice: DispatchNotice) {
        match notice {
            DispatchNotice::ScheduleDispatched { schedule_id, dispatch } => {
                self.logger.info("schedule", format!("fired '{}'", dispatch.workflow_ref)).schedule(schedule_id).emit();
            }
            DispatchNotice::ScheduleDispatchFailed { schedule_id, dispatch, error } => {
                self.logger
                    .error("schedule", format!("dispatch failed for '{}'", dispatch.workflow_ref))
                    .schedule(schedule_id)
                    .err(error)
                    .emit();
            }
            DispatchNotice::Failed { dispatch, error } => {
                self.logger
                    .error("process", format!("failed to start runner for {}", dispatch.subject_key()))
                    .err(error)
                    .emit();
            }
            DispatchNotice::Deferred { dispatch, reason } => {
                self.logger
                    .info("process", format!("deferred runner spawn for {} to next tick", dispatch.subject_key()))
                    .meta(serde_json::json!({"reason": reason}))
                    .emit();
            }
            DispatchNotice::Started { dispatch, .. } => {
                self.logger
                    .info("queue.dispatch", format!("dispatched {}", dispatch.subject_key()))
                    .subject(dispatch.subject_id())
                    .meta(serde_json::json!({"workflow_ref": dispatch.workflow_ref}))
                    .emit();
            }
        }
    }
}

/// Reconcile InProgress tasks against their workflow records.
///
/// Two cases:
///
/// 1. All workflows for the task are terminal with no success, AND the
///    latest terminal transition postdates the task's own last status
///    transition: the task is stale residue of a crashed/failed workflow
///    — block it. An operator who reset the task to InProgress AFTER the
///    workflows ended is working it manually (no new workflow row) and
///    must not be re-blocked every tick. The marker is
///    `metadata.status_changed_at` (only bumped by status applications,
///    not by ordinary field edits), falling back to `updated_at` for
///    records persisted before that field existed.
/// 2. Zero workflow records, no active runner process for the task, and
///    the task's last transition is older than the daemon's stale
///    in-progress threshold (`--stale-threshold-hours`, default 24h):
///    nothing will ever pick it up — reset it to Ready (not Blocked) so
///    dispatch can retry it. Human-assigned tasks are exempt: a person
///    may legitimately hold a claim without any workflow row, and there
///    is no post-hoc marker distinguishing a daemon dispatch whose runner
///    died pre-bootstrap from a manual claim.
pub(crate) async fn reconcile_stale_in_progress_tasks_for_hub(
    hub: Arc<dyn ServiceHub>,
    active_subject_ids: &std::collections::HashSet<String>,
    grace_secs: i64,
) -> Result<usize> {
    let tasks = hub.tasks().list().await?;
    let in_progress_tasks: Vec<_> = tasks.iter().filter(|t| t.status == TaskStatus::InProgress).collect();
    if in_progress_tasks.is_empty() {
        return Ok(0);
    }

    let workflows = hub.workflows().list().await?;
    let now = chrono::Utc::now();
    let mut reconciled = 0usize;
    for task in in_progress_tasks {
        let last_transition_at = task.metadata.status_changed_at.unwrap_or(task.metadata.updated_at);
        let task_workflows: Vec<_> = workflows.iter().filter(|w| w.task_id == task.id).collect();
        if task_workflows.is_empty() {
            if active_subject_ids.contains(&task.id) {
                continue;
            }
            if matches!(task.assignee, orchestrator_core::Assignee::Human { .. }) {
                continue;
            }
            if (now - last_transition_at).num_milliseconds() > grace_secs.saturating_mul(1000) {
                let _ = hub.tasks().set_status(&task.id, TaskStatus::Ready, false).await;
                reconciled += 1;
            }
            continue;
        }
        let all_terminal = task_workflows.iter().all(|w| {
            matches!(
                w.status,
                WorkflowStatus::Completed
                    | WorkflowStatus::Failed
                    | WorkflowStatus::Cancelled
                    | WorkflowStatus::Escalated
            )
        });
        if all_terminal {
            let any_success = task_workflows
                .iter()
                .any(|w| matches!(w.status, WorkflowStatus::Completed | WorkflowStatus::Escalated));
            // Only auto-transition to Blocked on failure. Task completion is never
            // automatic — only an agent or human should mark a task done after
            // verifying the work actually landed.
            if !any_success {
                // Pick the workflow with the latest terminal timestamp; its
                // terminal status decides the projection.
                let latest = task_workflows.iter().max_by_key(|w| w.completed_at.unwrap_or(w.started_at));
                let latest_terminal_at = latest.map(|w| w.completed_at.unwrap_or(w.started_at));
                if latest_terminal_at.is_some_and(|terminal_at| last_transition_at >= terminal_at) {
                    continue;
                }
                // A workflow that died Cancelled in a crash window must
                // project the task Cancelled, not Blocked. Reuse the shared
                // terminal projection so the mapping stays in one place.
                if latest.is_some_and(|w| w.status == WorkflowStatus::Cancelled) {
                    orchestrator_core::project_task_terminal_workflow_status(
                        hub.clone(),
                        &task.id,
                        WorkflowStatus::Cancelled,
                        None,
                    )
                    .await;
                } else {
                    let _ = hub.tasks().set_status(&task.id, TaskStatus::Blocked, false).await;
                }
                reconciled += 1;
            }
        }
    }
    Ok(reconciled)
}

pub(crate) type SlimProjectTickDriver<'a> = DefaultSlimProjectTickDriver<'a, CliProjectTickServices>;

pub(crate) fn slim_project_tick_driver<'a>(
    args: &DaemonRuntimeOptions,
    process_manager: &'a mut ProcessManager,
    logger: Arc<Logger>,
) -> SlimProjectTickDriver<'a> {
    default_slim_project_tick_driver(CliProjectTickServices::new(args, logger), process_manager)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::await_holding_lock)]

    use super::reconcile_stale_in_progress_tasks_for_hub;
    use crate::shared::test_env_lock;
    use orchestrator_core::{
        services::ServiceHub, FileServiceHub, Priority, TaskCreateInput, TaskStatus, TaskType, WorkflowRunInput,
        WorkflowStatus,
    };
    use protocol::test_utils::EnvVarGuard;
    use std::collections::HashSet;
    use std::process::Command as ProcessCommand;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn init_git_repo(temp: &TempDir) {
        let init = ProcessCommand::new("git")
            .args(["init", "-b", "main"])
            .current_dir(temp.path())
            .status()
            .expect("git init should run");
        assert!(init.success(), "git init should succeed");
        for args in [
            ["config", "user.email", "ao-test@example.com"].as_slice(),
            ["config", "user.name", "Animus Test"].as_slice(),
        ] {
            let status =
                ProcessCommand::new("git").args(args).current_dir(temp.path()).status().expect("git config should run");
            assert!(status.success(), "git config should succeed");
        }
        std::fs::write(temp.path().join("README.md"), "# test\n").expect("readme should be written");
        let add =
            ProcessCommand::new("git").args(["add", "README.md"]).current_dir(temp.path()).status().expect("git add");
        assert!(add.success(), "git add should succeed");
        let commit = ProcessCommand::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(temp.path())
            .status()
            .expect("git commit should run");
        assert!(commit.success(), "initial commit should succeed");
    }

    async fn hub_with_task(temp: &TempDir, title: &str) -> (Arc<dyn ServiceHub>, String) {
        init_git_repo(temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: title.to_string(),
                description: "stale in-progress reconciliation test".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        (hub, task.id)
    }

    #[tokio::test]
    async fn budget_kill_switch_skips_enforcement_leg_but_records_status() {
        use super::CliProjectTickServices;
        use crate::services::cost::DISABLE_BUDGET_ENFORCEMENT_ENV;
        use orchestrator_daemon_runtime::DefaultProjectTickServices;

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let state_root = temp.path().join("scope");
        std::fs::create_dir_all(&state_root).unwrap();
        let _override = EnvVarGuard::set("ANIMUS_COST_STATE_ROOT", Some(state_root.to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let root = project_root.to_string_lossy().to_string();

        let hub: Arc<dyn ServiceHub> = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let logger = Arc::new(orchestrator_logging::Logger::for_project(&project_root));
        let mut services = CliProjectTickServices { logger };

        let _off = EnvVarGuard::set(DISABLE_BUDGET_ENFORCEMENT_ENV, Some("1"));
        let events = services.enforce_budget_caps(hub, &root).await.expect("leg should not error");
        assert!(events.is_empty(), "kill-switch must skip the enforcement leg");

        let status = crate::services::cost::load_budget_enforcement_status(&project_root)
            .expect("status recorded even when skipped");
        assert!(!status.enabled, "recorded status reflects the kill-switch");
        drop(home);
    }

    #[tokio::test]
    async fn stale_in_progress_task_with_failed_workflow_is_still_blocked() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let (hub, task_id) = hub_with_task(&temp, "crashed workflow residue").await;
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        hub.tasks().set_status(&task_id, TaskStatus::InProgress, false).await.expect("task should be in progress");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task_id.clone(), None), None)
            .await
            .expect("workflow should start");
        // Drive the workflow to a terminal Failed state (distinct from
        // Cancelled, which now projects the task Cancelled). A fresh run is
        // Running; fail_current_phase records the phase failure and, on a
        // single-phase default plan with no retries left, lands the workflow
        // in a terminal non-success state.
        hub.workflows().fail_current_phase(&workflow.id, "boom".to_string()).await.expect("workflow phase should fail");
        // Cancel as a deterministic terminal fallback if the failure
        // transition retried instead of terminating — but assert below the
        // status is NOT Cancelled so we genuinely exercise the Blocked path.
        let wf_after = hub.workflows().get(&workflow.id).await.expect("workflow reload");
        assert_ne!(
            wf_after.status,
            WorkflowStatus::Cancelled,
            "this fixture must exercise a non-Cancelled terminal workflow"
        );

        // An ordinary field edit after the crash bumps `updated_at` but is
        // NOT a status transition — it must not shield the task from
        // reconciliation.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        hub.tasks()
            .update(
                &task_id,
                orchestrator_core::TaskUpdateInput {
                    title: None,
                    description: None,
                    priority: Some(Priority::High),
                    status: None,
                    assignee: None,
                    tags: None,
                    updated_by: None,
                    deadline: None,
                    linked_architecture_entities: None,
                },
            )
            .await
            .expect("task field edit should apply");
        // A no-op re-application of the current status is not a transition
        // either.
        hub.tasks().set_status(&task_id, TaskStatus::InProgress, false).await.expect("no-op status write should apply");

        let reconciled = reconcile_stale_in_progress_tasks_for_hub(hub.clone(), &HashSet::new(), 90)
            .await
            .expect("reconciliation should run");
        assert_eq!(reconciled, 1, "task whose only workflow ended after its last transition must be reconciled");
        let task = hub.tasks().get(&task_id).await.expect("task should reload");
        assert_eq!(task.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn stale_in_progress_task_with_cancelled_workflow_is_cancelled() {
        // Crash-window fixture: the workflow died Cancelled while the daemon
        // was down, so the task is still InProgress. The reconcile sweep must
        // project the task Cancelled (mirroring the terminal projection), not
        // Blocked.
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let (hub, task_id) = hub_with_task(&temp, "cancelled workflow residue").await;
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        hub.tasks().set_status(&task_id, TaskStatus::InProgress, false).await.expect("task should be in progress");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task_id.clone(), None), None)
            .await
            .expect("workflow should start");
        hub.workflows().cancel(&workflow.id).await.expect("workflow should cancel");

        let reconciled = reconcile_stale_in_progress_tasks_for_hub(hub.clone(), &HashSet::new(), 90)
            .await
            .expect("reconciliation should run");
        assert_eq!(reconciled, 1, "task whose only workflow died Cancelled must be reconciled");
        let task = hub.tasks().get(&task_id).await.expect("task should reload");
        assert_eq!(task.status, TaskStatus::Cancelled, "Cancelled workflow projects task Cancelled, not Blocked");
    }

    #[tokio::test]
    async fn manually_reset_in_progress_task_is_not_reblocked() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let (hub, task_id) = hub_with_task(&temp, "manually worked task").await;
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        hub.tasks().set_status(&task_id, TaskStatus::InProgress, false).await.expect("task should be in progress");
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task_id.clone(), None), None)
            .await
            .expect("workflow should start");
        hub.workflows().cancel(&workflow.id).await.expect("workflow should cancel");

        // Operator resets the task and works it manually — no new workflow
        // row. The task's own transition now postdates the terminal
        // workflow, so the reconciler must leave it alone on every tick.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        hub.tasks().set_status(&task_id, TaskStatus::Ready, false).await.expect("task should reset");
        hub.tasks().set_status(&task_id, TaskStatus::InProgress, false).await.expect("task should be in progress");

        for _ in 0..2 {
            let reconciled = reconcile_stale_in_progress_tasks_for_hub(hub.clone(), &HashSet::new(), 90)
                .await
                .expect("reconciliation should run");
            assert_eq!(reconciled, 0, "manually reset task must not be re-blocked");
            let task = hub.tasks().get(&task_id).await.expect("task should reload");
            assert_eq!(task.status, TaskStatus::InProgress);
        }
    }

    #[tokio::test]
    async fn in_progress_task_with_no_workflows_resets_to_ready_after_grace() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let (hub, task_id) = hub_with_task(&temp, "abandoned in-progress task").await;

        hub.tasks().set_status(&task_id, TaskStatus::InProgress, false).await.expect("task should be in progress");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let reconciled = reconcile_stale_in_progress_tasks_for_hub(hub.clone(), &HashSet::new(), 0)
            .await
            .expect("reconciliation should run");
        assert_eq!(reconciled, 1, "abandoned task beyond the grace age must be reconciled");
        let task = hub.tasks().get(&task_id).await.expect("task should reload");
        assert_eq!(task.status, TaskStatus::Ready, "abandoned task is reset to Ready, not Blocked");
    }

    #[tokio::test]
    async fn in_progress_task_with_no_workflows_is_left_alone_within_grace_or_while_runner_active() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let (hub, task_id) = hub_with_task(&temp, "freshly dispatched task").await;

        hub.tasks().set_status(&task_id, TaskStatus::InProgress, false).await.expect("task should be in progress");

        // Within the grace window: untouched.
        let reconciled = reconcile_stale_in_progress_tasks_for_hub(hub.clone(), &HashSet::new(), 3600)
            .await
            .expect("reconciliation should run");
        assert_eq!(reconciled, 0);
        assert_eq!(hub.tasks().get(&task_id).await.expect("task should reload").status, TaskStatus::InProgress);

        // Beyond the grace window but with a live runner process for the
        // subject (record not bootstrapped yet): untouched.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let active: HashSet<String> = HashSet::from([task_id.clone()]);
        let reconciled = reconcile_stale_in_progress_tasks_for_hub(hub.clone(), &active, 0)
            .await
            .expect("reconciliation should run");
        assert_eq!(reconciled, 0);
        assert_eq!(hub.tasks().get(&task_id).await.expect("task should reload").status, TaskStatus::InProgress);

        // Human-assigned claims are never auto-reset, even beyond the
        // grace: a person may legitimately work a task with no workflow
        // row.
        hub.tasks()
            .assign_human(&task_id, "operator".to_string(), "test".to_string())
            .await
            .expect("task should be human-assigned");
        let reconciled = reconcile_stale_in_progress_tasks_for_hub(hub.clone(), &HashSet::new(), 0)
            .await
            .expect("reconciliation should run");
        assert_eq!(reconciled, 0);
        assert_eq!(hub.tasks().get(&task_id).await.expect("task should reload").status, TaskStatus::InProgress);
    }
}
