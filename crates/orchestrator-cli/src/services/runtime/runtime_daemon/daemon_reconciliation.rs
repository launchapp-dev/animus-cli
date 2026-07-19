use super::*;
use crate::services::runtime::execution_fact_projection::project_terminal_workflow_result;
use crate::services::runtime::workflow_mutation_surface::cancel_orphaned_running_workflow;
use anyhow::Result;
use orchestrator_core::{
    active_workflow_runner_ids, dispatch_workflow_event, load_agent_runtime_config_or_default, services::ServiceHub,
    workflow_runner_liveness, OrchestratorWorkflow, RunnerLiveness, WorkflowConfig, WorkflowEvent,
    WorkflowMachineState, WorkflowStatus,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
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
}

impl ReconcileDecision {
    fn label(self) -> &'static str {
        match self {
            ReconcileDecision::MergeConflict => "merge-conflict",
            ReconcileDecision::WaitingManual => "waiting-manual",
            ReconcileDecision::ProtectedLiveRunner => "protected-live-runner",
            ReconcileDecision::ProtectedActiveSubject => "protected-active-subject",
            ReconcileDecision::SkippedDelegated => "skipped-delegated",
            ReconcileDecision::WithinGrace => "within-grace",
            ReconcileDecision::PreservedResumable => "preserved-resumable",
            ReconcileDecision::RedispatchCandidate => "redispatch-candidate",
            ReconcileDecision::SkippedLiveOrphan => "skipped-live-orphan",
            ReconcileDecision::SkippedBlockedResume => "skipped-blocked-resume",
            ReconcileDecision::NotResumable => "not-resumable",
            ReconcileDecision::CancelledOrphan => "cancelled-orphan",
        }
    }

    /// Actionable decisions that must be logged even when per-run trace is OFF:
    /// a cancel, a delegated skip, or a re-dispatch selection.
    fn always_emit(self) -> bool {
        matches!(
            self,
            ReconcileDecision::CancelledOrphan
                | ReconcileDecision::SkippedDelegated
                | ReconcileDecision::RedispatchCandidate
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

        let decision = if workflow.machine_state == WorkflowMachineState::MergeConflict {
            ReconcileDecision::MergeConflict
        } else if waiting_manual {
            ReconcileDecision::WaitingManual
        } else if in_runner_registry {
            ReconcileDecision::ProtectedLiveRunner
        } else if in_active_subjects {
            ReconcileDecision::ProtectedActiveSubject
        } else if is_delegated {
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
            ReconcileDecision::MergeConflict
            | ReconcileDecision::WaitingManual
            | ReconcileDecision::WithinGrace
            // These variants never arise on the cancel leg (redispatch-only), but
            // are matched exhaustively; they are no-ops here.
            | ReconcileDecision::RedispatchCandidate
            | ReconcileDecision::SkippedLiveOrphan
            | ReconcileDecision::SkippedBlockedResume
            | ReconcileDecision::NotResumable => {}
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

        let decision = if workflow.machine_state == WorkflowMachineState::MergeConflict {
            ReconcileDecision::MergeConflict
        } else if waiting_manual {
            ReconcileDecision::WaitingManual
        } else if in_runner_registry {
            ReconcileDecision::ProtectedLiveRunner
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
            // All other outcomes exclude the run from re-dispatch (no-op).
            ReconcileDecision::MergeConflict
            | ReconcileDecision::WaitingManual
            | ReconcileDecision::SkippedLiveOrphan
            | ReconcileDecision::SkippedBlockedResume
            | ReconcileDecision::WithinGrace
            | ReconcileDecision::NotResumable
            | ReconcileDecision::PreservedResumable
            | ReconcileDecision::CancelledOrphan => {}
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
