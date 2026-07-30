use super::*;
use crate::services::runtime::execution_fact_projection::reconcile_completed_processes;
use crate::services::runtime::runtime_daemon::daemon_reconciliation::{
    journal_resume_enabled, reconcile_manual_phase_timeouts, recover_orphaned_running_workflows,
    resumable_orphans_for_redispatch,
};
use anyhow::{anyhow, Result};
use orchestrator_core::services::ServiceHub;
use orchestrator_core::{Assignee, TaskStatus, WorkflowStateManager, WorkflowStatus};
use orchestrator_daemon_runtime::{
    default_slim_project_tick_driver, resolve_subject_dispatch, BudgetBreachEvent, CompletedProcess,
    CompletedProcessReconciliation, DefaultProjectTickServices, DefaultSlimProjectTickDriver, DispatchNotice,
    DispatchWorkflowStartSummary, ProcessManager, ProjectTickSnapshot, SubjectPluginDispatch,
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

    /// BU-4: spawn a fresh `workflow_runner` for up to `limit` in-flight runs
    /// the orphan sweep preserved (durable journal active). Each runner
    /// re-enters the run at its persisted `current_phase` (phase-boundary
    /// resume). The relayed actor preserves owner scoping across the restart.
    /// Returns the number of runners spawned (resumed runs), so the caller can
    /// subtract it from the queue-drain capacity.
    async fn redispatch_resumable_orphans(
        &mut self,
        root: &str,
        process_manager: &mut ProcessManager,
        limit: usize,
    ) -> usize {
        let hub: Arc<dyn ServiceHub> = match orchestrator_core::FileServiceHub::new(root) {
            Ok(hub) => Arc::new(hub),
            Err(error) => {
                self.logger
                    .warn("reconciliation", format!("journal-resume re-dispatch skipped: hub init failed: {error}"))
                    .emit();
                return 0;
            }
        };
        let active_subject_ids = process_manager.active_subject_ids();
        // Steady-state: shield live delegated (remote) runs from local re-dispatch.
        let candidates = resumable_orphans_for_redispatch(hub, root, &active_subject_ids, true).await;
        if candidates.is_empty() {
            return 0;
        }
        let state_manager = WorkflowStateManager::new(root);
        let mut started = 0usize;
        // Within-tick dedupe (codex P2): the candidate set is computed from a
        // single pre-loop `active_subject_ids` snapshot, so two Running records
        // for the SAME subject could both be returned. The ProcessManager only
        // reflects the first spawn AFTER it happens, so guard here too — never
        // drive one subject with two concurrent runners in a single tick.
        let mut dispatched_subjects: std::collections::HashSet<String> = std::collections::HashSet::new();
        for workflow in candidates {
            if started >= limit {
                break;
            }
            // Subjectless runs carry no subject to dedup on — each is distinct.
            if let Some(subject) = workflow.subject.as_ref() {
                if !dispatched_subjects.insert(subject.id().to_string()) {
                    continue;
                }
            }
            let workflow_ref = workflow.workflow_ref.clone().unwrap_or_else(|| "standard".to_string());
            // Relay the owner actor the run bootstrapped with so the restarted
            // run scopes identically; `None` => system/global run.
            let actor = state_manager.load_workflow_actor(&workflow.id);
            let dispatch = match workflow.subject.clone() {
                Some(subject) => orchestrator_daemon_runtime::SubjectDispatch::for_subject_with_metadata(
                    subject,
                    workflow_ref,
                    "journal-resume",
                    chrono::Utc::now(),
                ),
                None => orchestrator_daemon_runtime::SubjectDispatch::subjectless(
                    workflow_ref,
                    "journal-resume",
                    chrono::Utc::now(),
                ),
            }
            .with_input(workflow.input.clone())
            .with_vars(workflow.vars.clone())
            .with_actor(actor);

            // Target the EXISTING persisted run by id so the runner continues
            // it from `current_phase` (phase-boundary resume) instead of
            // starting a duplicate workflow for the subject.
            match process_manager.spawn_workflow_runner_resume(
                &dispatch,
                root,
                &workflow.id,
                orchestrator_daemon_runtime::workflow_current_phase_id(&workflow).as_deref(),
            ) {
                Ok(()) => {
                    started += 1;
                    self.logger
                        .info(
                            "reconciliation",
                            format!(
                                "journal-resume: re-dispatched in-flight workflow {} (subject {}) from phase boundary {}",
                                workflow.id,
                                workflow.subject.as_ref().map(|s| s.id()).unwrap_or_default(),
                                workflow.current_phase.as_deref().unwrap_or("<index>")
                            ),
                        )
                        .emit();
                }
                Err(error) => {
                    // Concurrency cap reached: stop — the remaining candidates
                    // are picked up on a later tick (still preserved, never
                    // cancelled). Any other spawn error is logged and the run
                    // stays preserved for the next attempt.
                    if error.downcast_ref::<orchestrator_daemon_runtime::WorkflowConcurrencyCapReached>().is_some() {
                        break;
                    }
                    self.logger
                        .warn(
                            "reconciliation",
                            format!("journal-resume re-dispatch of workflow {} failed: {error}", workflow.id),
                        )
                        .emit();
                }
            }
        }
        started
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
        // BU-4: when the durable journal is active, the zombie sweep PRESERVES
        // resumable orphans (re-dispatched from their phase boundary by
        // `dispatch_ready_tasks` below) instead of cancelling them. On the
        // SQLite path `journal_resume_enabled` is false and this is the
        // byte-identical pre-BU-4 cancel behavior.
        let resume_orphans = journal_resume_enabled(root);
        // Steady-state: the remote delegate is alive on its node, so skip live
        // delegated runs (see `recover_orphaned_running_workflows`).
        Ok(recover_orphaned_running_workflows(hub, root, active_subject_ids, resume_orphans, true).await)
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
        root: &str,
        active_subject_ids: &std::collections::HashSet<String>,
        stale_threshold_hours: u64,
    ) -> Result<usize> {
        let grace_secs = i64::try_from(stale_threshold_hours.saturating_mul(3600)).unwrap_or(i64::MAX);
        // Prefer the installed subject_backend plugin — the same store `subject
        // get/list/status` and the enqueue-by-id fix (TASK-215) route through —
        // so plugin-backed in-progress subjects are reconciled. On a
        // plugin-backed deployment (e.g. animus-postgres on the portal) the
        // legacy in-tree task store is empty, so the pre-routing sweep saw none
        // of them. Falls back to the in-tree store for the stock scaffold / when
        // no subject plugin owns `task`.
        let store = resolve_stale_task_store(root, hub.clone()).await;
        match reconcile_stale_in_progress_tasks_with_store(store.as_ref(), hub, active_subject_ids, grace_secs).await {
            Ok(reconciled) => Ok(reconciled),
            Err(error) => {
                // A subject_backend list/status failure (e.g. a transient plugin
                // timeout) must NOT abort the housekeeping tick — the in-tree
                // sweep this replaces could never make an installed subject
                // plugin block the later dispatch legs. Log and treat as "no
                // reconciliations this tick"; the next heartbeat retries.
                self.logger.warn("reconciliation", format!("stale in-progress reconciliation skipped: {error}")).emit();
                Ok(0)
            }
        }
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
        // Suppressed entirely while the pool is draining (`queue_drain_limit
        // == 0`): no resume re-dispatch, no queue lease.
        if queue_drain_limit == 0 {
            return Ok(DispatchWorkflowStartSummary::default());
        }

        // BU-4 journal-resume re-dispatch leg: when the durable journal is
        // active, restart in-flight runs the orphan sweep PRESERVED (rather
        // than cancelled) from their current phase boundary. Run this BEFORE
        // the queue lease (codex P2): the queue lease only excludes
        // ProcessManager-active subjects, so a pending queue entry for the
        // SAME subject as a preserved run would otherwise start first, strand
        // the preserved run, and duplicate the subject's work. Spawning the
        // resume first marks the subject active, so the lease's
        // `exclude_subjects` filter skips it. Bounded by `queue_drain_limit`
        // so it never exceeds pool sizing. Idempotent: a re-dispatched run
        // registers a live runner + agent record, excluding it on later ticks.
        let resumed = self.redispatch_resumable_orphans(root, process_manager, queue_drain_limit).await;

        // Queue-only dispatch model: the daemon executes ONLY what has been
        // explicitly enqueued, leasing up to the agent-slot capacity left
        // after resumes. It does NOT scan the subject backend for Ready tasks —
        // moving a task into the queue is the end user's responsibility (an
        // agent, a script, or a configured trigger calls `animus queue
        // enqueue`). Cron `schedules:` still dispatch via their own leg.
        let remaining = queue_drain_limit.saturating_sub(resumed);
        let summary = if remaining > 0 {
            dispatch_queued_entries_via_runner(root, process_manager, remaining).await?
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
                    .error(
                        "process",
                        format!("failed to start runner for {}", dispatch.subject_key().unwrap_or_default()),
                    )
                    .err(error)
                    .emit();
            }
            DispatchNotice::Deferred { dispatch, reason } => {
                self.logger
                    .info(
                        "process",
                        format!(
                            "deferred runner spawn for {} to next tick",
                            dispatch.subject_key().unwrap_or_default()
                        ),
                    )
                    .meta(serde_json::json!({"reason": reason}))
                    .emit();
            }
            DispatchNotice::Started { dispatch, .. } => {
                self.logger
                    .info("queue.dispatch", format!("dispatched {}", dispatch.subject_key().unwrap_or_default()))
                    .subject(dispatch.subject_id().unwrap_or_default())
                    .meta(serde_json::json!({"workflow_ref": dispatch.workflow_ref}))
                    .emit();
            }
        }
    }
}

/// In-tree hub entry point retained for the daemon's existing hub-based unit
/// tests, which drive the reconcile logic against a populated in-tree task
/// store. Production selects the store via [`resolve_stale_task_store`] so
/// plugin-backed subjects are reconciled too, so this wrapper is test-only.
#[cfg(test)]
pub(crate) async fn reconcile_stale_in_progress_tasks_for_hub(
    hub: Arc<dyn ServiceHub>,
    active_subject_ids: &std::collections::HashSet<String>,
    grace_secs: i64,
) -> Result<usize> {
    let store = HubStaleTaskStore::new(hub.clone());
    reconcile_stale_in_progress_tasks_with_store(&store, hub, active_subject_ids, grace_secs).await
}

/// Reconcile InProgress tasks sourced from `store` (the in-tree task store OR
/// the installed subject_backend plugin) against their in-tree workflow records.
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
///
/// The task read/write surface is abstracted behind [`StaleTaskStore`] so the
/// daemon can source in-progress subjects from the plugin the rest of the
/// subject surface uses — the in-tree store is empty on a plugin-backed
/// deployment, so the pre-routing sweep reconciled nothing there. Workflow
/// records (the terminal-status cross-reference) still come from the in-tree
/// [`WorkflowStateManager`] via `hub`, which the daemon populates locally on
/// every deployment.
pub(crate) async fn reconcile_stale_in_progress_tasks_with_store(
    store: &dyn StaleTaskStore,
    hub: Arc<dyn ServiceHub>,
    active_subject_ids: &std::collections::HashSet<String>,
    grace_secs: i64,
) -> Result<usize> {
    let in_progress_tasks = store.list_in_progress().await?;
    if in_progress_tasks.is_empty() {
        return Ok(0);
    }

    // No-blob summaries, NOT the full runs: this sweep only needs subject id +
    // status + terminal timestamp to cross-reference in-progress tasks. Fetching
    // every run's opaque blob here (`list()`) was a ~6s all-runs scan that ran
    // every heartbeat and head-of-line-blocked the shared journal host.
    let workflows = hub.workflows().list_summaries().await?;
    let now = chrono::Utc::now();
    let mut reconciled = 0usize;
    for task in &in_progress_tasks {
        let last_transition_at = task.last_transition_at;
        // A plugin id is kind-qualified (`task:TASK-1`); the in-tree workflow
        // record may key on the bare native id. Match on either form so the
        // cross-reference works for both stores (bare-vs-bare is unchanged).
        let task_workflows: Vec<_> = workflows.iter().filter(|w| task_ids_match(&w.task_id, &task.id)).collect();
        if task_workflows.is_empty() {
            // Skip a subject with a live runner. Normalize both sides so a
            // qualified plugin id (`task:TASK-1`) matches a bare active id (and
            // vice versa); bare-vs-bare stays an exact match. Without this a
            // plugin-backed task whose runner has not written a workflow row yet
            // could be reset to Ready out from under its live runner.
            if active_subject_ids.iter().any(|active| task_ids_match(active, &task.id)) {
                continue;
            }
            if task.is_human_assignee {
                continue;
            }
            if (now - last_transition_at).num_milliseconds() > grace_secs.saturating_mul(1000) {
                // Propagate a write failure (the caller logs + skips the leg) so
                // a plugin timeout/rejection is visible and the subject is
                // retried next tick, and only count a write that actually
                // landed — never report a reconciliation that did not happen.
                store.set_status(&task.id, TaskStatus::Ready).await?;
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
                // A workflow that died Cancelled in a crash window must project
                // the task Cancelled, not Blocked. The subject is InProgress here
                // (filtered above), so a bare `set_status(Cancelled)` matches the
                // shared terminal projection's behaviour for a non-terminal task.
                let next_status = if latest.is_some_and(|w| w.status == WorkflowStatus::Cancelled) {
                    TaskStatus::Cancelled
                } else {
                    TaskStatus::Blocked
                };
                // As above: surface a write failure and count only a landed write.
                store.set_status(&task.id, next_status).await?;
                reconciled += 1;
            }
        }
    }
    Ok(reconciled)
}

/// Two task ids match when equal, or equal after unwrapping ONLY a `task:`
/// qualifier from either side. Unwrapping just the `task:` prefix (not any
/// `<kind>:`) keeps this task-scoped: an unrelated qualified subject such as
/// `blog:BLOG-1` must not alias `task:BLOG-1` — otherwise a live non-task runner
/// could shield a stale task from reconciliation, or a foreign workflow row
/// could be mis-attributed to it.
fn task_ids_match(a: &str, b: &str) -> bool {
    a == b || crate::bare_task_id(a) == crate::bare_task_id(b)
}

/// Minimal projection of an in-progress task the stale-reconciler needs,
/// sourced either from the in-tree task store or a subject_backend plugin.
#[derive(Debug, Clone)]
pub(crate) struct StaleInProgressTask {
    /// Backend-qualified (plugin) or bare (in-tree) subject id.
    pub id: String,
    /// A human holds this subject. A person may legitimately claim a task with
    /// no workflow row, so it is exempt from the reset-to-Ready sweep.
    pub is_human_assignee: bool,
    /// Best available last-status-transition timestamp: the in-tree store uses
    /// `metadata.status_changed_at` (falling back to `updated_at`); the plugin
    /// wire carries only `updated_at`.
    pub last_transition_at: chrono::DateTime<chrono::Utc>,
}

/// Task read/write surface the stale-in-progress reconciler drives. Split out
/// so the daemon tick can source in-progress subjects from the installed
/// `subject_backend` plugin (the store the rest of the subject surface uses)
/// instead of the legacy in-tree task store, which is empty on a plugin-backed
/// deployment. A stub impl keeps the reconcile logic unit-testable.
#[async_trait::async_trait]
pub(crate) trait StaleTaskStore: Send + Sync {
    /// In-progress tasks, in no particular order.
    async fn list_in_progress(&self) -> Result<Vec<StaleInProgressTask>>;
    /// Apply a reconciled ready/terminal status to a task by id.
    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<()>;
}

/// In-tree task-store view. Behaviour identical to the pre-routing reconciler;
/// used by the stock scaffold (no subject plugin owning `task`) and the daemon's
/// existing hub-based unit tests.
pub(crate) struct HubStaleTaskStore {
    hub: Arc<dyn ServiceHub>,
}

impl HubStaleTaskStore {
    pub(crate) fn new(hub: Arc<dyn ServiceHub>) -> Self {
        Self { hub }
    }
}

#[async_trait::async_trait]
impl StaleTaskStore for HubStaleTaskStore {
    async fn list_in_progress(&self) -> Result<Vec<StaleInProgressTask>> {
        let tasks = self.hub.tasks().list().await?;
        Ok(tasks
            .into_iter()
            .filter(|t| t.status == TaskStatus::InProgress)
            .map(|t| StaleInProgressTask {
                is_human_assignee: matches!(t.assignee, Assignee::Human { .. }),
                last_transition_at: t.metadata.status_changed_at.unwrap_or(t.metadata.updated_at),
                id: t.id,
            })
            .collect())
    }

    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        self.hub.tasks().set_status(id, status, false).await.map(|_| ())
    }
}

/// Subject-router view: sources in-progress tasks from the installed
/// `subject_backend` plugin (the same store `subject get/list/status` and the
/// TASK-215 enqueue-by-id fix route through), so plugin-backed subjects the
/// empty in-tree store never sees are reconciled.
pub(crate) struct RouterStaleTaskStore {
    dispatch: SubjectPluginDispatch,
}

impl RouterStaleTaskStore {
    pub(crate) fn new(dispatch: SubjectPluginDispatch) -> Self {
        Self { dispatch }
    }

    /// `true` when a subject_backend plugin can serve the built-in `task` kind,
    /// so routing the reconciler through it is meaningful. Accepts an explicit
    /// `task` kind or a catch-all (`*`) backend, which `task/list` routes to
    /// as well.
    fn routes_tasks(dispatch: &SubjectPluginDispatch) -> bool {
        dispatch.is_active() && dispatch.kinds().iter().any(|kind| kind == "task" || kind == "*")
    }
}

#[async_trait::async_trait]
impl StaleTaskStore for RouterStaleTaskStore {
    async fn list_in_progress(&self) -> Result<Vec<StaleInProgressTask>> {
        // Page through every `task/list` response so a cursor-paginating (or
        // limit-clamping) backend does not hide stale in-progress subjects
        // beyond the first page. Bounded so a backend that never clears
        // `next_cursor` cannot loop the housekeeping leg forever. The status
        // filter is applied server-side and re-checked client-side.
        let mut tasks: Vec<StaleInProgressTask> = Vec::new();
        let mut cursor: Option<serde_json::Value> = None;
        for _ in 0..MAX_SUBJECT_LIST_PAGES {
            let mut params = serde_json::Map::new();
            params.insert("kind".to_string(), serde_json::json!(["task"]));
            params.insert("status".to_string(), serde_json::json!(["in-progress"]));
            if let Some(cursor) = cursor.take() {
                params.insert("cursor".to_string(), cursor);
            }
            let raw = self
                .dispatch
                .route_call("task/list", Some(serde_json::Value::Object(params)))
                .await
                .map_err(|err| anyhow!("subject backend task/list failed ({}): {}", err.code, err.message))?;
            tasks.extend(extract_in_progress_tasks(&raw));
            match extract_next_cursor(&raw) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(tasks)
    }

    async fn set_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        let params = serde_json::json!({ "id": id, "status": wire_status(status) });
        self.dispatch
            .route_call("task/status", Some(params))
            .await
            .map(|_| ())
            .map_err(|err| anyhow!("subject backend task/status failed ({}): {}", err.code, err.message))
    }
}

/// Choose the task store the stale-reconciler reads/writes through: the subject
/// router when a `subject_backend` plugin owns `task` (production / portal),
/// else the in-tree store (stock scaffold / no plugins). A routing-discovery
/// failure falls back to the in-tree store so a transient plugin problem never
/// takes the housekeeping leg down.
async fn resolve_stale_task_store(root: &str, hub: Arc<dyn ServiceHub>) -> Box<dyn StaleTaskStore> {
    match resolve_subject_dispatch(std::path::Path::new(root)).await {
        Ok(resolution) if RouterStaleTaskStore::routes_tasks(&resolution.selected) => {
            Box::new(RouterStaleTaskStore::new(resolution.selected))
        }
        _ => Box::new(HubStaleTaskStore::new(hub)),
    }
}

/// Wire status token the subject_backend plugin expects. `TaskStatus` already
/// serializes kebab-case, matching the `SubjectStatus` vocabulary for the
/// states the reconciler writes (`ready` / `blocked` / `cancelled`).
fn wire_status(status: TaskStatus) -> String {
    serde_json::to_value(status).ok().and_then(|value| value.as_str().map(str::to_string)).unwrap_or_default()
}

/// Upper bound on `task/list` pages [`RouterStaleTaskStore::list_in_progress`]
/// follows. At the default backend page size this covers very large stores
/// while guaranteeing the housekeeping leg never hangs on a backend that
/// returns a non-empty `next_cursor` indefinitely. Mirrors the status
/// dashboard's paging bound.
const MAX_SUBJECT_LIST_PAGES: usize = 1_000;

/// Pull a non-empty pagination cursor out of a `task/list` response. `None`
/// when the backend omits `next_cursor`, sets it to null, or returns an empty
/// string — any of which signals the final page.
fn extract_next_cursor(result: &serde_json::Value) -> Option<serde_json::Value> {
    match result.as_object()?.get("next_cursor")? {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) if s.trim().is_empty() => None,
        other => Some(other.clone()),
    }
}

/// Project a `task/list` response into the fields the reconciler needs,
/// tolerating the common envelope wrappers (`subjects` / `items` / `tasks` /
/// `results`) or a bare top-level array.
fn extract_in_progress_tasks(result: &serde_json::Value) -> Vec<StaleInProgressTask> {
    let subjects = if let Some(array) = result.as_array() {
        array.clone()
    } else if let Some(map) = result.as_object() {
        ["subjects", "items", "tasks", "results"]
            .iter()
            .find_map(|key| map.get(*key).and_then(|value| value.as_array()).cloned())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    subjects.iter().filter_map(subject_value_to_stale_task).collect()
}

/// Convert one wire subject object into a [`StaleInProgressTask`]. Re-checks the
/// status client-side (normalized so `in_progress` / casing variants are not
/// dropped) so a backend that ignores the `status` filter cannot make the
/// reconciler act on a subject that is not in-progress.
fn subject_value_to_stale_task(subject: &serde_json::Value) -> Option<StaleInProgressTask> {
    let id = subject.get("id").and_then(|value| value.as_str())?.to_string();
    // Only reconcile task-kind rows: a catch-all (`*`) or lax backend may return
    // other kinds from `task/list`, and this leg mutates via `task/status` — a
    // non-task row (e.g. `blog:BLOG-1`) must never be swept.
    let kind = subject.get("kind").and_then(|value| value.as_str()).unwrap_or_default();
    let is_task = kind == "task" || kind == orchestrator_core::SUBJECT_KIND_TASK || id.starts_with("task:");
    if !is_task {
        return None;
    }
    let status = subject.get("status").and_then(|value| value.as_str()).unwrap_or_default();
    if normalize_status(status) != "in-progress" {
        return None;
    }
    // Wire subjects carry no Agent/Human tag: an Animus agent claim is
    // conventionally `agent:<name>`. Treat any other non-empty assignee as a
    // human hold so a person's manual claim is not reset out from under them.
    // Read the top-level `assignee` first, falling back to `custom.assignee`
    // for backends that persist the assignee in custom data — resetting a
    // human's claim is destructive, so bias toward preserving it.
    let assignee = subject
        .get("assignee")
        .and_then(|value| value.as_str())
        .or_else(|| subject.pointer("/custom/assignee").and_then(|value| value.as_str()));
    let is_human_assignee =
        assignee.map(|assignee| !assignee.trim().is_empty() && !assignee.starts_with("agent:")).unwrap_or(false);
    let last_transition_at = subject
        .get("updated_at")
        .and_then(|value| value.as_str())
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now);
    Some(StaleInProgressTask { id, is_human_assignee, last_transition_at })
}

/// Fold a backend status string to its canonical spelling: lowercase with `_`
/// collapsed to `-`, so `in_progress` / `In-Progress` all compare equal to the
/// canonical `in-progress`. Mirrors the status dashboard's normalization.
fn normalize_status(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace('_', "-")
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

    /// Stub [`StaleTaskStore`] standing in for a subject_backend plugin: it
    /// returns a fixed in-progress set and records every status write, so the
    /// reconcile logic is exercised without spawning a plugin or touching the
    /// (empty) in-tree store.
    struct StubStaleTaskStore {
        tasks: Vec<super::StaleInProgressTask>,
        status_writes: std::sync::Mutex<Vec<(String, TaskStatus)>>,
        fail_writes: bool,
    }

    impl StubStaleTaskStore {
        fn new(tasks: Vec<super::StaleInProgressTask>) -> Self {
            Self { tasks, status_writes: std::sync::Mutex::new(Vec::new()), fail_writes: false }
        }

        /// A store whose `set_status` always fails, standing in for a plugin
        /// timeout/rejection.
        fn failing(tasks: Vec<super::StaleInProgressTask>) -> Self {
            Self { tasks, status_writes: std::sync::Mutex::new(Vec::new()), fail_writes: true }
        }

        fn writes(&self) -> Vec<(String, TaskStatus)> {
            self.status_writes.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }
    }

    #[async_trait::async_trait]
    impl super::StaleTaskStore for StubStaleTaskStore {
        async fn list_in_progress(&self) -> super::Result<Vec<super::StaleInProgressTask>> {
            Ok(self.tasks.clone())
        }

        async fn set_status(&self, id: &str, status: TaskStatus) -> super::Result<()> {
            if self.fail_writes {
                return Err(anyhow::anyhow!("simulated subject backend task/status failure"));
            }
            self.status_writes.lock().unwrap_or_else(|p| p.into_inner()).push((id.to_string(), status));
            Ok(())
        }
    }

    // Core of the fix: a plugin-backed, kind-qualified in-progress subject that
    // the empty in-tree store would never surface is reconciled — the stale
    // sweep resets it to Ready through the SAME store it was read from. Empty
    // in-tree workflow records (no run for the subject) drive the
    // reset-to-Ready leg.
    #[tokio::test]
    async fn plugin_backed_stale_in_progress_task_is_reset_to_ready_via_store() {
        let hub: Arc<dyn ServiceHub> = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let store = StubStaleTaskStore::new(vec![super::StaleInProgressTask {
            id: "task:TASK-900".to_string(),
            is_human_assignee: false,
            last_transition_at: chrono::Utc::now() - chrono::Duration::hours(48),
        }]);

        let reconciled = super::reconcile_stale_in_progress_tasks_with_store(&store, hub, &HashSet::new(), 24 * 3600)
            .await
            .expect("reconciliation should run");

        assert_eq!(reconciled, 1, "the plugin-backed stale subject is reconciled");
        assert_eq!(
            store.writes(),
            vec![("task:TASK-900".to_string(), TaskStatus::Ready)],
            "the reconciled subject is reset to Ready through the plugin store"
        );
    }

    // A human-held plugin-backed subject is exempt from the reset sweep, even
    // when stale past the grace window (mirrors the in-tree human-assignee
    // exemption).
    #[tokio::test]
    async fn plugin_backed_human_held_task_is_not_reset() {
        let hub: Arc<dyn ServiceHub> = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let store = StubStaleTaskStore::new(vec![super::StaleInProgressTask {
            id: "task:TASK-901".to_string(),
            is_human_assignee: true,
            last_transition_at: chrono::Utc::now() - chrono::Duration::hours(48),
        }]);

        let reconciled = super::reconcile_stale_in_progress_tasks_with_store(&store, hub, &HashSet::new(), 24 * 3600)
            .await
            .expect("reconciliation should run");

        assert_eq!(reconciled, 0, "a human-held subject is never auto-reset");
        assert!(store.writes().is_empty(), "no status write for a human-held subject");
    }

    // A failed status write (plugin timeout/rejection) must surface as an error
    // — not be swallowed while still counting a reconciliation that never
    // landed. The daemon's outer handler downgrades this to a logged skip.
    #[tokio::test]
    async fn plugin_status_write_failure_propagates_and_is_not_counted() {
        let hub: Arc<dyn ServiceHub> = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let store = StubStaleTaskStore::failing(vec![super::StaleInProgressTask {
            id: "task:TASK-902".to_string(),
            is_human_assignee: false,
            last_transition_at: chrono::Utc::now() - chrono::Duration::hours(48),
        }]);

        let err = super::reconcile_stale_in_progress_tasks_with_store(&store, hub, &HashSet::new(), 24 * 3600)
            .await
            .expect_err("a failed status write must propagate");
        assert!(err.to_string().contains("task/status failure"), "surfaced error identifies the failed write: {err}");
    }

    // A live runner shields a plugin-backed subject from the reset sweep even
    // when the active-process set keys on the BARE id while the plugin lists the
    // qualified form — otherwise the reconciler could reset a task out from
    // under its running workflow, allowing duplicate dispatch.
    #[tokio::test]
    async fn plugin_backed_task_with_active_bare_runner_is_not_reset() {
        let hub: Arc<dyn ServiceHub> = Arc::new(orchestrator_core::InMemoryServiceHub::new());
        let store = StubStaleTaskStore::new(vec![super::StaleInProgressTask {
            id: "task:TASK-1".to_string(),
            is_human_assignee: false,
            last_transition_at: chrono::Utc::now() - chrono::Duration::hours(48),
        }]);
        // Active set carries the bare native id; the plugin lists the qualified
        // form. The normalized membership test must still match.
        let active: HashSet<String> = HashSet::from(["TASK-1".to_string()]);

        let reconciled = super::reconcile_stale_in_progress_tasks_with_store(&store, hub, &active, 24 * 3600)
            .await
            .expect("reconciliation should run");

        assert_eq!(reconciled, 0, "a live runner (bare id) shields the qualified plugin subject");
        assert!(store.writes().is_empty(), "no status write while a runner is active");
    }

    // The router `task/list` parser projects only in-progress subjects, keys on
    // the backend-qualified id, reads `updated_at` as the last-transition
    // timestamp, and classifies an `agent:` assignee as non-human (eligible)
    // while any other assignee is a human hold (exempt).
    #[test]
    fn extract_in_progress_tasks_parses_wire_list_shape() {
        let payload = serde_json::json!({
            "subjects": [
                {
                    "id": "task:TASK-1",
                    "kind": "task",
                    "title": "agent-held in-progress",
                    "status": "in-progress",
                    "assignee": "agent:builder",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-02-01T12:00:00Z"
                },
                {
                    "id": "task:TASK-2",
                    "kind": "task",
                    "title": "human-held in-progress",
                    "status": "in-progress",
                    "assignee": "alice",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-02-02T00:00:00Z"
                },
                {
                    "id": "task:TASK-4",
                    "kind": "task",
                    "title": "human-held via custom.assignee",
                    "status": "in-progress",
                    "custom": { "assignee": "bob" },
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-02-04T00:00:00Z"
                },
                {
                    "id": "task:TASK-3",
                    "kind": "task",
                    "title": "already done — must be skipped",
                    "status": "done",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-02-03T00:00:00Z"
                },
                {
                    "id": "blog:BLOG-1",
                    "kind": "blog",
                    "title": "non-task row from a catch-all backend — must be skipped",
                    "status": "in-progress",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-02-05T00:00:00Z"
                }
            ]
        });

        let tasks = super::extract_in_progress_tasks(&payload);
        assert_eq!(tasks.len(), 3, "only in-progress task-kind subjects are projected");
        assert!(tasks.iter().all(|t| t.id.starts_with("task:")), "non-task rows are excluded from reconciliation");
        assert_eq!(tasks[0].id, "task:TASK-1");
        assert!(!tasks[0].is_human_assignee, "an agent:<name> claim is not a human hold");
        assert_eq!(tasks[0].last_transition_at.to_rfc3339(), "2026-02-01T12:00:00+00:00");
        assert_eq!(tasks[1].id, "task:TASK-2");
        assert!(tasks[1].is_human_assignee, "a plain assignee is treated as a human hold");
        assert_eq!(tasks[2].id, "task:TASK-4");
        assert!(tasks[2].is_human_assignee, "a custom.assignee hold is honored too");
    }

    #[test]
    fn extract_in_progress_tasks_normalizes_status_and_alt_envelopes() {
        // A backend that spells the status `in_progress` and wraps rows under
        // `items` (not `subjects`) must still be projected — otherwise the
        // stale reconciler would silently skip the whole store.
        let payload = serde_json::json!({
            "items": [
                {
                    "id": "task:TASK-7",
                    "status": "in_progress",
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-02-01T00:00:00Z"
                }
            ]
        });
        let tasks = super::extract_in_progress_tasks(&payload);
        assert_eq!(tasks.len(), 1, "in_progress spelling under `items` is projected");
        assert_eq!(tasks[0].id, "task:TASK-7");
    }

    #[test]
    fn extract_next_cursor_treats_null_and_empty_as_final_page() {
        assert_eq!(
            super::extract_next_cursor(&serde_json::json!({ "next_cursor": "abc" })),
            Some(serde_json::json!("abc"))
        );
        assert_eq!(super::extract_next_cursor(&serde_json::json!({ "next_cursor": serde_json::Value::Null })), None);
        assert_eq!(super::extract_next_cursor(&serde_json::json!({ "next_cursor": "  " })), None);
        assert_eq!(super::extract_next_cursor(&serde_json::json!({ "subjects": [] })), None);
    }

    #[test]
    fn wire_status_matches_subject_status_vocabulary() {
        assert_eq!(super::wire_status(TaskStatus::Ready), "ready");
        assert_eq!(super::wire_status(TaskStatus::Blocked), "blocked");
        assert_eq!(super::wire_status(TaskStatus::Cancelled), "cancelled");
    }

    #[test]
    fn task_ids_match_tolerates_task_qualifier_only() {
        assert!(super::task_ids_match("TASK-1", "TASK-1"));
        assert!(super::task_ids_match("task:TASK-1", "TASK-1"));
        assert!(super::task_ids_match("TASK-1", "task:TASK-1"));
        assert!(super::task_ids_match("task:TASK-1", "task:TASK-1"));
        assert!(!super::task_ids_match("task:TASK-1", "task:TASK-2"));
        // A different kind sharing the native id must NOT alias the task: a live
        // `blog:BLOG-1` runner must not shield a stale `task:BLOG-1`.
        assert!(!super::task_ids_match("blog:BLOG-1", "task:BLOG-1"));
        assert!(!super::task_ids_match("blog:BLOG-1", "BLOG-1"));
    }
}
