use super::*;
use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;
use crate::services::runtime::workflow_mutation_surface::cancel_orphaned_running_workflow;
use anyhow::Result;
use orchestrator_core::{
    active_workflow_runner_ids, dispatch_workflow_event, load_agent_runtime_config_or_default, services::ServiceHub,
    OrchestratorWorkflow, WorkflowEvent, WorkflowMachineState, WorkflowStatus,
};
use std::collections::HashSet;
use std::path::Path;
use tracing::{error, info, warn};

/// Grace period after a workflow's `started_at` before the orphan
/// reconciler is allowed to cancel it. Async dispatch paths (control-wire
/// `workflow/run`, CLI `workflow run` without `--sync`) create a Running
/// workflow record before any executor has a chance to register a pid
/// file. Cancelling those within the same tick wipes the user's intent
/// before the scheduler picks them up.
pub(crate) const ORPHAN_RECONCILIATION_GRACE_SECS: i64 = 90;

/// BU-4 kill-switch: force the pre-BU-4 cancel-orphans behavior even when the
/// durable journal backend is active. Safe rollback valve if journal-resume
/// misbehaves in production. Requires a daemon restart to take effect.
const DISABLE_JOURNAL_RESUME_ENV: &str = "ANIMUS_DAEMON_DISABLE_JOURNAL_RESUME";

fn env_flag_enabled(var: &str) -> bool {
    std::env::var(var).map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "on")).unwrap_or(false)
}

/// BU-4: whether the boot/steady-state orphan sweep should RESUME (preserve +
/// re-dispatch) in-flight runs instead of CANCELLING them.
///
/// `true` only when BOTH hold:
///   * the DURABLE (plugin-backed, e.g. Postgres) workflow journal is active —
///     so run state survived a host/volume wipe and there is something to
///     resume; AND
///   * the `ANIMUS_DAEMON_DISABLE_JOURNAL_RESUME` kill-switch is NOT set.
///
/// When `false` (the default for the in-tree SQLite backend), the orphan sweep
/// is byte-identical to the pre-BU-4 cancel behavior.
pub(crate) fn journal_resume_enabled(project_root: &str) -> bool {
    if env_flag_enabled(DISABLE_JOURNAL_RESUME_ENV) {
        return false;
    }
    orchestrator_core::durable_journal_active(Path::new(project_root))
}

/// BU-4: is an orphaned (no-live-runner) non-terminal run RESUMABLE, i.e. does
/// it have a concrete current phase to (re)enter?
///
/// A run is resumable from a phase boundary when it has either an explicit
/// `current_phase`, or a `current_phase_index` that points at a real phase in
/// its plan. A `Running` record with no addressable current phase (never
/// initialized / corrupt) is genuinely UNRESUMABLE and falls through to
/// cancellation — the fallback the BU-4 spec requires.
///
/// ## provider_session_id durability boundary (interim)
///
/// Exact MID-PHASE resume (re-attaching to the agent's in-flight provider
/// session) requires a session checkpoint carrying a `provider_session_id`,
/// handled separately by `auto_resume_running_checkpoints`. That checkpoint
/// lives on the scoped LOCAL volume; this pass relies on it surviving on a
/// durable volume (design recommendation a). When no recoverable
/// `provider_session_id` exists, resume falls back to re-dispatching the run
/// from its current PHASE BOUNDARY (a fresh workflow_runner re-enters
/// `current_phase` from scratch) — which only needs the run-state that the
/// durable journal preserves. `is_resumable_orphan` gates that phase-boundary
/// fallback; only a run with no phase boundary at all is cancelled.
fn is_resumable_orphan(workflow: &OrchestratorWorkflow) -> bool {
    workflow.current_phase.is_some() || workflow.phases.get(workflow.current_phase_index).is_some()
}

pub async fn recover_orphaned_running_workflows(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_subject_ids: &HashSet<String>,
    resume_orphans: bool,
) -> usize {
    let workflows = match hub.workflows().list().await {
        Ok(workflows) => workflows,
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                error = %error,
                "failed to list workflows for orphan recovery"
            );
            return 0;
        }
    };
    let externally_active_workflows = match active_workflow_runner_ids(Path::new(project_root)) {
        Ok(ids) => ids,
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                error = %error,
                "failed to read active workflow runner ids"
            );
            HashSet::new()
        }
    };
    let now = chrono::Utc::now();

    let mut recovered = 0usize;
    for workflow in workflows {
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        if workflow.machine_state == WorkflowMachineState::MergeConflict {
            continue;
        }
        if workflow_is_waiting_on_manual_phase(project_root, &workflow) {
            continue;
        }
        if active_subject_ids.contains(&workflow.id)
            || externally_active_workflows.contains(&workflow.id)
            || active_subject_ids.contains(workflow.subject.id())
        {
            continue;
        }
        if (now - workflow.started_at).num_seconds() < ORPHAN_RECONCILIATION_GRACE_SECS {
            continue;
        }

        // BU-4: when the durable journal is active (and the kill-switch is
        // off), do NOT destroy resumable in-flight work on daemon
        // restart/redeploy. Preserve the Running run so it can be resumed:
        //   * MID-PHASE via `auto_resume_running_checkpoints` when a session
        //     checkpoint with a provider_session_id survives, or
        //   * from its current PHASE BOUNDARY via the re-dispatch leg
        //     (`resumable_orphans_for_redispatch`), which restarts
        //     `current_phase` under a fresh runner using only the run-state
        //     the durable journal preserves.
        // Only genuinely UNRESUMABLE runs (no addressable current phase) fall
        // through to cancellation below. When `resume_orphans` is false (no
        // durable journal / kill-switch set), this is a no-op and the cancel
        // path is byte-identical to pre-BU-4.
        if resume_orphans && is_resumable_orphan(&workflow) {
            info!(
                actor = protocol::ACTOR_DAEMON,
                workflow_id = %workflow.id,
                subject_id = %workflow.subject.id(),
                task_id = %workflow.task_id,
                current_phase = workflow.current_phase.as_deref().unwrap_or_default(),
                "preserving resumable in-flight workflow for resume (durable journal active); not cancelling"
            );
            continue;
        }

        warn!(
            actor = protocol::ACTOR_DAEMON,
            workflow_id = %workflow.id,
            subject_id = %workflow.subject.id(),
            task_id = %workflow.task_id,
            "recovering orphaned running workflow"
        );
        let cancelled = cancel_orphaned_running_workflow(hub.clone(), project_root, &workflow).await;
        if cancelled {
            recovered = recovered.saturating_add(1);
        } else {
            error!(
                actor = protocol::ACTOR_DAEMON,
                workflow_id = %workflow.id,
                "failed to cancel orphaned workflow"
            );
        }
    }

    recovered
}

/// BU-4 re-dispatch candidates: non-terminal `Running` orphans (no live runner)
/// that should be RESTARTED from their current phase boundary by a fresh
/// `workflow_runner`. The caller (the steady-state dispatch leg) spawns one
/// runner per returned run via the `ProcessManager`.
///
/// This MIRRORS every guard in [`recover_orphaned_running_workflows`] so the
/// two never disagree about which runs are "orphaned": status `Running`, not a
/// merge conflict, not waiting on a manual phase, no live runner (neither in
/// `active_subject_ids` — runners owned by THIS daemon — nor in the
/// pid-liveness registry), past the `started_at` grace window, and resumable
/// from a phase boundary. It additionally REQUIRES `journal_resume_enabled`
/// (durable journal + kill-switch off) so it returns an empty set on the
/// SQLite path.
///
/// Idempotency / no-double-dispatch: the `ProcessManager` records the subject
/// as active the moment a runner is spawned, and `spawn_workflow_runner`
/// registers a live pid — so a run re-dispatched on one tick is excluded from
/// both guards on every subsequent tick. Runs already recovered MID-PHASE by
/// `auto_resume_running_checkpoints` move to `Paused` (handoff) and are
/// excluded here by the `Running`-only filter.
pub(crate) async fn resumable_orphans_for_redispatch(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_subject_ids: &HashSet<String>,
) -> Vec<OrchestratorWorkflow> {
    if !journal_resume_enabled(project_root) {
        return Vec::new();
    }
    let workflows = match hub.workflows().list().await {
        Ok(workflows) => workflows,
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                error = %error,
                "failed to list workflows for journal-resume re-dispatch"
            );
            return Vec::new();
        }
    };
    let externally_active_workflows = match active_workflow_runner_ids(Path::new(project_root)) {
        Ok(ids) => ids,
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                error = %error,
                "failed to read active workflow runner ids for journal-resume re-dispatch"
            );
            HashSet::new()
        }
    };

    // Codex P1: a detached workflow runner spawned by a PREVIOUS daemon can
    // still be alive after this daemon restarted. Such a runner is tracked by
    // the agent-record orphan scan (`runs/_pending/agents`), NOT by this fresh
    // process's `active_subject_ids` and not necessarily by the workflow-runner
    // pid registry. Re-dispatching its subject would spawn a SECOND runner for
    // the same work (double-dispatch). Exclude any subject/task that has a live
    // detected orphan. On scan failure we cannot verify liveness, so we
    // suppress re-dispatch entirely this tick (fail safe — never risk a
    // duplicate runner).
    let live_orphan_subjects: HashSet<String> =
        match orchestrator_daemon_runtime::agent_record::scan_orphans_for_project(Path::new(project_root)) {
            Ok(report) => {
                report.detected.into_iter().flat_map(|o| std::iter::once(o.subject_id).chain(o.task_id)).collect()
            }
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    error = %error,
                    "agent-record orphan scan failed; suppressing journal-resume re-dispatch this tick to avoid double-dispatch"
                );
                return Vec::new();
            }
        };

    // Codex P2: a run whose MID-PHASE resume was intentionally HELD by the
    // startup auto-resume pass (a `Blocked` session checkpoint — provider
    // plugin missing, or `resume_agent` returned a failure) is left `Running`
    // awaiting an operator `animus workflow resume --force`. Re-dispatching it
    // from the phase boundary would bypass that hold and re-run intentionally
    // paused work, so exclude it. On scan failure, fail safe by suppressing
    // re-dispatch this tick.
    let blocked_resume_workflows: HashSet<String> = match protocol::scoped_state_root(Path::new(project_root)) {
        Some(scoped_root) => {
            match animus_runtime_shared::phase_session::blocked_checkpoint_workflow_ids(&scoped_root) {
                Ok(ids) => ids,
                Err(error) => {
                    warn!(
                        actor = protocol::ACTOR_DAEMON,
                        error = %error,
                        "blocked-checkpoint scan failed; suppressing journal-resume re-dispatch this tick"
                    );
                    return Vec::new();
                }
            }
        }
        None => HashSet::new(),
    };
    let now = chrono::Utc::now();

    let mut candidates = Vec::new();
    for workflow in workflows {
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        if workflow.machine_state == WorkflowMachineState::MergeConflict {
            continue;
        }
        if workflow_is_waiting_on_manual_phase(project_root, &workflow) {
            continue;
        }
        if active_subject_ids.contains(&workflow.id)
            || externally_active_workflows.contains(&workflow.id)
            || active_subject_ids.contains(workflow.subject.id())
        {
            continue;
        }
        // Skip subjects whose detached runner from a previous daemon is still
        // alive (see `live_orphan_subjects` above).
        if live_orphan_subjects.contains(workflow.subject.id())
            || (!workflow.task_id.is_empty() && live_orphan_subjects.contains(&workflow.task_id))
        {
            continue;
        }
        // Skip runs whose mid-phase resume was intentionally held (Blocked
        // session checkpoint awaiting operator `--force`).
        if blocked_resume_workflows.contains(&workflow.id) {
            continue;
        }
        if (now - workflow.started_at).num_seconds() < ORPHAN_RECONCILIATION_GRACE_SECS {
            continue;
        }
        if !is_resumable_orphan(&workflow) {
            continue;
        }
        candidates.push(workflow);
    }
    candidates
}

pub async fn reconcile_manual_phase_timeouts(hub: Arc<dyn ServiceHub>, project_root: &str) -> Result<usize> {
    let runtime = load_agent_runtime_config_or_default(Path::new(project_root));
    let workflows = match hub.workflows().list().await {
        Ok(workflows) => workflows,
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                error = %error,
                "failed to list workflows for manual phase timeout reconciliation"
            );
            return Ok(0);
        }
    };
    let mut reconciled = 0usize;
    let now = chrono::Utc::now();

    for workflow in workflows {
        if workflow.status != WorkflowStatus::Paused {
            continue;
        }

        let phase_id = workflow
            .current_phase
            .clone()
            .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
            .unwrap_or_default();
        if phase_id.is_empty() {
            continue;
        }

        let definition = match runtime.phase_execution(&phase_id) {
            Some(definition) => definition,
            None => continue,
        };
        if !matches!(definition.mode, orchestrator_core::PhaseExecutionMode::Manual) {
            continue;
        }
        let manual = match definition.manual.as_ref() {
            Some(manual) => manual,
            None => continue,
        };
        let timeout_secs = match manual.timeout_secs {
            Some(timeout_secs) => timeout_secs,
            None => continue,
        };
        if timeout_secs == 0 {
            continue;
        }

        let started_at = workflow
            .phases
            .get(workflow.current_phase_index)
            .and_then(|phase| phase.started_at)
            .or(Some(workflow.started_at));
        let Some(started_at) = started_at else {
            continue;
        };
        let elapsed = now.signed_duration_since(started_at).num_seconds().max(0) as u64;
        if elapsed < timeout_secs {
            continue;
        }

        let reason = format!("manual phase '{}' timed out after {} seconds", phase_id, timeout_secs);
        let outcome = dispatch_workflow_event(
            hub.clone(),
            project_root,
            WorkflowEvent::RejectManualPhase {
                workflow_id: workflow.id.clone(),
                phase_id: phase_id.clone(),
                note: Some(reason.clone()),
            },
        )
        .await?;
        if let Some(updated) = outcome.workflow {
            project_terminal_workflow_result(
                hub.clone(),
                project_root,
                updated.subject.id(),
                Some(updated.task_id.as_str()),
                updated.workflow_ref.as_deref(),
                Some(updated.id.as_str()),
                updated.status,
                updated.failure_reason.as_deref(),
            )
            .await;
        }
        reconciled = reconciled.saturating_add(1);
    }

    Ok(reconciled)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::await_holding_lock)]

    use super::{recover_orphaned_running_workflows, resumable_orphans_for_redispatch};
    use crate::shared::test_env_lock;
    use orchestrator_core::{
        register_workflow_runner_pid, services::ServiceHub, unregister_workflow_runner_pid, FileServiceHub, Priority,
        TaskCreateInput, TaskType, WorkflowRunInput, WorkflowStateManager, WorkflowStatus,
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

    #[tokio::test]
    async fn registered_runner_pid_shields_old_running_workflow_from_orphan_cancel() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
        // v0.6: the kernel sources its base workflow config from a config_source
        // plugin; in tests, stand in for it after the hub scaffolds .animus/.
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "resumed workflow".to_string(),
                description: "orphan reconciler liveness test".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task.id.clone(), None), None)
            .await
            .expect("workflow should start");

        // Simulate a resumed workflow: started_at far in the past, well
        // beyond the orphan grace, status Running.
        let manager = WorkflowStateManager::new(temp.path());
        let mut stored = manager.load(&workflow.id).expect("workflow should load");
        stored.started_at = chrono::Utc::now() - chrono::Duration::hours(2);
        manager.save(&stored).expect("backdated workflow should save");

        // While a live runner pid is registered (the mechanism `workflow
        // resume` uses), the reconciler must leave the workflow alone.
        register_workflow_runner_pid(temp.path(), &workflow.id, std::process::id()).expect("pid should register");
        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false).await;
        assert_eq!(recovered, 0, "live runner pid must shield the resumed workflow");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running);

        // Once the runner is gone, the same workflow is reconciled.
        unregister_workflow_runner_pid(temp.path(), &workflow.id).expect("pid should unregister");
        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false).await;
        assert_eq!(recovered, 1, "orphaned workflow without a live runner must be cancelled");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Cancelled);
    }

    /// Shared BU-4 fixture: a project with a single backdated `Running`
    /// workflow (started_at two hours ago, well past the orphan grace). The
    /// returned `Box<dyn Any>` holds the config_source test seam alive for the
    /// duration of the test (dropping it would uninstall the synthetic base).
    async fn backdated_running_workflow_fixture(
        temp: &TempDir,
    ) -> (Arc<dyn ServiceHub>, String, String, Box<dyn std::any::Any>) {
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        init_git_repo(temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
        let seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "resumable workflow".to_string(),
                description: "BU-4 journal-resume reconcile test".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task.id.clone(), None), None)
            .await
            .expect("workflow should start");

        let manager = WorkflowStateManager::new(temp.path());
        let mut stored = manager.load(&workflow.id).expect("workflow should load");
        stored.started_at = chrono::Utc::now() - chrono::Duration::hours(2);
        manager.save(&stored).expect("backdated workflow should save");

        // The fixture must produce a run that the orphan sweep classifies as
        // RESUMABLE (a real current phase boundary); otherwise the BU-4
        // preserve branch would never fire.
        assert!(
            super::is_resumable_orphan(&stored),
            "fixture workflow must have an addressable current phase to exercise resume"
        );
        (hub.clone(), project_root, workflow.id, Box::new((_home, seam)))
    }

    // BU-4 core: with the durable-journal resume gate ON, an orphaned
    // (no-live-runner) non-terminal run is PRESERVED, not cancelled — it stays
    // Running so the resume / re-dispatch path can pick it up.
    #[tokio::test]
    async fn resumable_orphan_is_preserved_not_cancelled_when_resume_enabled() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), true).await;
        assert_eq!(recovered, 0, "resumable orphan must be preserved (not cancelled) when resume is enabled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running, "preserved orphan must stay Running for resume");
    }

    // BU-4 idempotency: a run WITH a live runner is skipped under resume —
    // never preserved-and-also-handled, never re-cancelled. The live-runner
    // guard short-circuits before the resume branch, exactly as it does on the
    // cancel path.
    #[tokio::test]
    async fn live_runner_run_is_skipped_under_resume() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        register_workflow_runner_pid(temp.path(), &workflow_id, std::process::id()).expect("pid should register");
        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), true).await;
        assert_eq!(recovered, 0, "a run with a live runner must be skipped, not cancelled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running);

        // And the re-dispatch leg must NOT select a run whose runner is live
        // (no double-dispatch). Even with the journal gate, the active-runner
        // guard excludes it; on the SQLite test path the gate already returns
        // empty, which is the stronger safety guarantee asserted below.
        let candidates = resumable_orphans_for_redispatch(hub.clone(), &project_root, &HashSet::new()).await;
        assert!(
            !candidates.iter().any(|w| w.id == workflow_id),
            "live-runner run must never be a re-dispatch candidate"
        );
    }

    // BU-4: a terminal run is ignored by the resume sweep (the Running-only
    // filter), so resume never resurrects completed work.
    #[tokio::test]
    async fn terminal_run_is_ignored_under_resume() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        let manager = WorkflowStateManager::new(temp.path());
        let mut stored = manager.load(&workflow_id).expect("workflow should load");
        stored.status = WorkflowStatus::Completed;
        stored.completed_at = Some(chrono::Utc::now());
        manager.save(&stored).expect("terminal workflow should save");

        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), true).await;
        assert_eq!(recovered, 0, "terminal runs must be ignored by the resume sweep");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Completed, "terminal run must be left untouched");

        let candidates = resumable_orphans_for_redispatch(hub.clone(), &project_root, &HashSet::new()).await;
        assert!(!candidates.iter().any(|w| w.id == workflow_id), "terminal run must never be a re-dispatch candidate");
    }

    // BU-4: a run still inside its `started_at` grace window is untouched
    // under resume — the grace guard is preserved exactly as on the cancel
    // path, so async-dispatched runs are not yanked before a runner registers.
    #[tokio::test]
    async fn orphan_within_grace_is_untouched_under_resume() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        // Pull started_at back into the grace window.
        let manager = WorkflowStateManager::new(temp.path());
        let mut stored = manager.load(&workflow_id).expect("workflow should load");
        stored.started_at = chrono::Utc::now();
        manager.save(&stored).expect("workflow should save");

        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), true).await;
        assert_eq!(recovered, 0, "a run within the grace window must not be cancelled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running);

        let candidates = resumable_orphans_for_redispatch(hub.clone(), &project_root, &HashSet::new()).await;
        assert!(
            !candidates.iter().any(|w| w.id == workflow_id),
            "a run within the grace window must never be a re-dispatch candidate"
        );
    }

    // BU-4 safety gate: the SQLite path (no durable journal) cancels the
    // orphan exactly as pre-BU-4 (resume_orphans=false), AND the re-dispatch
    // leg returns an empty set because `journal_resume_enabled` is false.
    #[tokio::test]
    async fn sqlite_path_cancels_orphan_and_redispatch_is_empty() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        // No workflow_journal plugin installed in the test => SQLite backend =>
        // journal_resume_enabled() == false. The re-dispatch leg self-gates.
        let candidates = resumable_orphans_for_redispatch(hub.clone(), &project_root, &HashSet::new()).await;
        assert!(candidates.is_empty(), "re-dispatch must be inert without a durable journal");

        // resume_orphans=false reproduces the exact pre-BU-4 cancel behavior.
        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false).await;
        assert_eq!(recovered, 1, "SQLite orphan without a live runner must still be cancelled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Cancelled);
    }

    // Workflows suspended on a pending interaction sit in Paused with no
    // live runner pid; the orphan reconciler must leave them alone (it only
    // targets Running records) until the answer path resumes them.
    #[tokio::test]
    async fn paused_workflow_is_exempt_from_orphan_recovery() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        init_git_repo(&temp);
        let project_root = temp.path().to_string_lossy().to_string();
        let hub: Arc<dyn ServiceHub> = Arc::new(FileServiceHub::new(&project_root).expect("file service hub"));
        // v0.6: the kernel sources its base workflow config from a config_source
        // plugin; in tests, stand in for it after the hub scaffolds .animus/.
        let _config_source_seam =
            orchestrator_config::workflow_config::config_source_client::install_yaml_config_source_base(temp.path());

        let task = hub
            .tasks()
            .create(TaskCreateInput {
                title: "suspended workflow".to_string(),
                description: "paused exemption test".to_string(),
                task_type: Some(TaskType::Feature),
                priority: Some(Priority::Medium),
                created_by: Some("test".to_string()),
                tags: Vec::new(),
                linked_requirements: Vec::new(),
                linked_architecture_entities: Vec::new(),
            })
            .await
            .expect("task should be created");
        let workflow = hub
            .workflows()
            .run(WorkflowRunInput::for_task(task.id.clone(), None), None)
            .await
            .expect("workflow should start");
        hub.workflows().pause(&workflow.id).await.expect("workflow should pause");

        // Backdate started_at well past the orphan grace so only the Paused
        // status shields it.
        let manager = WorkflowStateManager::new(temp.path());
        let mut stored = manager.load(&workflow.id).expect("workflow should load");
        stored.started_at = chrono::Utc::now() - chrono::Duration::hours(2);
        manager.save(&stored).expect("backdated workflow should save");

        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false).await;
        assert_eq!(recovered, 0, "paused workflows must be exempt from orphan recovery");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Paused);
    }
}

fn workflow_is_waiting_on_manual_phase(project_root: &str, workflow: &orchestrator_core::OrchestratorWorkflow) -> bool {
    let Some(phase_id) = workflow
        .current_phase
        .clone()
        .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
    else {
        return false;
    };

    orchestrator_core::load_agent_runtime_config(Path::new(project_root))
        .ok()
        .and_then(|config| config.phase_execution(&phase_id).cloned())
        .map(|definition| matches!(definition.mode, orchestrator_core::PhaseExecutionMode::Manual))
        .unwrap_or(false)
}
