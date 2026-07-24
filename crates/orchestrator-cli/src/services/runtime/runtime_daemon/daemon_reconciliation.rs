use super::*;
use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;
use crate::services::runtime::workflow_mutation_surface::cancel_orphaned_running_workflow;
use animus_environment_protocol::HarnessCommand;
use animus_runtime_shared::phase_session::{
    mark_environment_torn_down, read_checkpoint, update_session_failed, EnvironmentBinding,
};
use anyhow::Result;
use orchestrator_core::{
    active_workflow_runner_ids, dispatch_workflow_event, load_agent_runtime_config_or_default, services::ServiceHub,
    EnvironmentClient, OrchestratorWorkflow, WorkflowEvent, WorkflowMachineState, WorkflowStatus,
};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;
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

// ---------------------------------------------------------------------------
// TASK-933 / TASK-793 / TASK-811: delegated (environment-routed) orphan
// reconciliation.
//
// A DELEGATED coding phase runs its provider harness inside a remote node
// (Railway / container) prepared by an `environment` plugin. That node<->run
// binding is persisted into the phase session checkpoint by the OUT-OF-TREE
// workflow runner the instant `environment/prepare` succeeds (the daemon never
// calls prepare for a coding phase — see `environment_exec.rs` scope note). On
// a daemon restart the runner process is gone but the remote node may still be
// alive and billing.
//
// This block gives the daemon-side reconciler three capabilities, all driven
// off the persisted `EnvironmentBinding`:
//   * reap a leaked node by its handle before re-dispatch (HALF A leak-killer),
//   * gate the orphan-preserve on actual node liveness so a dead delegate is
//     not preserved as a phantom `Running` lease forever (TASK-793),
//   * terminalize a dead delegation ghost (reap node + fail the checkpoint +
//     cancel the workflow) so it never re-surfaces (TASK-811).
//
// IMPORTANT backward-compat property: the `environment` binding is written
// ONLY by a runner that carries the companion change. Until that ships,
// `current_delegate_binding` returns `None` for every workflow, so every
// helper below is INERT and the reconciler's behavior is byte-identical to
// today. There is no behavior change for local (non-delegated) runs, ever.
// ---------------------------------------------------------------------------

/// The workflow's current phase id (explicit `current_phase`, else the phase
/// at `current_phase_index`).
fn current_phase_id(workflow: &OrchestratorWorkflow) -> Option<String> {
    workflow
        .current_phase
        .clone()
        .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
}

/// Load the delegated environment binding for a workflow's current phase, if
/// its session checkpoint carries one that has NOT already been torn down.
/// Returns `(phase_id, binding)`. `None` for a local run, a missing/unreadable
/// checkpoint, or an already-reaped node — all of which mean "nothing to do".
fn current_delegate_binding(
    scoped_root: &Path,
    workflow: &OrchestratorWorkflow,
) -> Option<(String, EnvironmentBinding)> {
    let phase_id = current_phase_id(workflow)?;
    let checkpoint = read_checkpoint(scoped_root, &workflow.id, &phase_id).ok()??;
    let binding = checkpoint.environment.filter(|binding| !binding.torn_down)?;
    Some((phase_id, binding))
}

/// Liveness of a delegated node, as observed by a trivial exec probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelegateLiveness {
    /// The node answered a trivial exec — it is up and reusable.
    Alive,
    /// The node/plugin failed the probe — presumed gone (reap + terminalize).
    Dead,
    /// The environment plugin could not be resolved — cannot verify. Fail safe
    /// (preserve, never destroy work we cannot confirm is lost).
    Unknown,
}

/// Probe a delegated node's liveness. There is no `environment/status` method
/// (the protocol is prepare/exec/exec_stream/teardown only), so liveness is a
/// trivial, side-effect-free `exec` of `true` under a short timeout: success =>
/// Alive; a death-like host/RPC failure => Dead; an unresolvable plugin =>
/// Unknown.
///
/// NB: `EnvironmentClient::exec` bridges a blocking RPC; when called from the
/// daemon's async reconciler it uses `block_in_place`, which requires the
/// multi-threaded runtime the daemon runs on.
fn probe_delegate(project_root: &str, binding: &EnvironmentBinding) -> DelegateLiveness {
    let client = match EnvironmentClient::resolve(Path::new(project_root), &binding.environment_id) {
        Ok(client) => client,
        Err(_) => return DelegateLiveness::Unknown,
    };
    let probe = HarnessCommand { program: "true".to_string(), args: Vec::new(), env: Default::default(), cwd: None };
    match client.exec(&binding.handle, probe, Default::default(), None, Some(Duration::from_secs(10))) {
        Ok(_) => DelegateLiveness::Alive,
        Err(_) => DelegateLiveness::Dead,
    }
}

/// Reap the delegated node bound to a workflow's current phase, by its
/// persisted handle. Idempotent: teardown is dispose-by-id (a no-op if the node
/// is already gone), and `mark_environment_torn_down` is only written on
/// success, so a failed reap is retried on the next sweep (no double-free, no
/// permanent leak). Inert when there is no (un-torn-down) binding.
fn teardown_delegated_node(project_root: &str, scoped_root: Option<&Path>, workflow: &OrchestratorWorkflow) {
    let Some(scoped_root) = scoped_root else { return };
    let Some((phase_id, binding)) = current_delegate_binding(scoped_root, workflow) else { return };
    match EnvironmentClient::resolve(Path::new(project_root), &binding.environment_id) {
        Ok(client) => match client.teardown(&binding.handle) {
            Ok(()) => {
                let _ = mark_environment_torn_down(scoped_root, &workflow.id, &phase_id);
                info!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow.id,
                    node = %binding.handle.id,
                    "reaped leaked delegated node by persisted handle on restart reconciliation"
                );
            }
            Err(error) => warn!(
                actor = protocol::ACTOR_DAEMON,
                workflow_id = %workflow.id,
                node = %binding.handle.id,
                %error,
                "env teardown of delegated node failed on restart; will retry on the next sweep"
            ),
        },
        Err(error) => warn!(
            actor = protocol::ACTOR_DAEMON,
            workflow_id = %workflow.id,
            environment = %binding.environment_id,
            %error,
            "cannot resolve environment plugin to reap leaked delegated node; will retry on the next sweep"
        ),
    }
}

/// TASK-811: drive a DEAD delegation ghost to a terminal state. Reaps the node
/// by handle, fails the phase checkpoint so it never re-surfaces for auto-resume
/// (`list_running_checkpoints` only yields `Running`), and drives the workflow
/// itself terminal via the existing orphan-cancel path (so downstream terminal
/// projections fire identically to a normal orphan cancel).
async fn terminalize_dead_delegation(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    scoped_root: &Path,
    workflow: &OrchestratorWorkflow,
) {
    let phase_id = current_phase_id(workflow).unwrap_or_default();
    // 1. reap the node by persisted handle (idempotent; sets torn_down on success)
    teardown_delegated_node(project_root, Some(scoped_root), workflow);
    // 2. terminalize the phase checkpoint so it never re-surfaces for resume
    let _ = update_session_failed(
        scoped_root,
        &workflow.id,
        &phase_id,
        "delegated environment node died before exec; terminalized by orphan reconciler (TASK-811)",
    );
    // 3. drive the workflow terminal via the existing orphan-cancel path
    cancel_orphaned_running_workflow(hub, project_root, workflow).await;
}

/// The reconciliation verdict for an orphaned, resumable workflow, once its
/// delegated-node liveness (if any) is factored in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelegateDecision {
    /// Not a delegated run (no live binding) — behave exactly as the
    /// pre-existing local path.
    NotDelegated,
    /// Delegated node is alive (or unverifiable — fail safe) — preserve the
    /// run so the companion runner re-dispatch can reuse the node.
    Preserve,
    /// Delegated node is dead — terminalize the ghost and reap the node.
    TerminalizeDead,
}

/// Classify a resumable orphan by its delegated-node liveness. Pure decision so
/// the preserve/terminalize gate is unit-testable without a live plugin.
fn classify_delegate(scoped_root: Option<&Path>, project_root: &str, workflow: &OrchestratorWorkflow) -> DelegateDecision {
    let Some(scoped_root) = scoped_root else { return DelegateDecision::NotDelegated };
    let Some((_, binding)) = current_delegate_binding(scoped_root, workflow) else {
        return DelegateDecision::NotDelegated;
    };
    match probe_delegate(project_root, &binding) {
        DelegateLiveness::Alive | DelegateLiveness::Unknown => DelegateDecision::Preserve,
        DelegateLiveness::Dead => DelegateDecision::TerminalizeDead,
    }
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
    // TASK-933/793/811: the delegated-node binding lives in the scoped session
    // checkpoints. `None` (no scope) => every delegate helper is inert and the
    // sweep behaves exactly as the pre-existing local path.
    let scoped_root = protocol::scoped_state_root(Path::new(project_root));

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
            || workflow.subject.as_ref().is_some_and(|s| active_subject_ids.contains(s.id()))
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
            // TASK-793/811: a resumable orphan is preserved ONLY if it is a
            // local run or a delegated run whose node is still alive. A
            // delegated run whose node died between prepare and exec is NOT
            // preserved as a phantom Running lease — it is terminalized and its
            // node reaped. (`classify_delegate` is `NotDelegated` for every run
            // until the companion runner persists the binding, so this is
            // byte-identical to today's preserve-all until then.)
            match classify_delegate(scoped_root.as_deref(), project_root, &workflow) {
                DelegateDecision::TerminalizeDead => {
                    warn!(
                        actor = protocol::ACTOR_DAEMON,
                        workflow_id = %workflow.id,
                        subject_id = %workflow.subject.as_ref().map(|s| s.id()).unwrap_or_default(),
                        task_id = %workflow.task_id,
                        "delegated node is dead; terminalizing ghost + reaping node (TASK-793/811)"
                    );
                    terminalize_dead_delegation(
                        hub.clone(),
                        project_root,
                        scoped_root.as_deref().expect("delegated classification requires a scope"),
                        &workflow,
                    )
                    .await;
                    recovered = recovered.saturating_add(1);
                    continue;
                }
                DelegateDecision::NotDelegated | DelegateDecision::Preserve => {
                    info!(
                        actor = protocol::ACTOR_DAEMON,
                        workflow_id = %workflow.id,
                        subject_id = %workflow.subject.as_ref().map(|s| s.id()).unwrap_or_default(),
                        task_id = %workflow.task_id,
                        current_phase = workflow.current_phase.as_deref().unwrap_or_default(),
                        "preserving resumable in-flight workflow for resume (durable journal active); not cancelling"
                    );
                    continue;
                }
            }
        }

        warn!(
            actor = protocol::ACTOR_DAEMON,
            workflow_id = %workflow.id,
            subject_id = %workflow.subject.as_ref().map(|s| s.id()).unwrap_or_default(),
            task_id = %workflow.task_id,
            "recovering orphaned running workflow"
        );
        // HALF A leak-killer: reap any persisted delegated node BEFORE cancel,
        // so a non-resumable (or non-journal) orphan does not leak its node.
        // Inert (no-op) for local runs and until the companion runner persists
        // the binding.
        teardown_delegated_node(project_root, scoped_root.as_deref(), &workflow);
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
            || workflow.subject.as_ref().is_some_and(|s| active_subject_ids.contains(s.id()))
        {
            continue;
        }
        // Skip subjects whose detached runner from a previous daemon is still
        // alive (see `live_orphan_subjects` above).
        if workflow.subject.as_ref().is_some_and(|s| live_orphan_subjects.contains(s.id()))
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

    // Route task-status projections through the installed subject backend when
    // one owns `task` (portal), else the in-tree store. Resolved once for the
    // sweep.
    let task_store = orchestrator_daemon_runtime::resolve_task_projection_store(project_root, hub.clone()).await;

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
            task_store.as_ref(),
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
                updated.subject.as_ref().map(|s| s.id()).unwrap_or_default(),
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

    // -----------------------------------------------------------------
    // TASK-933 / TASK-793 / TASK-811: delegated-node reconciliation
    // -----------------------------------------------------------------

    use animus_runtime_shared::phase_session::{
        read_checkpoint, update_session_environment, write_session_pending, EnvironmentBinding,
        SessionCheckpointStatus,
    };

    fn sample_binding(node_id: &str) -> EnvironmentBinding {
        EnvironmentBinding {
            environment_id: "animus-environment-railway".to_string(),
            handle: animus_environment_protocol::EnvironmentHandle {
                id: node_id.to_string(),
                workspace_root: "/work".to_string(),
                metadata: serde_json::json!({ "railway_service_id": "svc-1" }),
            },
            bound_at: chrono::Utc::now().to_rfc3339(),
            torn_down: false,
        }
    }

    /// Write a Running session checkpoint carrying `binding` for the workflow's
    /// current phase, returning the scoped root + phase id.
    async fn bind_delegate(
        hub: &Arc<dyn ServiceHub>,
        project_root: &str,
        workflow_id: &str,
        binding: EnvironmentBinding,
    ) -> (std::path::PathBuf, String) {
        let workflow = hub.workflows().get(workflow_id).await.expect("workflow loads");
        let phase_id = super::current_phase_id(&workflow).expect("fixture workflow has a current phase");
        let scoped_root =
            protocol::scoped_state_root(std::path::Path::new(project_root)).expect("git-repo project has a scope");
        write_session_pending(&scoped_root, workflow_id, &phase_id, "claude", "run-delegated", None)
            .expect("write pending checkpoint");
        update_session_environment(&scoped_root, workflow_id, &phase_id, binding).expect("persist binding");
        (scoped_root, phase_id)
    }

    // current_delegate_binding only yields an UN-TORN-DOWN binding: no
    // checkpoint => None; a torn-down binding => None (already reaped); a live
    // binding => Some.
    #[tokio::test]
    async fn current_delegate_binding_reads_untorn_binding_only() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let workflow = hub.workflows().get(&workflow_id).await.expect("workflow loads");

        // No checkpoint yet => no binding.
        let scoped_root = protocol::scoped_state_root(std::path::Path::new(&project_root)).expect("scope");
        assert!(super::current_delegate_binding(&scoped_root, &workflow).is_none(), "no checkpoint => no binding");

        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-live")).await;
        let (got_phase, got_binding) =
            super::current_delegate_binding(&scoped_root, &workflow).expect("live binding present");
        assert_eq!(got_phase, phase_id);
        assert_eq!(got_binding.handle.id, "node-live");

        // Mark torn down => treated as nothing-to-do.
        animus_runtime_shared::phase_session::mark_environment_torn_down(&scoped_root, &workflow_id, &phase_id)
            .expect("mark torn down");
        assert!(
            super::current_delegate_binding(&scoped_root, &workflow).is_none(),
            "an already-reaped node is not returned again"
        );
    }

    // classify_delegate: NotDelegated without a binding; fail-safe Preserve when
    // the environment plugin cannot be resolved (no plugin installed in tests),
    // i.e. we never destroy work whose node liveness we cannot verify.
    #[tokio::test]
    async fn classify_delegate_not_delegated_and_fails_safe_to_preserve() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let workflow = hub.workflows().get(&workflow_id).await.expect("workflow loads");
        let scoped_root = protocol::scoped_state_root(std::path::Path::new(&project_root)).expect("scope");

        assert_eq!(
            super::classify_delegate(Some(&scoped_root), &project_root, &workflow),
            super::DelegateDecision::NotDelegated,
            "a run with no environment binding is not delegated"
        );
        assert_eq!(
            super::classify_delegate(None, &project_root, &workflow),
            super::DelegateDecision::NotDelegated,
            "no scope => not delegated"
        );

        bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-unverifiable")).await;
        // No environment plugin is installed in the test, so resolve fails =>
        // liveness Unknown => Preserve (fail safe), NOT terminalize.
        assert_eq!(
            super::classify_delegate(Some(&scoped_root), &project_root, &workflow),
            super::DelegateDecision::Preserve,
            "unverifiable liveness must fail safe to Preserve, never destroy the run"
        );
    }

    // The preserve-gate: a resumable delegated orphan whose liveness cannot be
    // verified is PRESERVED (Running), not cancelled — mirrors the local
    // preserve path. (Alive/Dead discrimination needs a live plugin and is
    // covered by the terminalize test below for the Dead action.)
    #[tokio::test]
    async fn resumable_delegated_orphan_with_unverifiable_node_is_preserved() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-x")).await;

        // resume_orphans=true (durable-journal posture) + a delegated binding
        // whose plugin is unresolvable => fail-safe preserve.
        let recovered = recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), true).await;
        assert_eq!(recovered, 0, "an unverifiable delegated orphan must be preserved, not terminalized");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow reloads");
        assert_eq!(reloaded.status, WorkflowStatus::Running, "preserved delegated orphan stays Running");
    }

    // TASK-811: terminalize_dead_delegation drives the checkpoint terminal
    // (Failed, so it never re-surfaces for auto-resume) and cancels the
    // workflow via the existing orphan-cancel path. (Node teardown no-ops here
    // because no environment plugin is installed — the reap is idempotent and
    // best-effort; the terminal state transition is the assertion.)
    #[tokio::test]
    async fn terminalize_dead_delegation_fails_checkpoint_and_cancels_workflow() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-dead")).await;
        let workflow = hub.workflows().get(&workflow_id).await.expect("workflow loads");

        super::terminalize_dead_delegation(hub.clone(), &project_root, &scoped_root, &workflow).await;

        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert_eq!(
            checkpoint.status,
            SessionCheckpointStatus::Failed,
            "dead delegation ghost's checkpoint is failed so it never re-surfaces for resume"
        );
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow reloads");
        assert_eq!(reloaded.status, WorkflowStatus::Cancelled, "the dead delegation ghost's workflow is terminalized");
    }

    // teardown_delegated_node is a safe no-op when there is no scope, no
    // checkpoint, or the binding is already torn down (idempotent; never errors,
    // never double-reaps).
    #[tokio::test]
    async fn teardown_delegated_node_is_inert_without_a_live_binding() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let workflow = hub.workflows().get(&workflow_id).await.expect("workflow loads");

        // No scope / no checkpoint: pure no-op (must not panic or error).
        super::teardown_delegated_node(&project_root, None, &workflow);
        let scoped_root = protocol::scoped_state_root(std::path::Path::new(&project_root)).expect("scope");
        super::teardown_delegated_node(&project_root, Some(&scoped_root), &workflow);

        // With a binding but an already-torn-down flag, teardown is skipped and
        // the flag stays set (idempotent).
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-gone")).await;
        animus_runtime_shared::phase_session::mark_environment_torn_down(&scoped_root, &workflow_id, &phase_id)
            .expect("mark torn down");
        super::teardown_delegated_node(&project_root, Some(&scoped_root), &workflow);
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert!(
            checkpoint.environment.expect("binding present").torn_down,
            "an already-reaped binding stays torn_down (no double-free)"
        );
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
