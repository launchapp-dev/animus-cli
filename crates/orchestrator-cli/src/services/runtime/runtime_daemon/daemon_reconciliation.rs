use super::*;
use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;
use crate::services::runtime::workflow_mutation_surface::cancel_orphaned_running_workflow;
use animus_environment_protocol::{EnvironmentHandle, ExecResponse, HarnessCommand};
use animus_runtime_shared::phase_session::{
    list_checkpoints_with_retained_environment, mark_environment_torn_down, read_checkpoint, update_session_failed,
    EnvironmentBinding,
};
use anyhow::{Context, Result};
use orchestrator_core::{
    active_workflow_runner_ids, dispatch_workflow_event, load_agent_runtime_config_or_default, services::ServiceHub,
    workflow_runner_liveness, EnvironmentClient, OrchestratorWorkflow, RunnerLiveness, WorkflowConfig, WorkflowEvent,
    WorkflowMachineState, WorkflowStatus,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;
use tracing::{debug, error, info, warn};

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

/// Gates the verbose per-run `reconcile-decision` STDOUT lines. Default ON — a
/// reap of a live remote run is effectively invisible in production (the
/// reconciler otherwise logs only via `tracing`/stderr and the on-disk
/// `orchestrator_logging` .jsonl, neither of which surfaces to Railway), so this
/// diagnostic MUST be on by default after deploy. Set
/// `ANIMUS_DAEMON_RECONCILE_TRACE=0` (or `false`/`off`/`no`) to quiet the
/// steady-state no-op lines once the reap is understood. Cancels, delegated
/// skips, config-outage suppressions, and the per-sweep summary are emitted
/// REGARDLESS of this flag so the actionable events always reach the log.
const RECONCILE_TRACE_ENV: &str = "ANIMUS_DAEMON_RECONCILE_TRACE";

fn reconcile_trace_enabled() -> bool {
    !matches!(std::env::var(RECONCILE_TRACE_ENV).ok().as_deref().map(str::trim), Some("0" | "false" | "off" | "no"))
}

/// The classified outcome of evaluating one `Running` workflow against the
/// orphan-sweep guards. Purely for the `reconcile-decision` log line — the
/// actual action is driven off the SAME booleans in the SAME precedence, so the
/// logged decision never diverges from what the sweep does.
#[derive(Clone, Copy)]
enum ReconcileDecision {
    /// Shielded because the run sits in a merge-conflict handoff (left alone).
    MergeConflict,
    /// Shielded because its current phase is a manual/human phase (left alone).
    WaitingManual,
    /// Shielded because its `workflow_id` is in the live runner-pid registry.
    ProtectedLiveRunner,
    /// Shielded because its subject/id is in this daemon's `active_subject_ids`.
    ProtectedActiveSubject,
    /// Skipped because it is delegated to a live remote (environment) node.
    SkippedDelegated,
    /// TASK-793/811: delegated to a remote node whose persisted handle probed
    /// DEAD — terminalize the ghost (reap node + fail checkpoint + cancel).
    TerminalizeDeadDelegate,
    /// Not yet actionable: still inside the `started_at` grace window.
    WithinGrace,
    /// Preserved (not cancelled) for journal resume — it has a phase boundary.
    PreservedResumable,
    /// Selected as a journal-resume re-dispatch candidate (redispatch leg only).
    RedispatchCandidate,
    /// Skipped: a detached runner from a previous daemon is still alive
    /// (redispatch leg only).
    SkippedLiveOrphan,
    /// Skipped: its mid-phase resume was intentionally held (redispatch leg only).
    SkippedBlockedResume,
    /// Not resumable (no addressable current phase) — re-dispatch declines it
    /// (redispatch leg only; the cancel leg cancels this case).
    NotResumable,
    /// Cancelled as an unrecoverable orphan (cancel leg only).
    CancelledOrphan,
    /// TASK-1466: a TERMINAL run's retained environment lease was torn down
    /// by the sweep (terminal-env leg only).
    TeardownTerminalLease,
    /// TASK-1466: teardown of a terminal run's retained environment lease
    /// FAILED; the durable record is retained for the next sweep
    /// (terminal-env leg only).
    TerminalLeaseTeardownFailed,
    /// TASK-1466: retained lease skipped — the run is not terminal in the
    /// journal (terminal-env leg only).
    SkippedLeaseRunActive,
    /// TASK-1466: retained lease skipped — the journal row could not be read,
    /// so terminality is unconfirmed (terminal-env leg only; fail safe).
    SkippedLeaseRunUnknown,
    /// TASK-1466: terminal run is still inside the teardown grace floor, so
    /// the normal owner-driven teardown gets first chance (terminal-env leg
    /// only).
    TerminalLeaseWithinGrace,
}

impl ReconcileDecision {
    fn label(self) -> &'static str {
        match self {
            ReconcileDecision::MergeConflict => "merge-conflict",
            ReconcileDecision::WaitingManual => "waiting-manual",
            ReconcileDecision::ProtectedLiveRunner => "protected-live-runner",
            ReconcileDecision::ProtectedActiveSubject => "protected-active-subject",
            ReconcileDecision::SkippedDelegated => "skipped-delegated",
            ReconcileDecision::TerminalizeDeadDelegate => "terminalize-dead-delegate",
            ReconcileDecision::WithinGrace => "within-grace",
            ReconcileDecision::PreservedResumable => "preserved-resumable",
            ReconcileDecision::RedispatchCandidate => "redispatch-candidate",
            ReconcileDecision::SkippedLiveOrphan => "skipped-live-orphan",
            ReconcileDecision::SkippedBlockedResume => "skipped-blocked-resume",
            ReconcileDecision::NotResumable => "not-resumable",
            ReconcileDecision::CancelledOrphan => "cancelled-orphan",
            ReconcileDecision::TeardownTerminalLease => "teardown-terminal-lease",
            ReconcileDecision::TerminalLeaseTeardownFailed => "terminal-lease-teardown-failed",
            ReconcileDecision::SkippedLeaseRunActive => "skipped-lease-run-active",
            ReconcileDecision::SkippedLeaseRunUnknown => "skipped-lease-run-unknown",
            ReconcileDecision::TerminalLeaseWithinGrace => "terminal-lease-within-grace",
        }
    }

    /// Actionable decisions that must be logged even when per-run trace is OFF:
    /// a cancel, a delegated skip, or a re-dispatch selection.
    fn always_emit(self) -> bool {
        matches!(
            self,
            ReconcileDecision::CancelledOrphan
                | ReconcileDecision::SkippedDelegated
                | ReconcileDecision::TerminalizeDeadDelegate
                | ReconcileDecision::RedispatchCandidate
                | ReconcileDecision::TeardownTerminalLease
                | ReconcileDecision::TerminalLeaseTeardownFailed
        )
    }
}

/// Render an optional/possibly-empty field so the space-separated `key=value`
/// record stays parseable (empty -> `-`).
fn log_field(value: &str) -> &str {
    if value.trim().is_empty() {
        "-"
    } else {
        value
    }
}

/// Snapshot the runner-pid liveness for every `Running` workflow BEFORE
/// `active_workflow_runner_ids` runs — that call PRUNES any pid file it judges
/// dead, which would erase the very recorded-vs-current start-time facts the
/// reap diagnostic needs. Reading first (read-only) preserves them.
fn snapshot_runner_liveness(
    project_root: &Path,
    workflows: &[OrchestratorWorkflow],
) -> HashMap<String, RunnerLiveness> {
    workflows
        .iter()
        .filter(|workflow| workflow.status == WorkflowStatus::Running)
        .map(|workflow| (workflow.id.clone(), workflow_runner_liveness(project_root, &workflow.id)))
        .collect()
}

/// Emit one greppable single-line `reconcile-decision` record to STDOUT (the
/// only channel that reaches Railway logs). The runner-registry block
/// (`pid_file_present`/`pid`/`pid_alive`/`recorded_start`/`current_start`/
/// `runner_is_live`) is the KEY data for diagnosing why a live runner failed to
/// shield a delegated run from the reap.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn emit_reconcile_decision(
    sweep: &str,
    workflow: &OrchestratorWorkflow,
    decision: ReconcileDecision,
    age_secs: i64,
    in_active_subjects: bool,
    in_runner_registry: bool,
    is_delegated: bool,
    is_resumable: bool,
    waiting_manual: bool,
    liveness: &RunnerLiveness,
) {
    let subject_id = workflow.subject.as_ref().map(|s| s.id().to_string()).unwrap_or_default();
    let pid = liveness.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string());
    println!(
        "reconcile-decision ts={ts} sweep={sweep} workflow_id={workflow_id} workflow_ref={workflow_ref} \
         subject_id={subject_id} status={status:?} machine_state={machine_state:?} age_secs={age_secs} \
         grace_secs={grace_secs} in_active_subjects={in_active_subjects} in_runner_registry={in_runner_registry} \
         is_delegated={is_delegated} is_resumable={is_resumable} waiting_manual={waiting_manual} \
         pid_file_present={pid_file_present} pid={pid} pid_alive={pid_alive} recorded_start={recorded_start} \
         current_start={current_start} runner_is_live={runner_is_live} decision={decision}",
        ts = chrono::Utc::now().to_rfc3339(),
        workflow_id = log_field(&workflow.id),
        workflow_ref = log_field(workflow.workflow_ref.as_deref().unwrap_or_default()),
        subject_id = log_field(&subject_id),
        status = workflow.status,
        machine_state = workflow.machine_state,
        grace_secs = ORPHAN_RECONCILIATION_GRACE_SECS,
        pid_file_present = liveness.present,
        pid_alive = liveness.pid_alive,
        recorded_start = log_field(liveness.recorded_start.as_deref().unwrap_or_default()),
        current_start = log_field(liveness.current_start.as_deref().unwrap_or_default()),
        runner_is_live = liveness.live,
        decision = decision.label(),
    );
}

/// Emit the one-line per-sweep summary. `runner_registry_ids` shows EXACTLY what
/// `active_workflow_runner_ids` returned this tick — crucial for confirming
/// whether a supposedly-live runner was actually in the registry. `extra` carries
/// a leg-specific trailing field (e.g. `selected=<n>` for the re-dispatch leg,
/// which never cancels); it is appended verbatim when non-empty.
fn emit_reconcile_sweep_summary(
    sweep: &str,
    evaluated: usize,
    cancelled: usize,
    skipped_delegated: usize,
    protected: usize,
    runner_registry_ids: &HashSet<String>,
    extra: &str,
) {
    let mut ids: Vec<&str> = runner_registry_ids.iter().map(String::as_str).collect();
    ids.sort_unstable();
    let extra = if extra.is_empty() { String::new() } else { format!(" {extra}") };
    println!(
        "reconcile-sweep ts={ts} sweep={sweep} evaluated={evaluated} cancelled={cancelled} \
         skipped_delegated={skipped_delegated} protected={protected} runner_registry_ids=[{ids}]{extra}",
        ts = chrono::Utc::now().to_rfc3339(),
        ids = ids.join(","),
    );
}

/// Emit a one-line record when a whole sweep is SUPPRESSED this tick (config
/// source outage). `tracing::warn!` is invisible in Railway, so surface it to
/// STDOUT too.
fn emit_reconcile_suppressed(sweep: &str, reason: &str) {
    println!("reconcile-sweep ts={ts} sweep={sweep} suppressed={reason}", ts = chrono::Utc::now().to_rfc3339());
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
// TASK-933 / TASK-793 / TASK-811: liveness-REFINED delegated reconciliation.
//
// rc.24 (`is_delegated_run`) skips a delegated run by ROUTING INTENT: if config
// routes the workflow to a non-local environment, the sweep leaves it alone
// because "the node owns its liveness". rc.24's own commit names the gap
// (TASK-793): a delegated run whose node DIED between prepare and exec is then
// preserved as a phantom `Running` lease forever, and its node leaks.
//
// This block REFINES that intent-based skip with the node's ACTUAL liveness,
// read off the `EnvironmentBinding` the out-of-tree runner persists into the
// phase session checkpoint the instant `environment/prepare` succeeds (the
// daemon never runs a coding phase itself — see `environment_exec.rs`). The two
// compose into ONE gate, not two parallel skips:
//   * intent says "delegated"  -> rc.24's `is_delegated_run` selects the run;
//   * a persisted binding lets us PROBE the node:
//       - Alive / Unknown / no-binding -> keep rc.24's skip (preserve),
//       - Dead (past grace)            -> terminalize the ghost + reap the node.
//
// Backward-compat: until the companion runner persists a binding,
// `current_delegate_binding` returns `None`, `delegate_is_dead` is always
// `false`, and the decision stays exactly rc.24's `SkippedDelegated`. There is
// no behavior change for any run until the binding exists.
// ---------------------------------------------------------------------------

/// The workflow's current phase id (explicit `current_phase`, else the phase at
/// `current_phase_index`).
fn current_phase_id(workflow: &OrchestratorWorkflow) -> Option<String> {
    workflow
        .current_phase
        .clone()
        .or_else(|| workflow.phases.get(workflow.current_phase_index).map(|phase| phase.phase_id.clone()))
}

/// Load the delegated environment binding for a workflow's current phase, if
/// its session checkpoint carries one that has NOT already been torn down.
/// Returns `(phase_id, binding)`. `None` for a local run, a missing/unreadable
/// checkpoint, or an already-reaped node — all "nothing to do".
fn current_delegate_binding(
    scoped_root: &Path,
    workflow: &OrchestratorWorkflow,
) -> Option<(String, EnvironmentBinding)> {
    let phase_id = current_phase_id(workflow)?;
    let checkpoint = read_checkpoint(scoped_root, &workflow.id, &phase_id).ok()??;
    let binding = checkpoint.environment.filter(|binding| !binding.torn_down)?;
    Some((phase_id, binding))
}

/// Release every distinct delegated node retained by a workflow before an
/// external cancellation is committed. Bindings are scanned across all phase
/// checkpoints because the current phase may be manual/local while an earlier
/// delegated phase still owns the workflow-scoped lease. Teardown happens
/// first and every checkpoint referring to that handle is marked only after it
/// succeeds, so failed cleanup remains retryable.
pub(crate) fn teardown_retained_environment_for_cancel(
    project_root: &str,
    workflow: &OrchestratorWorkflow,
) -> anyhow::Result<()> {
    let Some(scoped_root) = protocol::scoped_state_root(Path::new(project_root)) else {
        return Ok(());
    };
    teardown_retained_environment_with(&scoped_root, workflow, |binding| {
        let client =
            EnvironmentClient::resolve(Path::new(project_root), &binding.environment_id).with_context(|| {
                format!(
                    "cannot resolve environment plugin '{}' to cancel retained node {} for workflow {}",
                    binding.environment_id, binding.handle.id, workflow.id
                )
            })?;
        client.teardown(&binding.handle).with_context(|| {
            format!(
                "failed to teardown retained node {} in environment '{}' for cancelled workflow {}",
                binding.handle.id, binding.environment_id, workflow.id
            )
        })
    })
}

fn teardown_retained_environment_with<F>(
    scoped_root: &Path,
    workflow: &OrchestratorWorkflow,
    teardown: F,
) -> anyhow::Result<()>
where
    F: FnMut(&EnvironmentBinding) -> anyhow::Result<()>,
{
    let mut teardown = teardown;
    for (binding, phase_ids) in retained_delegate_bindings(scoped_root, workflow) {
        teardown(&binding)?;
        for phase_id in &phase_ids {
            mark_environment_torn_down(scoped_root, &workflow.id, phase_id).with_context(|| {
                format!(
                    "node {} was torn down but its retained binding could not be marked for workflow {}/{}",
                    binding.handle.id, workflow.id, phase_id
                )
            })?;
        }
        info!(
            actor = protocol::ACTOR_DAEMON,
            workflow_id = %workflow.id,
            node = %binding.handle.id,
            phases = ?phase_ids,
            "released retained delegated node before external workflow cancellation"
        );
    }
    Ok(())
}

/// Group every untorn-down phase binding by exact environment handle. A
/// healthy brokered workflow normally has one group (the same handle repeated
/// by later phases); grouping also prevents duplicate teardown if several
/// checkpoints reference it and safely exposes any historical extra lease.
fn retained_delegate_bindings(
    scoped_root: &Path,
    workflow: &OrchestratorWorkflow,
) -> Vec<(EnvironmentBinding, Vec<String>)> {
    let mut groups: Vec<(EnvironmentBinding, Vec<String>)> = Vec::new();
    for phase in &workflow.phases {
        let Some(binding) = read_checkpoint(scoped_root, &workflow.id, &phase.phase_id)
            .ok()
            .flatten()
            .and_then(|checkpoint| checkpoint.environment)
            .filter(|binding| !binding.torn_down)
        else {
            continue;
        };
        if let Some((_, phase_ids)) = groups.iter_mut().find(|(candidate, _)| {
            candidate.environment_id == binding.environment_id && candidate.handle == binding.handle
        }) {
            phase_ids.push(phase.phase_id.clone());
        } else {
            groups.push((binding, vec![phase.phase_id.clone()]));
        }
    }
    groups
}

/// Liveness of a delegated node, as observed by a trivial exec probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DelegateLiveness {
    /// The node answered a trivial exec — it is up and reusable.
    Alive,
    /// The node/plugin failed every probe with a definitive death signal.
    Dead,
    /// Could not verify (unresolvable plugin, or consistently-transient
    /// failure). Fail safe — never destroy work we cannot confirm is lost.
    Unknown,
}

/// Number of probe attempts before a delegated node is declared dead. A single
/// success at ANY attempt => Alive; only ALL attempts failing terminalizes the
/// node. This absorbs a transient relay/RPC blip (or a one-off timeout) against
/// a genuinely-live node during restart reconciliation, which would otherwise
/// false-kill an in-flight coding job — the exact work-loss this feature exists
/// to prevent.
const PROBE_ATTEMPTS: usize = 3;

/// Delay between failed probe attempts.
const PROBE_RETRY_DELAY: Duration = Duration::from_millis(750);

/// Per-attempt exec-probe timeout.
const PROBE_EXEC_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether a probe error looks like a TRANSIENT transport blip (node maybe
/// alive but slow) rather than a definitive "node is gone" signal. Mirrors
/// `EnvironmentClient::ping_is_dead`: a `Timeout` is busy/alive; a closed
/// transport (`ConnectionLost` / `ProcessExited`) is an unambiguous death.
fn probe_error_is_transient(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<orchestrator_plugin_host::HostError>(),
            Some(orchestrator_plugin_host::HostError::Timeout(_))
        )
    })
}

/// Probe a delegated node's liveness via a trivial, side-effect-free `exec` of
/// `true`, RETRIED up to [`PROBE_ATTEMPTS`] times: the first success => Alive;
/// an unresolvable plugin => Unknown; all attempts failing => Unknown when the
/// last failure looked transient (preserve + re-probe next sweep), else Dead.
pub(crate) fn probe_delegate(project_root: &str, binding: &EnvironmentBinding) -> DelegateLiveness {
    let client = match EnvironmentClient::resolve(Path::new(project_root), &binding.environment_id) {
        Ok(client) => client,
        Err(_) => return DelegateLiveness::Unknown,
    };
    probe_liveness_with_retry(PROBE_ATTEMPTS, PROBE_RETRY_DELAY, || {
        let probe =
            HarnessCommand { program: "true".to_string(), args: Vec::new(), env: Default::default(), cwd: None };
        client.exec(&binding.handle, probe, Default::default(), None, Some(PROBE_EXEC_TIMEOUT))
    })
}

/// Retry loop for [`probe_delegate`], factored out with an injectable exec
/// producer so the retry/backoff semantics are unit-testable without a live
/// plugin. `Alive` on the first `Ok`; after all attempts fail, `Unknown` when
/// the LAST failure looked transient, else `Dead`.
fn probe_liveness_with_retry<F>(attempts: usize, retry_delay: Duration, mut exec_probe: F) -> DelegateLiveness
where
    F: FnMut() -> anyhow::Result<ExecResponse>,
{
    let mut last_error_transient = false;
    for attempt in 0..attempts {
        match exec_probe() {
            Ok(_) => return DelegateLiveness::Alive,
            Err(err) => {
                last_error_transient = probe_error_is_transient(&err);
                if attempt + 1 < attempts && !retry_delay.is_zero() {
                    std::thread::sleep(retry_delay);
                }
            }
        }
    }
    if last_error_transient {
        DelegateLiveness::Unknown
    } else {
        DelegateLiveness::Dead
    }
}

/// TASK-793: whether an intent-delegated run's persisted node has DEFINITIVELY
/// died. `true` ONLY when a binding exists AND the probe returns `Dead`; a
/// missing binding, an alive node, or an unverifiable one all return `false`
/// (keep rc.24's skip). Callers must additionally require past-grace before
/// terminalizing, so a just-started delegate is never reaped.
fn delegate_is_dead(scoped_root: Option<&Path>, project_root: &str, workflow: &OrchestratorWorkflow) -> bool {
    let Some(scoped_root) = scoped_root else { return false };
    let Some((_, binding)) = current_delegate_binding(scoped_root, workflow) else { return false };
    probe_delegate(project_root, &binding) == DelegateLiveness::Dead
}

/// Reap the delegated node bound to a workflow's current phase, by its
/// persisted handle. Idempotent: teardown is dispose-by-id (a no-op if already
/// gone), and `mark_environment_torn_down` is only written on success, so a
/// failed reap is retried on the next sweep. Inert when there is no
/// (un-torn-down) binding.
fn teardown_delegated_node(project_root: &str, scoped_root: Option<&Path>, workflow: &OrchestratorWorkflow) -> bool {
    let Some(scoped_root) = scoped_root else { return true };
    let Some((phase_id, binding)) = current_delegate_binding(scoped_root, workflow) else { return true };
    match EnvironmentClient::resolve(Path::new(project_root), &binding.environment_id) {
        Ok(client) => match client.teardown(&binding.handle) {
            Ok(()) => {
                if let Err(error) = mark_environment_torn_down(scoped_root, &workflow.id, &phase_id) {
                    warn!(
                        actor = protocol::ACTOR_DAEMON,
                        workflow_id = %workflow.id,
                        %error,
                        "node was reaped but its checkpoint could not be updated; idempotent teardown will retry"
                    );
                    return false;
                }
                info!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow.id,
                    node = %binding.handle.id,
                    "reaped delegated node by persisted handle during orphan reconciliation"
                );
                true
            }
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow.id,
                    node = %binding.handle.id,
                    %error,
                    "env teardown of delegated node failed; will retry on the next sweep"
                );
                false
            }
        },
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                workflow_id = %workflow.id,
                environment = %binding.environment_id,
                %error,
                "cannot resolve environment plugin to reap delegated node; will retry on the next sweep"
            );
            false
        }
    }
}

/// TASK-811: drive a DEAD delegation ghost to a terminal state — reap the node
/// by handle, fail the phase checkpoint so it never re-surfaces for auto-resume
/// (`list_running_checkpoints` only yields `Running`), and cancel the workflow
/// via the existing orphan-cancel path (so downstream terminal projections fire
/// identically to a normal orphan cancel). Returns whether the workflow was
/// cancelled.
async fn terminalize_dead_delegation(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    scoped_root: &Path,
    workflow: &OrchestratorWorkflow,
) -> bool {
    let phase_id = current_phase_id(workflow).unwrap_or_default();
    if !teardown_delegated_node(project_root, Some(scoped_root), workflow) {
        // Keep both checkpoint and workflow Running. A terminal checkpoint is
        // omitted from future sweeps, so terminalizing here would permanently
        // suppress the promised cleanup retry and leak the node.
        return false;
    }
    let _ = update_session_failed(
        scoped_root,
        &workflow.id,
        &phase_id,
        "delegated environment node died before exec; terminalized by orphan reconciler (TASK-811)",
    );
    cancel_orphaned_running_workflow(hub, project_root, workflow).await
}

/// Normalize a subject id to its BARE form by stripping a leading `kind:`
/// prefix (`task:TASK-1` -> `TASK-1`; a bare `TASK-1` is unchanged).
///
/// A remote-animus (REQ-052) node runs the delegated workflow standalone and
/// journals its run's subject UPSTREAM into this daemon's journal in the BARE
/// form, while the delegating runner registers that subject QUALIFIED in
/// `active_subject_ids`. Comparing the two raw forms never matches, so the
/// node's own `Running` row is misclassified as a resumable orphan mid-session
/// and re-dispatched — spawning a duplicate ephemeral node every reconciliation
/// tick until dispatch is paused. Normalizing BOTH sides to bare before the
/// exclusion checks closes that fan-out loop.
fn bare_subject_id(id: &str) -> &str {
    id.split_once(':').map_or(id, |(_, rest)| rest)
}

/// Workflow id (lowercased) -> workflow-level `environment:` id, mirroring the
/// map the daemon compiles into `ProcessManager::workflow_environments` (see
/// `daemon_run.rs`): every `workflow.environment` (unfiltered) is captured so the
/// sweep classifies a run the same way dispatch's broker gate did.
///
/// TODO(codex): this classifies delegation from config ROUTING INTENT, which is
/// insufficient in three cross-cutting ways (all share one root cause — the sweep
/// needs to know whether a LIVE delegate/broker-lease actually exists, not merely
/// what config would route). Deferred here because the correct fix is a
/// cross-crate design change (thread the boot-frozen `ProcessManager` routing +
/// a broker-lease/delegation record through the sweep, or persist the resolved
/// environment on the run record) that also supersedes this pass's mandated
/// config-load + outage-suppression design:
///   * [P1 hot-reload divergence] `ProcessManager::{workflow_environments,
///     environment_routing}` are captured ONCE at daemon boot (daemon_run.rs) and
///     are NOT refreshed on hot-reload, whereas [`load_global_routing`] re-reads
///     the CURRENT config every sweep. A live `team_workflow_set` /
///     `environment_routing` edit that flips a workflow local<->remote mid-run
///     diverges from the routing that dispatched it: remote->local can reap a
///     still-live delegate; local->remote can shield a genuine local orphan.
///   * [P1 startup-resume stranding] with the durable journal active,
///     `recover_startup_orphans` (skip_live_delegated=false, resume_orphans=true)
///     PRESERVES a resumable delegated orphan (dead after broker reap) for the
///     steady-state re-dispatch leg to restart — but that leg passes
///     skip_live_delegated=true and filters it out, so a no-checkpoint remote run
///     stays `Running` forever after a restart. No single boolean is correct: a
///     LIVE delegate must be skipped (avoid a duplicate) while a REAPED one must
///     be re-dispatched, and config alone cannot tell them apart.
///   * [P2 intent != live delegate] a non-local route only expresses intent; if
///     broker start failed (documented non-fatal), the runner never started, or
///     it exited before broker `acquire`, no delegate exists yet the guard still
///     returns true and both legs skip the genuinely orphaned row forever.
fn workflow_level_environments(config: &WorkflowConfig) -> HashMap<String, String> {
    config
        .workflows
        .iter()
        .filter_map(|workflow| workflow.environment.as_ref().map(|env| (workflow.id.to_ascii_lowercase(), env.clone())))
        .collect()
}

/// Whether an in-flight run is DELEGATED to a NON-LOCAL environment (REQ-052
/// "one-id"): the portal daemon hands the WHOLE run to a remote node via
/// `EnvironmentClient::exec_session`; that node executes every phase and journals
/// home. While the delegate runs, this daemon is NOT the execution host, so its
/// LOCAL liveness signals (`active_subject_ids`, the runner-pid registry, the
/// bare-subject match) never see the run and would wrongly classify it as an
/// orphan — reaping live work on every nudge. The delegate owns the run's
/// lifecycle, so the sweep must never cancel or re-dispatch it.
///
/// The decision MIRRORS `ProcessManager::configure_environment_broker`: resolve
/// the run's environment from the SAME inputs (subject kind, workflow-level
/// `environment:`, config `environment_routing`) and treat it as delegated iff
/// the resolved id is present AND not a host-only (`local`/`worktree`) id. This
/// covers routing-rule / routing-default delegation (not just the workflow-level
/// binding) and, conversely, never shields a run that resolves to a LOCAL env.
/// See the `TODO(codex)` on [`workflow_level_environments`] for the known limits
/// of classifying from config intent (hot-reload divergence, startup-resume
/// stranding, and intent-without-a-live-delegate) and the recommended fix.
fn is_delegated_run(
    workflow: &OrchestratorWorkflow,
    workflow_environments: &HashMap<String, String>,
    environment_routing: Option<&orchestrator_config::workflow_config::EnvironmentRouting>,
) -> bool {
    let workflow_env = workflow
        .workflow_ref
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .and_then(|r| workflow_environments.get(&r.to_ascii_lowercase()).map(String::as_str));
    match orchestrator_config::workflow_config::resolve_environment(
        workflow.subject.as_ref().map(|subject| subject.kind()),
        None,
        None,
        workflow_env,
        environment_routing,
    ) {
        Some(environment_id) => !orchestrator_daemon_runtime::is_local_environment(&environment_id),
        None => false,
    }
}

/// The environment-routing inputs the orphan sweep needs to classify a run as
/// delegated, or a signal to SUPPRESS reconciliation this tick.
enum RoutingLoad {
    /// Routing resolved (empty when no `config_source` is installed — the stock
    /// scaffold: nothing routes non-local, so the sweep behaves as pre-fix).
    Available {
        workflow_environments: HashMap<String, String>,
        environment_routing: Option<orchestrator_config::workflow_config::EnvironmentRouting>,
    },
    /// A `config_source` plugin IS present but its load failed (transient spawn /
    /// RPC / DB-overload / validation). Delegation cannot be determined, and a
    /// delegated run intentionally has NO local liveness — so cancelling or
    /// re-dispatching it now would reap live remote work. The caller must leave
    /// such runs untouched this tick (they retry on the next heartbeat).
    Unavailable,
}

/// Load the GLOBAL (`actor = None`) routing tables — the SAME source the broker
/// gates on. `daemon_run.rs` builds `ProcessManager::workflow_environments` +
/// `environment_routing` from `load_workflow_config_or_default` (global) once at
/// boot, so the sweep MUST classify delegation from the global partition to
/// mirror the broker (a per-actor partition would DIVERGE from what actually
/// brokered the run). Uses the non-swallowing [`try_load_workflow_config`] so a
/// config-source OUTAGE is distinguished from a genuinely absent source: on an
/// outage the caller suppresses the sweep rather than risk reaping a run whose
/// remote delegate is still alive.
fn load_global_routing(project_root: &Path) -> RoutingLoad {
    use orchestrator_config::workflow_config::WorkflowConfigAvailability;
    match orchestrator_config::workflow_config::try_load_workflow_config(project_root, None) {
        WorkflowConfigAvailability::Loaded(loaded) => RoutingLoad::Available {
            workflow_environments: workflow_level_environments(&loaded.config),
            environment_routing: loaded.config.environment_routing.clone(),
        },
        WorkflowConfigAvailability::NoSource => {
            RoutingLoad::Available { workflow_environments: HashMap::new(), environment_routing: None }
        }
        WorkflowConfigAvailability::SourceUnavailable(_) => RoutingLoad::Unavailable,
    }
}

/// `skip_live_delegated` selects WHOSE authority owns a delegated
/// (environment-bound, REQ-052 "one-id") `Running` run at this call site:
///   * `true` (STEADY-STATE, the heartbeat sweep): the remote delegate is ALIVE
///     on its node and journals home; this daemon's LOCAL liveness signals never
///     see it, so it must be SKIPPED (never cancelled/re-dispatched) or the sweep
///     reaps live remote work every nudge. This loads the global routing tables
///     to classify delegation and, on a config-source OUTAGE, suppresses the
///     whole sweep this tick (a delegated run has no local liveness — reaping it
///     blind would kill a live delegate).
///   * `false` (STARTUP, `recover_startup_orphans`): `EnvironmentBroker::start`
///     → `reap_prior_daemon_records` has ALREADY torn down the prior daemon
///     instance's remote nodes at boot, so a delegated `Running` row surviving
///     into startup is genuinely DEAD and MUST be handled here (resumed or
///     cancelled) — skipping it would strand it `Running` forever. In this mode
///     the function loads NO config and applies NO delegated-skip: it is
///     byte-identical to the pre-fix behavior (zero added work).
pub async fn recover_orphaned_running_workflows(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_subject_ids: &HashSet<String>,
    resume_orphans: bool,
    skip_live_delegated: bool,
) -> usize {
    let workflows = match hub.workflows().list_active().await {
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
    // Nothing to reconcile — skip all routing work so an idle daemon does zero
    // extra work per sweep.
    if workflows.is_empty() {
        return 0;
    }
    // Only the STEADY-STATE sweep (`skip_live_delegated`) protects delegated
    // runs; STARTUP must handle them (the broker already reaped the prior
    // instance's nodes at boot — see this fn's doc). When shielding is off we
    // load NO routing and skip NO run, staying byte-identical to pre-fix.
    // Resolve delegation from the GLOBAL routing tables (the same source the
    // broker gates on; see `load_global_routing`). On a config-source OUTAGE we
    // cannot classify delegation, so suppress the WHOLE sweep this tick rather
    // than risk reaping a run whose remote delegate is still alive.
    let routing = if skip_live_delegated {
        match load_global_routing(Path::new(project_root)) {
            RoutingLoad::Available { workflow_environments, environment_routing } => {
                Some((workflow_environments, environment_routing))
            }
            RoutingLoad::Unavailable => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    "skipping orphan sweep this tick: config source unavailable (cannot classify delegation)"
                );
                emit_reconcile_suppressed("cancel", "config-source-unavailable");
                return 0;
            }
        }
    } else {
        None
    };
    // Snapshot pid-file liveness for every Running workflow BEFORE
    // `active_workflow_runner_ids` prunes stale files (see `snapshot_runner_liveness`).
    let runner_liveness = snapshot_runner_liveness(Path::new(project_root), &workflows);
    let trace = reconcile_trace_enabled();
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
    // Compare a node's BARE upstream-journaled subject against the delegating
    // runner's QUALIFIED active id (see `bare_subject_id`).
    let active_subject_bare: HashSet<&str> = active_subject_ids.iter().map(|s| bare_subject_id(s)).collect();
    // TASK-933/793/811: the delegated-node binding lives in the scoped session
    // checkpoints. `None` (no scope) => every delegate helper is inert and the
    // sweep behaves exactly as rc.24.
    let scoped_root = protocol::scoped_state_root(Path::new(project_root));

    let mut recovered = 0usize;
    let mut evaluated = 0usize;
    let mut skipped_delegated = 0usize;
    let mut protected = 0usize;
    for workflow in workflows {
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        evaluated += 1;

        // Resolve every guard boolean up front so both the log record and the
        // action below read from the SAME facts (the logged decision can never
        // diverge from what the sweep does). Precedence MIRRORS the original
        // guard chain: merge-conflict -> waiting-manual -> live-runner /
        // active-subject -> delegated -> grace -> resumable-preserve -> cancel.
        let liveness = runner_liveness.get(&workflow.id).cloned().unwrap_or_default();
        let waiting_manual = workflow_is_waiting_on_manual_phase(project_root, &workflow);
        let in_active_subjects = active_subject_ids.contains(&workflow.id)
            || workflow.subject.as_ref().is_some_and(|s| active_subject_bare.contains(bare_subject_id(s.id())));
        let in_runner_registry = externally_active_workflows.contains(&workflow.id);
        let is_delegated = routing
            .as_ref()
            .is_some_and(|(envs, environment_routing)| is_delegated_run(&workflow, envs, environment_routing.as_ref()));
        let age_secs = (now - workflow.started_at).num_seconds();
        let within_grace = age_secs < ORPHAN_RECONCILIATION_GRACE_SECS;
        let resumable = is_resumable_orphan(&workflow);
        // A persisted binding is stronger evidence than current routing
        // intent. In particular startup deliberately does not load routing,
        // and configuration may have changed since this run was dispatched.
        // Always liveness-gate such a delegate before the generic
        // journal-resume preserve branch, otherwise a dead node is preserved
        // forever across every daemon restart.
        let dead_delegate = !within_grace && delegate_is_dead(scoped_root.as_deref(), project_root, &workflow);

        let decision = if workflow.machine_state == WorkflowMachineState::MergeConflict {
            ReconcileDecision::MergeConflict
        } else if waiting_manual {
            ReconcileDecision::WaitingManual
        } else if in_runner_registry {
            ReconcileDecision::ProtectedLiveRunner
        } else if dead_delegate {
            ReconcileDecision::TerminalizeDeadDelegate
        } else if in_active_subjects {
            ReconcileDecision::ProtectedActiveSubject
        } else if is_delegated {
            // Alive / unverifiable / no-binding (and any within-grace
            // delegate) keep rc.24's fail-safe skip. Definitively dead
            // persisted bindings were handled above, including at startup
            // where routing is intentionally unavailable.
            ReconcileDecision::SkippedDelegated
        } else if within_grace {
            ReconcileDecision::WithinGrace
        } else if resume_orphans && resumable {
            ReconcileDecision::PreservedResumable
        } else {
            ReconcileDecision::CancelledOrphan
        };

        if trace || decision.always_emit() {
            emit_reconcile_decision(
                "cancel",
                &workflow,
                decision,
                age_secs,
                in_active_subjects,
                in_runner_registry,
                is_delegated,
                resumable,
                waiting_manual,
                &liveness,
            );
        }

        match decision {
            ReconcileDecision::ProtectedLiveRunner | ReconcileDecision::ProtectedActiveSubject => {
                protected += 1;
            }
            ReconcileDecision::SkippedDelegated => {
                skipped_delegated += 1;
                // A delegated run stays Running for its whole remote execution;
                // keep the tracing echo at `debug` (out of the default stream).
                debug!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow.id,
                    workflow_ref = workflow.workflow_ref.as_deref().unwrap_or_default(),
                    "skipping orphan sweep for delegated/environment-bound run"
                );
            }
            ReconcileDecision::TerminalizeDeadDelegate => {
                // TASK-793/811: the delegate's node probed dead — reap it, fail
                // the checkpoint, and cancel the workflow so it never re-surfaces
                // as a phantom Running lease.
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow.id,
                    workflow_ref = workflow.workflow_ref.as_deref().unwrap_or_default(),
                    "delegated node probed dead; terminalizing ghost + reaping node (TASK-793/811)"
                );
                if let Some(scoped_root) = scoped_root.as_deref() {
                    if terminalize_dead_delegation(hub.clone(), project_root, scoped_root, &workflow).await {
                        recovered = recovered.saturating_add(1);
                    } else {
                        error!(
                            actor = protocol::ACTOR_DAEMON,
                            workflow_id = %workflow.id,
                            "failed to cancel dead delegated workflow"
                        );
                    }
                }
            }
            ReconcileDecision::MergeConflict
            | ReconcileDecision::WaitingManual
            | ReconcileDecision::WithinGrace
            // These variants never arise on the cancel leg (redispatch- and
            // terminal-env-only), but are matched exhaustively; they are no-ops
            // here.
            | ReconcileDecision::RedispatchCandidate
            | ReconcileDecision::SkippedLiveOrphan
            | ReconcileDecision::SkippedBlockedResume
            | ReconcileDecision::NotResumable
            | ReconcileDecision::TeardownTerminalLease
            | ReconcileDecision::TerminalLeaseTeardownFailed
            | ReconcileDecision::SkippedLeaseRunActive
            | ReconcileDecision::SkippedLeaseRunUnknown
            | ReconcileDecision::TerminalLeaseWithinGrace => {}
            ReconcileDecision::PreservedResumable => {
                // BU-4: with the durable journal active (kill-switch off), do NOT
                // destroy resumable in-flight work on daemon restart/redeploy.
                // Preserve the Running run for resume — MID-PHASE via
                // `auto_resume_running_checkpoints`, or from its current PHASE
                // BOUNDARY via `resumable_orphans_for_redispatch`.
                info!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow.id,
                    subject_id = %workflow.subject.as_ref().map(|s| s.id()).unwrap_or_default(),
                    task_id = %workflow.task_id,
                    current_phase = workflow.current_phase.as_deref().unwrap_or_default(),
                    "preserving resumable in-flight workflow for resume (durable journal active); not cancelling"
                );
            }
            ReconcileDecision::CancelledOrphan => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow.id,
                    subject_id = %workflow.subject.as_ref().map(|s| s.id()).unwrap_or_default(),
                    task_id = %workflow.task_id,
                    "recovering orphaned running workflow"
                );
                // HALF A leak-killer: reap any persisted delegated node BEFORE
                // cancel so an orphan (e.g. a dead delegate handled at startup,
                // where intent-gating is off) does not leak its node. Inert for
                // local runs and until the companion runner persists a binding.
                if !teardown_delegated_node(project_root, scoped_root.as_deref(), &workflow) {
                    // Cancellation makes the workflow/checkpoint terminal and
                    // removes it from subsequent orphan sweeps. Keep it Running
                    // when cleanup fails so the persisted binding remains a
                    // retryable cleanup obligation instead of becoming a
                    // permanently leaked node.
                    error!(
                        actor = protocol::ACTOR_DAEMON,
                        workflow_id = %workflow.id,
                        "delegated node cleanup failed; deferring orphan cancellation so teardown can retry"
                    );
                    continue;
                }
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
        }
    }

    emit_reconcile_sweep_summary(
        "cancel",
        evaluated,
        recovered,
        skipped_delegated,
        protected,
        &externally_active_workflows,
        "",
    );
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
///
/// `skip_live_delegated` matches [`recover_orphaned_running_workflows`]: the
/// steady-state dispatch leg passes `true` so a run whose remote delegate is
/// ALIVE is never re-dispatched locally (which would spawn a duplicate runner),
/// with the same config-outage suppression. `false` loads no routing and skips
/// nothing (byte-identical to pre-fix); startup never calls this leg.
pub(crate) async fn resumable_orphans_for_redispatch(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    active_subject_ids: &HashSet<String>,
    skip_live_delegated: bool,
) -> Vec<OrchestratorWorkflow> {
    if !journal_resume_enabled(project_root) {
        return Vec::new();
    }
    let workflows = match hub.workflows().list_active().await {
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
    // Nothing running — skip the routing work + orphan scans entirely.
    if workflows.is_empty() {
        return Vec::new();
    }
    // Same GLOBAL delegation classification as the cancel sweep, and gated the
    // same way: only when `skip_live_delegated` do we load routing and shield
    // delegated runs. A run delegated to a remote node must never be
    // re-dispatched locally (that would spawn a duplicate). On a config-source
    // outage, suppress re-dispatch this tick (see `load_global_routing`).
    let routing = if skip_live_delegated {
        match load_global_routing(Path::new(project_root)) {
            RoutingLoad::Available { workflow_environments, environment_routing } => {
                Some((workflow_environments, environment_routing))
            }
            RoutingLoad::Unavailable => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    "suppressing journal-resume re-dispatch this tick: config source unavailable"
                );
                emit_reconcile_suppressed("redispatch", "config-source-unavailable");
                return Vec::new();
            }
        }
    } else {
        None
    };
    // Snapshot pid-file liveness before `active_workflow_runner_ids` prunes stale
    // files (see `snapshot_runner_liveness`).
    let runner_liveness = snapshot_runner_liveness(Path::new(project_root), &workflows);
    let trace = reconcile_trace_enabled();
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
    // Normalize to BARE form so a node's upstream-journaled subject matches the
    // delegating runner's QUALIFIED active/orphan ids (see `bare_subject_id`).
    let active_subject_bare: HashSet<&str> = active_subject_ids.iter().map(|s| bare_subject_id(s)).collect();
    let live_orphan_bare: HashSet<&str> = live_orphan_subjects.iter().map(|s| bare_subject_id(s)).collect();
    let scoped_root = protocol::scoped_state_root(Path::new(project_root));

    let mut candidates = Vec::new();
    let mut evaluated = 0usize;
    let mut selected = 0usize;
    let mut skipped_delegated = 0usize;
    let mut protected = 0usize;
    for workflow in workflows {
        if workflow.status != WorkflowStatus::Running {
            continue;
        }
        evaluated += 1;

        // Resolve all guard booleans up front so the log record and the
        // selection read the SAME facts. Precedence MIRRORS the guard chain:
        // merge-conflict -> waiting-manual -> live-runner / active-subject ->
        // delegated -> live-orphan -> blocked-resume -> grace -> resumable.
        let liveness = runner_liveness.get(&workflow.id).cloned().unwrap_or_default();
        let waiting_manual = workflow_is_waiting_on_manual_phase(project_root, &workflow);
        let in_active_subjects = active_subject_ids.contains(&workflow.id)
            || workflow.subject.as_ref().is_some_and(|s| active_subject_bare.contains(bare_subject_id(s.id())));
        let in_runner_registry = externally_active_workflows.contains(&workflow.id);
        let is_delegated = routing
            .as_ref()
            .is_some_and(|(envs, environment_routing)| is_delegated_run(&workflow, envs, environment_routing.as_ref()));
        let live_orphan = workflow.subject.as_ref().is_some_and(|s| live_orphan_bare.contains(bare_subject_id(s.id())))
            || (!workflow.task_id.is_empty() && live_orphan_bare.contains(bare_subject_id(&workflow.task_id)));
        let blocked_resume = blocked_resume_workflows.contains(&workflow.id);
        let age_secs = (now - workflow.started_at).num_seconds();
        let within_grace = age_secs < ORPHAN_RECONCILIATION_GRACE_SECS;
        let resumable = is_resumable_orphan(&workflow);
        let dead_delegate = !within_grace && delegate_is_dead(scoped_root.as_deref(), project_root, &workflow);

        let decision = if workflow.machine_state == WorkflowMachineState::MergeConflict {
            ReconcileDecision::MergeConflict
        } else if waiting_manual {
            ReconcileDecision::WaitingManual
        } else if in_runner_registry {
            ReconcileDecision::ProtectedLiveRunner
        } else if dead_delegate {
            ReconcileDecision::TerminalizeDeadDelegate
        } else if in_active_subjects {
            ReconcileDecision::ProtectedActiveSubject
        } else if is_delegated {
            ReconcileDecision::SkippedDelegated
        } else if live_orphan {
            ReconcileDecision::SkippedLiveOrphan
        } else if blocked_resume {
            ReconcileDecision::SkippedBlockedResume
        } else if within_grace {
            ReconcileDecision::WithinGrace
        } else if !resumable {
            ReconcileDecision::NotResumable
        } else {
            ReconcileDecision::RedispatchCandidate
        };

        if trace || decision.always_emit() {
            emit_reconcile_decision(
                "redispatch",
                &workflow,
                decision,
                age_secs,
                in_active_subjects,
                in_runner_registry,
                is_delegated,
                resumable,
                waiting_manual,
                &liveness,
            );
        }

        match decision {
            ReconcileDecision::ProtectedLiveRunner | ReconcileDecision::ProtectedActiveSubject => {
                protected += 1;
            }
            ReconcileDecision::SkippedDelegated => {
                skipped_delegated += 1;
                debug!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow.id,
                    workflow_ref = workflow.workflow_ref.as_deref().unwrap_or_default(),
                    "skipping orphan re-dispatch for delegated/environment-bound run"
                );
            }
            ReconcileDecision::RedispatchCandidate => {
                selected += 1;
                candidates.push(workflow);
            }
            ReconcileDecision::TerminalizeDeadDelegate => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %workflow.id,
                    workflow_ref = workflow.workflow_ref.as_deref().unwrap_or_default(),
                    "dead delegated node reached redispatch sweep; terminalizing instead of spawning a replacement"
                );
                if let Some(scoped_root) = scoped_root.as_deref() {
                    let _ = terminalize_dead_delegation(hub.clone(), project_root, scoped_root, &workflow).await;
                }
            }
            // All other outcomes exclude the run from re-dispatch (no-op). A
            // dead delegate normally gets terminalized by the cancel leg first,
            // but this leg independently applies the same gate so callers
            // cannot re-dispatch-and-leak when invoked on its own. The
            // terminal-env-only variants never arise here either.
            ReconcileDecision::MergeConflict
            | ReconcileDecision::WaitingManual
            | ReconcileDecision::SkippedLiveOrphan
            | ReconcileDecision::SkippedBlockedResume
            | ReconcileDecision::WithinGrace
            | ReconcileDecision::NotResumable
            | ReconcileDecision::PreservedResumable
            | ReconcileDecision::CancelledOrphan
            | ReconcileDecision::TeardownTerminalLease
            | ReconcileDecision::TerminalLeaseTeardownFailed
            | ReconcileDecision::SkippedLeaseRunActive
            | ReconcileDecision::SkippedLeaseRunUnknown
            | ReconcileDecision::TerminalLeaseWithinGrace => {}
        }
    }
    emit_reconcile_sweep_summary(
        "redispatch",
        evaluated,
        0,
        skipped_delegated,
        protected,
        &externally_active_workflows,
        &format!("selected={selected}"),
    );
    candidates
}

pub async fn reconcile_manual_phase_timeouts(hub: Arc<dyn ServiceHub>, project_root: &str) -> Result<usize> {
    let runtime = load_agent_runtime_config_or_default(Path::new(project_root));
    let workflows = match hub.workflows().list_active().await {
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

// ---------------------------------------------------------------------------
// TASK-1466: terminal-run environment teardown sweep.
//
// Node teardown on workflow completion is driven by the OWNING runner/plugin
// (terminal projection -> `ProcessManager::teardown_environment_if_terminal`
// -> `EnvironmentBroker::teardown`). When that owner DIES with the run — a
// phase failure kills the runner, the plugin restarts, or the daemon dies
// between terminal projection and teardown — nothing ever tears the node
// down: the broker retries a failed teardown record only at daemon STARTUP
// (`reap_prior_daemon_records`), so at steady state the node stays alive on
// the provider and burns compute indefinitely.
//
// This leg closes the gap: enumerate every durable UNTORN-DOWN environment
// lease (broker lease records AND phase-session checkpoint bindings), look up
// the owning run in the journal, and when the row is TERMINAL drive teardown
// through the environment plugin exactly as the runner would have
// (`EnvironmentClient::resolve` + `client.teardown(&handle)` — the plugin's
// pre-teardown cleanup/publish hook is preserved; no direct node deletion).
// On teardown/plugin failure the durable record is left in place so the NEXT
// sweep retries, and a `reconcile-decision` line carries the reason.
//
// Double-teardown safety: the leg acts ONLY on a journal-confirmed terminal
// row, and only after [`TERMINAL_ENV_TEARDOWN_GRACE_SECS`] has elapsed since
// `completed_at` — the normal owner-driven teardown fires synchronously as
// the run lands terminal, so the floor keeps the sweep from doubling an
// in-flight teardown. Past the floor a terminal run has no live owner left to
// race. Teardown itself is dispose-by-id idempotent and the broker tolerates
// torn-down leases, so a residual race is safe.
// ---------------------------------------------------------------------------

/// Grace after a run lands TERMINAL in the journal before this sweep drives
/// teardown itself (see the block comment above).
const TERMINAL_ENV_TEARDOWN_GRACE_SECS: i64 = 90;

/// One untorn-down environment lease attributed to a workflow run, assembled
/// from the durable broker lease records and/or the phase-session checkpoint
/// bindings (deduplicated by exact handle).
struct RetainedEnvironmentLease {
    run_id: String,
    environment_id: String,
    handle: EnvironmentHandle,
    /// Root the environment client resolves against (the broker record's own
    /// `project_root`, or the sweep's root for checkpoint-only leases).
    resolve_root: String,
    /// Checkpoint phases referencing this exact handle (marked torn down
    /// after a successful teardown).
    phase_ids: Vec<String>,
    /// Whether a durable broker lease record exists for this run (deleted
    /// after a successful teardown).
    broker_record: bool,
    /// Where the lease was found, for the decision log.
    source: &'static str,
    /// Broker record state or checkpoint status, for the decision log.
    record_state: String,
    /// Best available lease timestamp (record `updated_at` / binding
    /// `bound_at`); the grace-floor fallback when the terminal journal row
    /// has no `completed_at`.
    reference_ts: Option<chrono::DateTime<chrono::Utc>>,
}

fn parse_rfc3339(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts).map(|dt| dt.with_timezone(&chrono::Utc)).ok()
}

/// Enumerate every durable untorn-down environment lease: broker lease
/// records in any non-`TornDown` state with a persisted handle, plus
/// phase-session checkpoint bindings with `torn_down == false`. The two
/// sources overlap (the broker binds the Running phase checkpoint at
/// acquire); entries are merged by exact handle so one node is torn down
/// once per sweep.
fn retained_environment_leases(scoped_root: &Path, project_root: &str) -> Vec<RetainedEnvironmentLease> {
    let mut leases: Vec<RetainedEnvironmentLease> = Vec::new();
    for record in orchestrator_daemon_runtime::list_environment_lease_records(project_root) {
        if matches!(record.state, orchestrator_daemon_runtime::BrokerLeaseState::TornDown) {
            continue;
        }
        let Some(handle) = record.handle else { continue };
        leases.push(RetainedEnvironmentLease {
            run_id: record.run_id,
            environment_id: record.environment_id,
            handle,
            resolve_root: record.project_root,
            phase_ids: Vec::new(),
            broker_record: true,
            source: "broker-lease",
            record_state: record.state.label().to_string(),
            reference_ts: parse_rfc3339(&record.updated_at),
        });
    }
    let checkpoints = match list_checkpoints_with_retained_environment(scoped_root) {
        Ok(checkpoints) => checkpoints,
        Err(error) => {
            warn!(
                actor = protocol::ACTOR_DAEMON,
                %error,
                "failed to list phase checkpoints for terminal environment teardown sweep; broker leases only this sweep"
            );
            Vec::new()
        }
    };
    for (_, checkpoint) in checkpoints {
        let Some(binding) = checkpoint.environment.filter(|binding| !binding.torn_down) else { continue };
        if let Some(lease) = leases.iter_mut().find(|lease| {
            lease.run_id == checkpoint.workflow_id
                && lease.environment_id == binding.environment_id
                && lease.handle == binding.handle
        }) {
            lease.phase_ids.push(checkpoint.phase_id);
            lease.source = "broker-lease+phase-checkpoint";
            continue;
        }
        leases.push(RetainedEnvironmentLease {
            run_id: checkpoint.workflow_id,
            environment_id: binding.environment_id,
            handle: binding.handle,
            resolve_root: project_root.to_string(),
            phase_ids: vec![checkpoint.phase_id],
            broker_record: false,
            source: "phase-checkpoint",
            record_state: format!("{:?}", checkpoint.status),
            reference_ts: parse_rfc3339(&binding.bound_at),
        });
    }
    leases
}

/// Emit one greppable single-line `reconcile-decision` record for the
/// terminal-env sweep (same key=value shape as the other sweeps).
fn emit_terminal_env_decision(
    lease: &RetainedEnvironmentLease,
    run_status: Option<WorkflowStatus>,
    age_secs: i64,
    decision: ReconcileDecision,
    reason: &str,
) {
    let reason = reason.replace(['\n', '\r'], " ");
    println!(
        "reconcile-decision ts={ts} sweep=terminal-env workflow_id={workflow_id} run_status={run_status} \
         source={source} record_state={record_state} environment_id={environment_id} handle_id={handle_id} \
         age_secs={age_secs} decision={decision} reason={reason}",
        ts = chrono::Utc::now().to_rfc3339(),
        workflow_id = log_field(&lease.run_id),
        run_status = run_status.map(|status| format!("{status:?}")).unwrap_or_else(|| "-".to_string()),
        source = lease.source,
        record_state = log_field(&lease.record_state),
        environment_id = log_field(&lease.environment_id),
        handle_id = log_field(&lease.handle.id),
        decision = decision.label(),
    );
}

/// TASK-1466: tear down environment leases whose owning workflow run is
/// TERMINAL in the journal but was never torn down (owner died before
/// teardown). Returns the number of leases torn down this sweep. See the
/// block comment above for the model and safety arguments.
pub async fn reconcile_terminal_environment_leases(hub: Arc<dyn ServiceHub>, project_root: &str) -> usize {
    reconcile_terminal_environment_leases_with(hub, project_root, |resolve_root, environment_id, handle| {
        let client = EnvironmentClient::resolve(Path::new(resolve_root), environment_id).with_context(|| {
            format!("cannot resolve environment plugin '{environment_id}' to tear down retained node {}", handle.id)
        })?;
        client.teardown(handle).with_context(|| {
            format!(
                "failed to teardown retained node {handle_id} in environment '{environment_id}'",
                handle_id = handle.id
            )
        })
    })
    .await
}

/// The sweep with the environment teardown call injected, so tests can drive
/// it without an installed environment plugin (mirrors
/// [`teardown_retained_environment_with`]).
async fn reconcile_terminal_environment_leases_with<F>(
    hub: Arc<dyn ServiceHub>,
    project_root: &str,
    teardown: F,
) -> usize
where
    F: FnMut(&str, &str, &EnvironmentHandle) -> anyhow::Result<()>,
{
    let mut teardown = teardown;
    let Some(scoped_root) = protocol::scoped_state_root(Path::new(project_root)) else {
        return 0;
    };
    let candidates = retained_environment_leases(&scoped_root, project_root);
    if candidates.is_empty() {
        return 0;
    }
    let trace = reconcile_trace_enabled();
    let now = chrono::Utc::now();
    let mut torn_down = 0usize;
    for lease in candidates {
        let workflow = match hub.workflows().get(&lease.run_id).await {
            Ok(workflow) => workflow,
            Err(error) => {
                // Fail safe: without a journal row terminality cannot be
                // confirmed, so the lease stays a retryable obligation.
                if trace {
                    emit_terminal_env_decision(
                        &lease,
                        None,
                        -1,
                        ReconcileDecision::SkippedLeaseRunUnknown,
                        &format!("workflow lookup failed: {error:#}"),
                    );
                }
                continue;
            }
        };
        if !orchestrator_core::is_terminal_workflow_run_status(workflow.status) {
            // A live (or merely non-terminal) owner may still tear this lease
            // down itself; never race it.
            if trace {
                emit_terminal_env_decision(
                    &lease,
                    Some(workflow.status),
                    -1,
                    ReconcileDecision::SkippedLeaseRunActive,
                    "run is not terminal",
                );
            }
            continue;
        }
        let terminal_at = workflow.completed_at.or(lease.reference_ts);
        let age_secs = terminal_at.map(|ts| (now - ts).num_seconds()).unwrap_or(i64::MAX);
        if age_secs < TERMINAL_ENV_TEARDOWN_GRACE_SECS {
            // The normal owner-driven teardown fires as the run lands
            // terminal; give it first chance before doubling it.
            if trace {
                emit_terminal_env_decision(
                    &lease,
                    Some(workflow.status),
                    age_secs,
                    ReconcileDecision::TerminalLeaseWithinGrace,
                    "terminal run within teardown grace floor",
                );
            }
            continue;
        }
        match teardown(&lease.resolve_root, &lease.environment_id, &lease.handle) {
            Ok(()) => {
                // Mark every checkpoint binding torn down FIRST; only when all
                // marks landed is the broker record deleted, so a mark failure
                // keeps a durable retry obligation (the retried teardown is
                // idempotent).
                let mut all_marked = true;
                for phase_id in &lease.phase_ids {
                    if let Err(error) = mark_environment_torn_down(&scoped_root, &lease.run_id, phase_id) {
                        all_marked = false;
                        warn!(
                            actor = protocol::ACTOR_DAEMON,
                            workflow_id = %lease.run_id,
                            node = %lease.handle.id,
                            %error,
                            "node was torn down but its checkpoint could not be marked; idempotent teardown will retry"
                        );
                    }
                }
                if all_marked && lease.broker_record {
                    orchestrator_daemon_runtime::remove_environment_lease_record(project_root, &lease.run_id);
                }
                info!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %lease.run_id,
                    node = %lease.handle.id,
                    source = lease.source,
                    "tore down terminal workflow's retained environment node (TASK-1466)"
                );
                emit_terminal_env_decision(
                    &lease,
                    Some(workflow.status),
                    age_secs,
                    ReconcileDecision::TeardownTerminalLease,
                    "terminal run retained an untorn-down environment lease",
                );
                torn_down = torn_down.saturating_add(1);
            }
            Err(error) => {
                warn!(
                    actor = protocol::ACTOR_DAEMON,
                    workflow_id = %lease.run_id,
                    node = %lease.handle.id,
                    %error,
                    "terminal-run environment teardown failed; retaining the durable record for the next sweep"
                );
                emit_terminal_env_decision(
                    &lease,
                    Some(workflow.status),
                    age_secs,
                    ReconcileDecision::TerminalLeaseTeardownFailed,
                    &format!("{error:#}"),
                );
            }
        }
    }
    torn_down
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

    // REQ-052 fan-out fix: a delegating runner registers a QUALIFIED subject id
    // (`task:TASK-1`) in `active_subject_ids`, while the remote node journals
    // the same run's subject UPSTREAM in the BARE form (`TASK-1`). The orphan
    // sweep's exclusion checks now normalize BOTH sides via `bare_subject_id`
    // so they match; without it the node's own Running row was re-dispatched
    // every tick, spawning a duplicate node.
    #[test]
    fn bare_subject_id_strips_leading_kind_prefix() {
        use super::bare_subject_id;
        assert_eq!(bare_subject_id("task:TASK-632"), "TASK-632");
        assert_eq!(bare_subject_id("TASK-632"), "TASK-632"); // already bare — unchanged
        assert_eq!(bare_subject_id("requirement:REQ-1"), "REQ-1");
        assert_eq!(bare_subject_id("transcript:TRANSCRIPT-001"), "TRANSCRIPT-001");
        // only the FIRST ':' is the kind separator; the remainder is preserved
        assert_eq!(bare_subject_id("task:weird:id"), "weird:id");
        assert_eq!(bare_subject_id(""), "");
        // the qualified/bare pair that caused the fan-out must normalize equal
        assert_eq!(bare_subject_id("task:TASK-632"), bare_subject_id("TASK-632"));
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
        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false, false).await;
        assert_eq!(recovered, 0, "live runner pid must shield the resumed workflow");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running);

        // Once the runner is gone, the same workflow is reconciled.
        unregister_workflow_runner_pid(temp.path(), &workflow.id).expect("pid should unregister");
        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false, false).await;
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

        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), true, false).await;
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
        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), true, false).await;
        assert_eq!(recovered, 0, "a run with a live runner must be skipped, not cancelled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running);

        // And the re-dispatch leg must NOT select a run whose runner is live
        // (no double-dispatch). Even with the journal gate, the active-runner
        // guard excludes it; on the SQLite test path the gate already returns
        // empty, which is the stronger safety guarantee asserted below.
        let candidates = resumable_orphans_for_redispatch(hub.clone(), &project_root, &HashSet::new(), false).await;
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

        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), true, false).await;
        assert_eq!(recovered, 0, "terminal runs must be ignored by the resume sweep");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Completed, "terminal run must be left untouched");

        let candidates = resumable_orphans_for_redispatch(hub.clone(), &project_root, &HashSet::new(), false).await;
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

        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), true, false).await;
        assert_eq!(recovered, 0, "a run within the grace window must not be cancelled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running);

        let candidates = resumable_orphans_for_redispatch(hub.clone(), &project_root, &HashSet::new(), false).await;
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
        let candidates = resumable_orphans_for_redispatch(hub.clone(), &project_root, &HashSet::new(), false).await;
        assert!(candidates.is_empty(), "re-dispatch must be inert without a durable journal");

        // resume_orphans=false reproduces the exact pre-BU-4 cancel behavior.
        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false, false).await;
        assert_eq!(recovered, 1, "SQLite orphan without a live runner must still be cancelled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Cancelled);
    }

    /// Re-install the `config_source` base for `temp`, applying `mutate` to the
    /// compiled base first — so a test can bind the fixture run's workflow to an
    /// `environment:` (or set `environment_routing`) and drive the reconciler's
    /// real delegation classification. The returned guard must outlive the sweep
    /// (the last install for a root wins while it is alive).
    fn rebind_config_base(
        temp: &TempDir,
        mutate: impl FnOnce(&mut orchestrator_core::WorkflowConfig),
    ) -> orchestrator_config::workflow_config::config_source_client::test_seam::TestBaseGuard {
        let mut base =
            orchestrator_config::workflow_config::config_source_client::test_seam::base_for(temp.path(), None)
                .expect("fixture installed a config_source base");
        mutate(&mut base);
        orchestrator_config::workflow_config::config_source_client::test_seam::install(temp.path(), base)
    }

    // TASK-750 core: at STEADY-STATE (`skip_live_delegated=true`) a DELEGATED
    // (environment-bound) run executes on a LIVE remote node and journals home,
    // so this daemon's LOCAL liveness signals never see it. Even absent from
    // `active_subject_ids` and the pid registry and well past the grace window,
    // the cancel sweep must SKIP it — reaping it kills live remote work on every
    // daemon nudge. `resume_orphans=false` (the pre-BU-4 cancel path) proves the
    // skip is independent of the journal-resume gate: the delegated-run guard
    // alone shields the run.
    #[tokio::test]
    async fn environment_bound_run_is_not_cancelled_by_orphan_sweep() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        // Bind the run's own workflow to a remote (non-local) `environment:` so
        // the reconciler resolves it as delegated, exactly like the broker.
        let workflow_ref = hub
            .workflows()
            .get(&workflow_id)
            .await
            .expect("workflow should load")
            .workflow_ref
            .expect("fixture run carries a workflow_ref");
        let _env_guard = rebind_config_base(&temp, |base| {
            let mut bound = false;
            for workflow in &mut base.workflows {
                if workflow.id == workflow_ref {
                    workflow.environment = Some("railway".to_string());
                    bound = true;
                }
            }
            assert!(bound, "the run's workflow '{workflow_ref}' must exist in the base to bind an environment");
        });

        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false, true).await;
        assert_eq!(recovered, 0, "an environment-bound (delegated) orphan must never be cancelled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running, "delegated run must stay Running");

        // Mirror on the re-dispatch leg: a delegated run must never be selected
        // for a local re-dispatch (which would spawn a duplicate runner). On the
        // SQLite test path the journal gate already returns empty; the delegated
        // guard adds a second, gate-independent exclusion.
        let candidates = resumable_orphans_for_redispatch(hub.clone(), &project_root, &HashSet::new(), true).await;
        assert!(!candidates.iter().any(|w| w.id == workflow_id), "delegated run must never be a re-dispatch candidate");
    }

    // Codex P1: delegation can come from `environment_routing` (a non-local
    // `default` or a matching kind rule), NOT only a workflow-level `environment:`.
    // A run routed remotely by the routing default must ALSO be skipped by the
    // sweep — mirroring the broker, which brokers it too.
    #[tokio::test]
    async fn routing_default_delegated_run_is_not_cancelled() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        // No workflow-level environment; a config-level routing default sends
        // everything to a remote node.
        let _env_guard = rebind_config_base(&temp, |base| {
            base.environment_routing = Some(orchestrator_config::workflow_config::EnvironmentRouting {
                default: Some("railway".to_string()),
                rules: Vec::new(),
            });
        });

        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false, true).await;
        assert_eq!(recovered, 0, "a routing-default delegated run must not be cancelled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running);
    }

    // Codex P2: a workflow bound to a HOST-ONLY environment id (`local` /
    // `worktree`) is NOT delegated — the broker runs it locally — so an orphaned
    // local runner must STILL be cancelled, never shielded indefinitely.
    #[tokio::test]
    async fn host_local_environment_orphan_is_still_cancelled() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        let workflow_ref = hub
            .workflows()
            .get(&workflow_id)
            .await
            .expect("workflow should load")
            .workflow_ref
            .expect("fixture run carries a workflow_ref");
        let _env_guard = rebind_config_base(&temp, |base| {
            for workflow in &mut base.workflows {
                if workflow.id == workflow_ref {
                    workflow.environment = Some("local".to_string());
                }
            }
        });

        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false, true).await;
        assert_eq!(recovered, 1, "a host-only (local) environment orphan must still be cancelled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Cancelled);
    }

    // Codex P1 (outage fail-safe): when the config_source plugin is present but
    // FAILING, delegation cannot be determined. A delegated run intentionally
    // has no local liveness, so the sweep must LEAVE IT ALONE this tick rather
    // than reap it — reconciliation is suppressed, not defaulted to "local".
    #[tokio::test]
    async fn orphan_sweep_suppressed_when_config_source_unavailable() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        // A present-but-failing config_source (checked before the base seam) makes
        // `try_load_workflow_config` return `SourceUnavailable`.
        let _failure = orchestrator_config::workflow_config::config_source_client::test_seam::install_failure(
            temp.path(),
            "simulated config_source outage",
        );

        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false, true).await;
        assert_eq!(recovered, 0, "a config-source outage must suppress cancellation, not reap the run");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Running, "run must be left untouched during the outage");

        // The re-dispatch leg suppresses too. (On the SQLite test path the
        // journal gate already returns empty; the outage guard is the stronger
        // guarantee — a delegated run is never re-dispatched during an outage.)
        let candidates = resumable_orphans_for_redispatch(hub.clone(), &project_root, &HashSet::new(), true).await;
        assert!(
            !candidates.iter().any(|w| w.id == workflow_id),
            "config-source outage must suppress re-dispatch candidacy"
        );
    }

    // Call-site dependency: at STARTUP (`skip_live_delegated=false`) the broker's
    // `reap_prior_daemon_records` has already torn down the prior daemon's remote
    // nodes, so a delegated `Running` row surviving into boot is DEAD — the sweep
    // must NOT shield it (shielding would strand it Running forever). The SAME
    // fixture the steady-state test shields (`environment_bound_run_is_not_...`)
    // is CANCELLED here purely because `skip_live_delegated=false`.
    #[tokio::test]
    async fn startup_still_cancels_reaped_delegated_run() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        // Bind the run's workflow to a remote environment — identical setup to
        // the steady-state shield test; only the call-site flag differs.
        let workflow_ref = hub
            .workflows()
            .get(&workflow_id)
            .await
            .expect("workflow should load")
            .workflow_ref
            .expect("fixture run carries a workflow_ref");
        let _env_guard = rebind_config_base(&temp, |base| {
            let mut bound = false;
            for workflow in &mut base.workflows {
                if workflow.id == workflow_ref {
                    workflow.environment = Some("railway".to_string());
                    bound = true;
                }
            }
            assert!(bound, "the run's workflow '{workflow_ref}' must exist in the base to bind an environment");
        });

        // skip_live_delegated=false (the startup path): the reaped delegate is
        // dead, so the sweep handles it exactly as a plain orphan and cancels it.
        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false, false).await;
        assert_eq!(recovered, 1, "at startup a reaped delegated orphan must still be cancelled");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Cancelled, "startup must clean up the dead delegated row");
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

        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false, false).await;
        assert_eq!(recovered, 0, "paused workflows must be exempt from orphan recovery");
        let reloaded = hub.workflows().get(&workflow.id).await.expect("workflow should reload");
        assert_eq!(reloaded.status, WorkflowStatus::Paused);
    }

    // -----------------------------------------------------------------
    // TASK-933 / TASK-793 / TASK-811: liveness-refined delegated reconciliation
    // -----------------------------------------------------------------

    use animus_runtime_shared::phase_session::{
        read_checkpoint, update_session_environment, write_session_pending, EnvironmentBinding, SessionCheckpointStatus,
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
    /// current phase; returns the scoped root + phase id.
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

    fn ok_probe_response() -> animus_environment_protocol::ExecResponse {
        animus_environment_protocol::ExecResponse {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
        }
    }

    // Hardening: a live node that blips once (or twice) then answers is Alive —
    // the retry absorbs a transient failure, never false-killing in-flight work.
    #[test]
    fn probe_retry_treats_a_transient_blip_then_success_as_alive() {
        use std::cell::Cell;
        let calls = Cell::new(0usize);
        let liveness = super::probe_liveness_with_retry(3, std::time::Duration::ZERO, || {
            let n = calls.get();
            calls.set(n + 1);
            if n == 0 {
                Err(anyhow::Error::from(orchestrator_plugin_host::HostError::Timeout(std::time::Duration::from_secs(
                    10,
                ))))
            } else {
                Ok(ok_probe_response())
            }
        });
        assert_eq!(liveness, super::DelegateLiveness::Alive, "a live node that blips once must not be terminalized");
        assert_eq!(calls.get(), 2, "probe retried once, then succeeded");
    }

    // A genuinely-gone node fails EVERY attempt with a definitive death signal
    // (closed transport) => Dead.
    #[test]
    fn probe_retry_all_attempts_fail_definitively_is_dead() {
        use std::cell::Cell;
        let calls = Cell::new(0usize);
        let liveness = super::probe_liveness_with_retry(3, std::time::Duration::ZERO, || {
            calls.set(calls.get() + 1);
            Err(anyhow::Error::from(orchestrator_plugin_host::HostError::ConnectionLost))
        });
        assert_eq!(liveness, super::DelegateLiveness::Dead, "a node whose transport is closed on all attempts is dead");
        assert_eq!(calls.get(), 3, "all three attempts were exhausted before declaring Dead");
    }

    // A node that TIMES OUT on every attempt (busy/slow but maybe alive) leans
    // to Unknown => preserve, never Dead.
    #[test]
    fn probe_retry_all_attempts_time_out_is_unknown_fail_safe() {
        let liveness = super::probe_liveness_with_retry(3, std::time::Duration::ZERO, || {
            Err(anyhow::Error::from(orchestrator_plugin_host::HostError::Timeout(std::time::Duration::from_secs(10))))
        });
        assert_eq!(
            liveness,
            super::DelegateLiveness::Unknown,
            "a consistently-timing-out node fails safe to Unknown (preserve), never Dead"
        );
    }

    // current_delegate_binding yields an UN-TORN-DOWN binding only.
    #[tokio::test]
    async fn current_delegate_binding_reads_untorn_binding_only() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let workflow = hub.workflows().get(&workflow_id).await.expect("workflow loads");

        let scoped_root = protocol::scoped_state_root(std::path::Path::new(&project_root)).expect("scope");
        assert!(super::current_delegate_binding(&scoped_root, &workflow).is_none(), "no checkpoint => no binding");

        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-live")).await;
        let (got_phase, got_binding) =
            super::current_delegate_binding(&scoped_root, &workflow).expect("live binding present");
        assert_eq!(got_phase, phase_id);
        assert_eq!(got_binding.handle.id, "node-live");

        animus_runtime_shared::phase_session::mark_environment_torn_down(&scoped_root, &workflow_id, &phase_id)
            .expect("mark torn down");
        assert!(
            super::current_delegate_binding(&scoped_root, &workflow).is_none(),
            "an already-reaped node is not returned again"
        );
    }

    // TASK-793 refinement: an intent-delegated run (config routes it remote)
    // whose node liveness cannot be verified (no plugin installed => probe
    // Unknown) is STILL skipped/preserved — the liveness gate refines rc.24's
    // skip without ever false-killing a delegate it cannot confirm is dead.
    #[tokio::test]
    async fn intent_delegated_run_with_unverifiable_node_is_still_preserved() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;

        // Make the run intent-delegated (rc.24 gate) by binding its workflow to a
        // remote environment in config.
        let workflow_ref = hub
            .workflows()
            .get(&workflow_id)
            .await
            .expect("workflow loads")
            .workflow_ref
            .expect("fixture run carries a workflow_ref");
        let _env_guard = rebind_config_base(&temp, |base| {
            for workflow in &mut base.workflows {
                if workflow.id == workflow_ref {
                    workflow.environment = Some("railway".to_string());
                }
            }
        });
        // Persist a binding, but no environment plugin is installed => probe
        // resolve fails => Unknown => NOT dead => stays SkippedDelegated.
        bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-unverifiable")).await;

        let recovered =
            recover_orphaned_running_workflows(hub.clone(), &project_root, &HashSet::new(), false, true).await;
        assert_eq!(recovered, 0, "an unverifiable delegate must be preserved, never terminalized");
        let reloaded = hub.workflows().get(&workflow_id).await.expect("workflow reloads");
        assert_eq!(reloaded.status, WorkflowStatus::Running, "unverifiable delegate stays Running");
    }

    // TASK-811: after the delegated node has been successfully reaped,
    // terminalize_dead_delegation drives the checkpoint Failed (so it never
    // re-surfaces for auto-resume) and cancels the workflow. Marking the
    // fixture binding torn down models that successful cleanup without making
    // this unit test depend on an installed environment plugin.
    #[tokio::test]
    async fn terminalize_dead_delegation_fails_checkpoint_and_cancels_workflow() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-dead")).await;
        animus_runtime_shared::phase_session::mark_environment_torn_down(&scoped_root, &workflow_id, &phase_id)
            .expect("model successful delegated-node teardown");
        let workflow = hub.workflows().get(&workflow_id).await.expect("workflow loads");

        let cancelled = super::terminalize_dead_delegation(hub.clone(), &project_root, &scoped_root, &workflow).await;
        assert!(cancelled, "terminalize cancels the workflow");

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

    // teardown_delegated_node is a safe no-op with no scope, no checkpoint, or an
    // already-torn-down binding (idempotent; never errors, never double-reaps).
    #[tokio::test]
    async fn teardown_delegated_node_is_inert_without_a_live_binding() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let workflow = hub.workflows().get(&workflow_id).await.expect("workflow loads");

        super::teardown_delegated_node(&project_root, None, &workflow);
        let scoped_root = protocol::scoped_state_root(std::path::Path::new(&project_root)).expect("scope");
        super::teardown_delegated_node(&project_root, Some(&scoped_root), &workflow);

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

    #[tokio::test]
    async fn external_cancel_releases_retained_node_once_then_marks_binding() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-held")).await;
        let workflow = hub.workflows().get(&workflow_id).await.expect("workflow loads");
        let teardown_calls = AtomicUsize::new(0);

        super::teardown_retained_environment_with(&scoped_root, &workflow, |binding| {
            assert_eq!(binding.handle.id, "node-held");
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("first cancellation cleanup");
        super::teardown_retained_environment_with(&scoped_root, &workflow, |_| {
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("idempotent repeated cleanup");

        assert_eq!(teardown_calls.load(Ordering::SeqCst), 1, "retained node is torn down exactly once");
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert!(checkpoint.environment.expect("binding").torn_down, "successful cleanup releases the hold");
    }

    #[tokio::test]
    async fn external_cancel_finds_retained_binding_from_earlier_phase() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let workflow = hub.workflows().get(&workflow_id).await.expect("workflow loads");
        let (scoped_root, retained_phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-from-earlier-phase")).await;

        // Model cancellation after the workflow has advanced to a later
        // manual/local phase whose checkpoint has no environment binding.
        let (later_index, later_phase) = workflow
            .phases
            .iter()
            .enumerate()
            .find(|(_, phase)| phase.phase_id != retained_phase_id)
            .expect("fixture has a later phase");
        let mut later_workflow = workflow.clone();
        later_workflow.current_phase_index = later_index;
        later_workflow.current_phase = Some(later_phase.phase_id.clone());

        let teardown_calls = AtomicUsize::new(0);
        super::teardown_retained_environment_with(&scoped_root, &later_workflow, |binding| {
            assert_eq!(binding.handle.id, "node-from-earlier-phase");
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("earlier retained binding is released");
        super::teardown_retained_environment_with(&scoped_root, &later_workflow, |_| {
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("repeated cleanup is inert");

        assert_eq!(teardown_calls.load(Ordering::SeqCst), 1, "earlier retained handle tears down exactly once");
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &retained_phase_id).expect("read").expect("checkpoint present");
        assert!(checkpoint.environment.expect("binding").torn_down, "earlier binding is marked torn down");
    }

    #[tokio::test]
    async fn failed_external_cancel_cleanup_keeps_binding_retryable() {
        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-held")).await;
        let workflow = hub.workflows().get(&workflow_id).await.expect("workflow loads");

        let err =
            super::teardown_retained_environment_with(&scoped_root, &workflow, |_| anyhow::bail!("relay unavailable"))
                .expect_err("failed cleanup must abort cancellation");
        assert!(format!("{err:#}").contains("relay unavailable"));
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert!(
            !checkpoint.environment.expect("binding").torn_down,
            "failed teardown must leave the durable hold available for retry"
        );
    }

    // -----------------------------------------------------------------
    // TASK-1466: terminal-run environment teardown sweep
    // -----------------------------------------------------------------

    /// TASK-1466 fixture: the backdated fixture run moved to a terminal
    /// status, with `completed_at` backdated past the teardown grace floor.
    async fn terminal_workflow_fixture(
        temp: &TempDir,
        status: WorkflowStatus,
    ) -> (Arc<dyn ServiceHub>, String, String, Box<dyn std::any::Any>) {
        let (hub, project_root, workflow_id, guards) = backdated_running_workflow_fixture(temp).await;
        let manager = WorkflowStateManager::new(temp.path());
        let mut stored = manager.load(&workflow_id).expect("workflow should load");
        stored.status = status;
        stored.completed_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));
        manager.save(&stored).expect("terminal workflow should save");
        (hub, project_root, workflow_id, guards)
    }

    /// Write a durable broker lease record for `workflow_id` exactly as the
    /// broker persists it, without constructing a broker.
    fn write_broker_lease_record(project_root: &str, workflow_id: &str, node_id: &str) -> std::path::PathBuf {
        let scoped_root = protocol::scoped_state_root(std::path::Path::new(project_root)).expect("scope");
        let records_dir = scoped_root.join("workflow-environments");
        std::fs::create_dir_all(&records_dir).expect("records dir");
        let path = records_dir.join(format!("{}.json", protocol::sanitize_identifier(workflow_id, "run")));
        let old = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let record = serde_json::json!({
            "run_id": workflow_id,
            "daemon_instance_id": "dead-daemon",
            "environment_id": "animus-environment-railway",
            "project_root": project_root,
            "state": "tearing-down",
            "handle": { "id": node_id, "workspace_root": "/work", "metadata": { "railway_service_id": "svc-1" } },
            "created_at": old,
            "updated_at": old,
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&record).expect("record json")).expect("write record");
        path
    }

    // Core: a Completed run whose checkpoint binding was never torn down
    // (owner died between terminal projection and teardown) gets torn down
    // exactly once; a second sweep is inert.
    #[tokio::test]
    async fn terminal_completed_run_retained_binding_is_torn_down_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) =
            terminal_workflow_fixture(&temp, WorkflowStatus::Completed).await;
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-terminal")).await;
        let teardown_calls = AtomicUsize::new(0);

        let torn =
            super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |root, env, handle| {
                assert_eq!(root, project_root.as_str());
                assert_eq!(env, "animus-environment-railway");
                assert_eq!(handle.id, "node-terminal");
                teardown_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await;
        assert_eq!(torn, 1, "the terminal run's retained node must be torn down");
        assert_eq!(teardown_calls.load(Ordering::SeqCst), 1);
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert!(checkpoint.environment.expect("binding").torn_down, "successful teardown marks the binding");

        let torn_again = super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |_, _, _| {
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(torn_again, 0, "a torn-down lease is not re-torn-down");
        assert_eq!(teardown_calls.load(Ordering::SeqCst), 1, "teardown ran exactly once across sweeps");
    }

    // A Failed run (phase failure killed the runner before teardown) gets the
    // same treatment as Completed.
    #[tokio::test]
    async fn terminal_failed_run_retained_binding_is_torn_down() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = terminal_workflow_fixture(&temp, WorkflowStatus::Failed).await;
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-failed")).await;
        let teardown_calls = AtomicUsize::new(0);

        let torn = super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |_, _, handle| {
            assert_eq!(handle.id, "node-failed");
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(torn, 1, "a failed run's retained node must be torn down");
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert!(checkpoint.environment.expect("binding").torn_down);
    }

    // A NON-terminal run's retained lease is never touched: a live owner may
    // still tear it down itself, and the sweep must not race it.
    #[tokio::test]
    async fn non_terminal_run_retained_binding_is_untouched() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) = backdated_running_workflow_fixture(&temp).await;
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-live")).await;
        let teardown_calls = AtomicUsize::new(0);

        let torn = super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |_, _, _| {
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(torn, 0, "a Running run's lease is left to its live owner");
        assert_eq!(teardown_calls.load(Ordering::SeqCst), 0, "teardown must not run for a Running row");

        hub.workflows().pause(&workflow_id).await.expect("workflow should pause");
        let torn = super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |_, _, _| {
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(torn, 0, "a Paused run's lease is left alone too");
        assert_eq!(teardown_calls.load(Ordering::SeqCst), 0, "teardown must not run for a Paused row");
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert!(!checkpoint.environment.expect("binding").torn_down, "non-terminal binding stays retained");
    }

    // Retry semantics: a failed teardown keeps the durable obligation and the
    // NEXT sweep drives teardown again (this is the steady-state retry the
    // broker alone only performs at startup).
    #[tokio::test]
    async fn failed_teardown_is_retained_and_retried_next_sweep() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) =
            terminal_workflow_fixture(&temp, WorkflowStatus::Completed).await;
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-flaky")).await;
        let teardown_calls = AtomicUsize::new(0);

        let torn = super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |_, _, _| {
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("relay unavailable")
        })
        .await;
        assert_eq!(torn, 0, "a failed teardown tears nothing down");
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert!(!checkpoint.environment.expect("binding").torn_down, "failure keeps the binding retryable");

        let torn = super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |_, _, _| {
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(torn, 1, "the next sweep retries the retained lease");
        assert_eq!(teardown_calls.load(Ordering::SeqCst), 2, "teardown was attempted once per sweep");
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert!(checkpoint.environment.expect("binding").torn_down, "the retry completes the cleanup");
    }

    // Broker lease records are swept too (a TearingDown record retained after
    // a failed broker teardown, or a Ready record whose teardown never ran):
    // torn down by handle and the durable record removed on success.
    #[tokio::test]
    async fn terminal_run_broker_lease_record_is_torn_down_and_removed() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) =
            terminal_workflow_fixture(&temp, WorkflowStatus::Completed).await;
        let record_path = write_broker_lease_record(&project_root, &workflow_id, "node-leased");
        let teardown_calls = AtomicUsize::new(0);

        let torn =
            super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |root, env, handle| {
                assert_eq!(root, project_root.as_str());
                assert_eq!(env, "animus-environment-railway");
                assert_eq!(handle.id, "node-leased");
                teardown_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await;
        assert_eq!(torn, 1, "the terminal run's broker lease must be torn down");
        assert!(!record_path.exists(), "the durable lease record is removed after a successful teardown");

        let torn_again = super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |_, _, _| {
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(torn_again, 0, "no record remains for the next sweep");
        assert_eq!(teardown_calls.load(Ordering::SeqCst), 1, "teardown ran exactly once");
    }

    // When the SAME handle is held by both a broker record and a phase
    // checkpoint, the sweep tears it down once and cleans up both records.
    #[tokio::test]
    async fn broker_record_and_checkpoint_for_one_handle_teardown_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) =
            terminal_workflow_fixture(&temp, WorkflowStatus::Completed).await;
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-shared")).await;
        let record_path = write_broker_lease_record(&project_root, &workflow_id, "node-shared");
        let teardown_calls = AtomicUsize::new(0);

        let torn = super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |_, _, handle| {
            assert_eq!(handle.id, "node-shared");
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(torn, 1, "one shared node is torn down once");
        assert_eq!(teardown_calls.load(Ordering::SeqCst), 1, "no duplicate teardown for the same handle");
        assert!(!record_path.exists(), "broker record removed");
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert!(checkpoint.environment.expect("binding").torn_down, "checkpoint binding marked");
    }

    // The grace floor: a run that JUST landed terminal is skipped so the
    // normal owner-driven teardown (which fires as the journal lands
    // terminal) gets first chance; the sweep is the backstop, not the racer.
    #[tokio::test]
    async fn terminal_run_within_teardown_grace_is_skipped() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _lock = test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().expect("temp dir");
        let (hub, project_root, workflow_id, _guards) =
            terminal_workflow_fixture(&temp, WorkflowStatus::Completed).await;
        let (scoped_root, phase_id) =
            bind_delegate(&hub, &project_root, &workflow_id, sample_binding("node-fresh")).await;
        // Re-terminalize with a FRESH completed_at (inside the grace floor).
        let manager = WorkflowStateManager::new(temp.path());
        let mut stored = manager.load(&workflow_id).expect("workflow should load");
        stored.completed_at = Some(chrono::Utc::now());
        manager.save(&stored).expect("workflow should save");
        let teardown_calls = AtomicUsize::new(0);

        let torn = super::reconcile_terminal_environment_leases_with(hub.clone(), &project_root, |_, _, _| {
            teardown_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(torn, 0, "a just-terminal run is left to the normal teardown path this sweep");
        assert_eq!(teardown_calls.load(Ordering::SeqCst), 0);
        let checkpoint =
            read_checkpoint(&scoped_root, &workflow_id, &phase_id).expect("read").expect("checkpoint present");
        assert!(!checkpoint.environment.expect("binding").torn_down, "the lease is retained for a later sweep");
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
