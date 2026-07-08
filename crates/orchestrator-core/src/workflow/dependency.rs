//! Inter-workflow dependencies and fan-in (join) coordination.
//!
//! Today a subject's `dependencies` field is metadata-only. This module adds an
//! ENFORCED barrier between workflow RUNS: a run can declare that it depends on N
//! upstream runs completing, and a JOIN run fires exactly once all of its declared
//! upstreams reach a terminal state.
//!
//! ## Fan-out then fan-in
//!
//! Enqueue N parallel runs (the fan-out), plus one JOIN run that declares the N as
//! its upstreams (the fan-in). The join is HELD (kept in a non-running state) by
//! the dispatcher until every upstream reaches a terminal status; then it is
//! released and dispatched exactly once.
//!
//! ## Carrier: run `vars` (no protocol change)
//!
//! The dependency declaration rides on the run's `vars` map, which round-trips from
//! [`WorkflowRunInput::vars`](crate::types::WorkflowRunInput) into
//! [`OrchestratorWorkflow::vars`](crate::types::OrchestratorWorkflow) at bootstrap
//! and is persisted with every journal save. Two reserved keys:
//!
//! - [`DEPENDS_ON_VAR`] — the upstream run ids, either a JSON array
//!   (`["wf-a","wf-b"]`) or a comma/whitespace-separated list (`wf-a, wf-b`).
//! - [`JOIN_POLICY_VAR`] — the failed-upstream policy: `block` (default),
//!   `proceed`, or `cancel`.
//!
//! Because the carrier is `vars`, no out-of-tree wire type changes; the mechanism
//! is entirely in-kernel.
//!
//! ## Evaluation is pure; the journal is the source of truth
//!
//! [`resolve_ready_joins`] takes a snapshot of runs (their id/status/vars, which the
//! caller reads from the workflow journal) and returns the joins that are ready to
//! FIRE or should be CANCELLED right now. It is a pure function of the snapshot, so
//! it is trivially testable and side-effect free. The daemon calls it on every
//! `workflow_completed` / `workflow_failed` event (and on its reconcile tick),
//! then releases/dispatches or cancels the returned joins.
//!
//! ## Fire exactly once
//!
//! A join is only considered while it is still AWAITING RELEASE (its own status is
//! `Pending` or `Paused`). Once the dispatcher fires it (status → `Running`) or it
//! finishes, it is no longer eligible, so a subsequent evaluation can never
//! re-fire it. The barrier is therefore idempotent under repeated evaluation.

use std::collections::{HashMap, HashSet};

use crate::types::WorkflowStatus;

/// Reserved `vars` key carrying a join run's upstream run ids. Value is either a
/// JSON string array (`["wf-a","wf-b"]`) or a comma/whitespace-separated list.
pub const DEPENDS_ON_VAR: &str = "ANIMUS_DEPENDS_ON";

/// Reserved `vars` key carrying the failed-upstream policy for a join run. One of
/// `block` (default), `proceed`, `cancel`. Parsed case-insensitively.
pub const JOIN_POLICY_VAR: &str = "ANIMUS_JOIN_POLICY";

/// What a join run does when one of its declared upstreams reaches a FAILED
/// terminal state (`Failed` / `Escalated` / `Cancelled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpstreamFailurePolicy {
    /// Never fire the join if any upstream failed; it stays held. The safe default:
    /// a join gated on a barrier should not run when the barrier is not fully
    /// green. Surfaced as [`JoinDecision::Block`] for observability.
    #[default]
    Block,
    /// Fire the join once every upstream is terminal, REGARDLESS of whether some
    /// failed. Use when the join is a cleanup/report step that must run either way.
    Proceed,
    /// Cancel the join as soon as any upstream fails. Use when a failed upstream
    /// makes the join meaningless.
    Cancel,
}

impl UpstreamFailurePolicy {
    /// Parse a policy from its `vars` value, case-insensitively. Unknown or empty
    /// values fall back to the [`Block`](UpstreamFailurePolicy::Block) default.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "proceed" | "continue" => Self::Proceed,
            "cancel" | "abort" => Self::Cancel,
            _ => Self::Block,
        }
    }

    /// The canonical `vars` value for this policy.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Proceed => "proceed",
            Self::Cancel => "cancel",
        }
    }
}

/// A run's declared dependency on N upstream runs, parsed from its `vars`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDependencySpec {
    /// Upstream run ids this run waits on. Deduplicated, order-preserving.
    pub upstreams: Vec<String>,
    /// What to do when an upstream fails.
    pub policy: UpstreamFailurePolicy,
}

impl RunDependencySpec {
    /// Parse a dependency spec from a run's `vars`. Returns `None` when the run
    /// declares no upstreams (i.e. it is not a join run). An empty/whitespace
    /// upstream list also yields `None` — a join with zero upstreams is not a
    /// barrier.
    pub fn from_vars(vars: &HashMap<String, String>) -> Option<Self> {
        let raw = vars.get(DEPENDS_ON_VAR)?;
        let upstreams = parse_upstreams(raw);
        if upstreams.is_empty() {
            return None;
        }
        let policy = vars.get(JOIN_POLICY_VAR).map(|v| UpstreamFailurePolicy::parse(v)).unwrap_or_default();
        Some(Self { upstreams, policy })
    }

    /// Write this spec into a `vars` map (e.g. a `WorkflowRunInput`'s) so the
    /// declaration rides along on enqueue. The upstreams are stored as a JSON
    /// array so ids containing commas/whitespace round-trip losslessly.
    pub fn write_into_vars(&self, vars: &mut HashMap<String, String>) {
        let encoded = serde_json::to_string(&self.upstreams).unwrap_or_else(|_| self.upstreams.join(","));
        vars.insert(DEPENDS_ON_VAR.to_string(), encoded);
        vars.insert(JOIN_POLICY_VAR.to_string(), self.policy.as_str().to_string());
    }
}

/// Parse the [`DEPENDS_ON_VAR`] value into upstream ids. Accepts a JSON string
/// array first (the canonical form written by [`RunDependencySpec::write_into_vars`]);
/// otherwise splits on commas and whitespace. Trims, drops empties, dedups while
/// preserving first-seen order.
fn parse_upstreams(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let candidates: Vec<String> = serde_json::from_str::<Vec<String>>(trimmed)
        .unwrap_or_else(|_| trimmed.split([',', '\n', '\t', ' ']).map(|s| s.to_string()).collect());
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for candidate in candidates {
        let id = candidate.trim().to_string();
        if id.is_empty() {
            continue;
        }
        if seen.insert(id.clone()) {
            out.push(id);
        }
    }
    out
}

/// The terminal/pending classification of a single upstream run for barrier
/// purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamOutcome {
    /// Not yet terminal (`Pending` / `Running` / `Paused`), or not found in the
    /// snapshot (a run enqueued slightly later, or a stale/pruned id). Both keep
    /// the join WAITING — the runtime path is deliberately race-safe; bogus ids are
    /// rejected up front by [`validate_declaration`] instead of deadlocking here.
    Pending,
    /// Reached `Completed` — a clean success.
    Succeeded,
    /// Reached a FAILED terminal state (`Failed` / `Escalated` / `Cancelled`).
    Failed,
}

/// Classify a workflow status for barrier evaluation.
pub fn classify_status(status: WorkflowStatus) -> UpstreamOutcome {
    match status {
        WorkflowStatus::Completed => UpstreamOutcome::Succeeded,
        WorkflowStatus::Failed | WorkflowStatus::Escalated | WorkflowStatus::Cancelled => UpstreamOutcome::Failed,
        WorkflowStatus::Pending | WorkflowStatus::Running | WorkflowStatus::Paused => UpstreamOutcome::Pending,
    }
}

/// Whether a join run is still AWAITING RELEASE — eligible for a fire/cancel
/// decision.
///
/// A dependency-declaring run is HELD by the dispatcher in `Pending` (created but
/// with no phase started) until its barrier clears; the dispatcher then releases it
/// via `WorkflowLifecycleExecutor::release_held_run`. Once released it is `Running`,
/// then terminal, so it is never eligible again — which is what makes firing
/// idempotent.
///
/// `Paused` is deliberately EXCLUDED: a join that has already fired and later
/// suspends for a human interaction / approval is `Paused` mid-execution, not held.
/// Treating that as awaiting-release would re-fire it against still-terminal
/// upstreams, breaking the exact-once guarantee. Only a never-started `Pending` run
/// is a held join.
pub fn is_awaiting_release(status: WorkflowStatus) -> bool {
    matches!(status, WorkflowStatus::Pending)
}

/// The decision for a single join run against a snapshot of upstream outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinDecision {
    /// At least one upstream is not yet terminal (or is missing). Keep the join
    /// held; do nothing.
    Wait {
        /// Upstream ids still pending/missing.
        pending: Vec<String>,
    },
    /// The barrier is satisfied per policy — release and dispatch the join now.
    Fire,
    /// Policy is `cancel` and at least one upstream failed — cancel the join.
    Cancel {
        /// Upstream ids that failed.
        failed: Vec<String>,
    },
    /// Policy is `block` and at least one upstream failed — the join stays held
    /// permanently. Surfaced (rather than silently held) so the dispatcher can log
    /// / alert on a barrier that can never clear.
    Block {
        /// Upstream ids that failed.
        failed: Vec<String>,
    },
}

impl JoinDecision {
    /// Whether this decision requires the dispatcher to act (fire or cancel). A
    /// `Wait` or `Block` decision is informational — the join simply stays held.
    pub fn is_actionable(&self) -> bool {
        matches!(self, JoinDecision::Fire | JoinDecision::Cancel { .. })
    }
}

/// Evaluate a single join spec against a resolver mapping each upstream id to its
/// current outcome. Pure and deterministic.
pub fn evaluate_join(spec: &RunDependencySpec, mut outcome_of: impl FnMut(&str) -> UpstreamOutcome) -> JoinDecision {
    let mut pending = Vec::new();
    let mut failed = Vec::new();
    for upstream in &spec.upstreams {
        match outcome_of(upstream) {
            UpstreamOutcome::Pending => pending.push(upstream.clone()),
            UpstreamOutcome::Failed => failed.push(upstream.clone()),
            UpstreamOutcome::Succeeded => {}
        }
    }

    match spec.policy {
        // Cancel wins the instant an upstream fails — no need to wait for the rest.
        UpstreamFailurePolicy::Cancel if !failed.is_empty() => JoinDecision::Cancel { failed },
        // Block short-circuits on failure too, but keeps the join held rather than
        // cancelling it.
        UpstreamFailurePolicy::Block if !failed.is_empty() => JoinDecision::Block { failed },
        // Otherwise the gating rule is uniform: wait until nothing is pending.
        _ if !pending.is_empty() => JoinDecision::Wait { pending },
        // All upstreams terminal, no policy-blocking failure => fire. (Under
        // `Proceed`, failed upstreams are tolerated and land here.)
        _ => JoinDecision::Fire,
    }
}

/// A minimal, storage-agnostic view of a workflow run for barrier evaluation. The
/// caller builds these from journal records ([`OrchestratorWorkflow`]).
///
/// [`OrchestratorWorkflow`]: crate::types::OrchestratorWorkflow
#[derive(Debug, Clone)]
pub struct RunSnapshot {
    pub id: String,
    pub status: WorkflowStatus,
    pub vars: HashMap<String, String>,
}

impl RunSnapshot {
    /// Build a snapshot from a full workflow record.
    pub fn from_workflow(workflow: &crate::types::OrchestratorWorkflow) -> Self {
        Self { id: workflow.id.clone(), status: workflow.status, vars: workflow.vars.clone() }
    }

    /// This run's dependency spec, if it declares one.
    pub fn dependency_spec(&self) -> Option<RunDependencySpec> {
        RunDependencySpec::from_vars(&self.vars)
    }
}

/// The outcome of evaluating one held join against the run snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinResolution {
    /// The join run's id.
    pub join_run_id: String,
    /// The decision for it.
    pub decision: JoinDecision,
    /// The parsed spec (upstreams + policy) for logging/diagnostics.
    pub spec: RunDependencySpec,
}

/// Evaluate every AWAITING-RELEASE join in `runs` and return the ACTIONABLE ones —
/// those the dispatcher must fire or cancel right now. Joins that are still
/// [`JoinDecision::Wait`]ing, or permanently [`JoinDecision::Block`]ed, are
/// omitted (they simply stay held); use [`resolve_all_joins`] to observe those too.
///
/// This is the entry point the daemon calls on each `workflow_completed` /
/// `workflow_failed` event. Because only `Pending`/`Paused` joins are considered,
/// a join is returned at most once across its lifetime (firing flips it to
/// `Running`; cancelling flips it to `Cancelled`), so the fan-in is idempotent.
pub fn resolve_ready_joins(runs: &[RunSnapshot]) -> Vec<JoinResolution> {
    resolve_all_joins(runs).into_iter().filter(|resolution| resolution.decision.is_actionable()).collect()
}

/// Like [`resolve_ready_joins`] but returns a resolution for EVERY awaiting join,
/// including `Wait` and `Block`. Useful for status/observability surfaces that want
/// to show why a join is still held.
pub fn resolve_all_joins(runs: &[RunSnapshot]) -> Vec<JoinResolution> {
    let status_by_id: HashMap<&str, WorkflowStatus> = runs.iter().map(|run| (run.id.as_str(), run.status)).collect();

    let mut resolutions = Vec::new();
    for run in runs {
        if !is_awaiting_release(run.status) {
            continue;
        }
        let Some(spec) = run.dependency_spec() else {
            continue;
        };
        let decision = evaluate_join(&spec, |upstream| {
            status_by_id.get(upstream).copied().map(classify_status).unwrap_or(UpstreamOutcome::Pending)
        });
        resolutions.push(JoinResolution { join_run_id: run.id.clone(), decision, spec });
    }
    resolutions
}

/// Whether a run should be HELD (kept out of dispatch) at enqueue time because it
/// declares upstreams that are not all satisfied yet. The dispatcher uses this to
/// decide whether to enqueue a run as held vs. dispatch it immediately.
///
/// Returns `false` for a run with no dependency spec (a normal run), or one whose
/// barrier is already clear (all upstreams already succeeded, or `proceed` with all
/// terminal).
pub fn should_hold_at_enqueue(spec: &RunDependencySpec, runs: &[RunSnapshot]) -> bool {
    let status_by_id: HashMap<&str, WorkflowStatus> = runs.iter().map(|run| (run.id.as_str(), run.status)).collect();
    let decision = evaluate_join(spec, |upstream| {
        status_by_id.get(upstream).copied().map(classify_status).unwrap_or(UpstreamOutcome::Pending)
    });
    !matches!(decision, JoinDecision::Fire)
}

/// A problem with a dependency declaration, found at declare/enqueue time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyError {
    /// A run declared itself as one of its own upstreams.
    SelfDependency { run_id: String },
    /// An upstream id is not a known run (typo / never enqueued). Rejecting this at
    /// declare time is what lets the runtime path treat a missing upstream as
    /// race-safe `Pending` without risking a permanent deadlock.
    UnknownUpstream { run_id: String, upstream: String },
    /// The declared dependencies form a cycle among join runs.
    Cycle { runs: Vec<String> },
}

/// Validate a dependency declaration for `run_id` against the set of known run ids.
/// Checks for self-dependency and unknown upstreams. Cycle detection across
/// multiple joins is [`detect_cycles`] (run over the full snapshot).
pub fn validate_declaration(
    run_id: &str,
    spec: &RunDependencySpec,
    known_run_ids: &HashSet<String>,
) -> Result<(), Vec<DependencyError>> {
    let mut errors = Vec::new();
    for upstream in &spec.upstreams {
        if upstream == run_id {
            errors.push(DependencyError::SelfDependency { run_id: run_id.to_string() });
        } else if !known_run_ids.contains(upstream) {
            errors.push(DependencyError::UnknownUpstream { run_id: run_id.to_string(), upstream: upstream.clone() });
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Detect dependency cycles among the join runs in `runs`. Returns one
/// [`DependencyError::Cycle`] per detected cycle (the run ids on the cycle, in
/// discovery order). An empty result means the dependency graph is a DAG.
///
/// Only edges between runs PRESENT in the snapshot are followed; an upstream that
/// is not itself a join in the snapshot is a leaf and cannot be part of a cycle.
pub fn detect_cycles(runs: &[RunSnapshot]) -> Vec<DependencyError> {
    // Adjacency: join run id -> its upstream ids (edges we can follow).
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for run in runs {
        if let Some(spec) = run.dependency_spec() {
            adjacency.insert(run.id.clone(), spec.upstreams);
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        InProgress,
        Done,
    }

    let mut marks: HashMap<String, Mark> = HashMap::new();
    let mut cycles = Vec::new();

    // Iterative DFS carrying the active path so a back-edge yields the exact cycle.
    for start in adjacency.keys() {
        if marks.contains_key(start) {
            continue;
        }
        // Stack of (node, next-child-index) plus a parallel path vector.
        let mut stack: Vec<(String, usize)> = vec![(start.clone(), 0)];
        let mut path: Vec<String> = vec![start.clone()];
        marks.insert(start.clone(), Mark::InProgress);

        while let Some((node, child_idx)) = stack.last().cloned() {
            let children = adjacency.get(&node).cloned().unwrap_or_default();
            if child_idx >= children.len() {
                marks.insert(node.clone(), Mark::Done);
                stack.pop();
                path.pop();
                continue;
            }
            // Advance the parent's cursor before descending.
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            let child = children[child_idx].clone();
            // Only follow edges to nodes that are themselves joins in the graph.
            if !adjacency.contains_key(&child) {
                continue;
            }
            match marks.get(&child) {
                Some(Mark::InProgress) => {
                    // Back-edge: extract the cycle from the active path.
                    let cycle_start = path.iter().position(|n| n == &child).unwrap_or(0);
                    let mut cycle: Vec<String> = path[cycle_start..].to_vec();
                    cycle.push(child.clone());
                    cycles.push(DependencyError::Cycle { runs: cycle });
                }
                Some(Mark::Done) => {}
                None => {
                    marks.insert(child.clone(), Mark::InProgress);
                    stack.push((child.clone(), 0));
                    path.push(child);
                }
            }
        }
    }

    cycles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn snapshot(id: &str, status: WorkflowStatus, dep_vars: &[(&str, &str)]) -> RunSnapshot {
        RunSnapshot { id: id.to_string(), status, vars: vars(dep_vars) }
    }

    fn join(id: &str, status: WorkflowStatus, upstreams: &[&str], policy: UpstreamFailurePolicy) -> RunSnapshot {
        let spec = RunDependencySpec { upstreams: upstreams.iter().map(|s| s.to_string()).collect(), policy };
        let mut v = HashMap::new();
        spec.write_into_vars(&mut v);
        RunSnapshot { id: id.to_string(), status, vars: v }
    }

    // --- parsing -----------------------------------------------------------

    #[test]
    fn parses_json_array_upstreams() {
        let v = vars(&[(DEPENDS_ON_VAR, r#"["wf-a","wf-b","wf-c"]"#)]);
        let spec = RunDependencySpec::from_vars(&v).expect("spec");
        assert_eq!(spec.upstreams, vec!["wf-a", "wf-b", "wf-c"]);
        assert_eq!(spec.policy, UpstreamFailurePolicy::Block);
    }

    #[test]
    fn parses_comma_and_whitespace_list_and_dedups() {
        let v = vars(&[(DEPENDS_ON_VAR, " wf-a, wf-b  wf-a\nwf-c ")]);
        let spec = RunDependencySpec::from_vars(&v).expect("spec");
        assert_eq!(spec.upstreams, vec!["wf-a", "wf-b", "wf-c"]);
    }

    #[test]
    fn no_depends_on_key_is_not_a_join() {
        assert!(RunDependencySpec::from_vars(&vars(&[])).is_none());
        // Empty value -> not a barrier.
        assert!(RunDependencySpec::from_vars(&vars(&[(DEPENDS_ON_VAR, "  ")])).is_none());
    }

    #[test]
    fn policy_parse_is_case_insensitive_with_block_default() {
        assert_eq!(UpstreamFailurePolicy::parse("PROCEED"), UpstreamFailurePolicy::Proceed);
        assert_eq!(UpstreamFailurePolicy::parse(" Cancel "), UpstreamFailurePolicy::Cancel);
        assert_eq!(UpstreamFailurePolicy::parse("nonsense"), UpstreamFailurePolicy::Block);
        assert_eq!(UpstreamFailurePolicy::parse(""), UpstreamFailurePolicy::Block);
    }

    #[test]
    fn spec_round_trips_through_vars() {
        let spec =
            RunDependencySpec { upstreams: vec!["wf-a".into(), "wf-b".into()], policy: UpstreamFailurePolicy::Proceed };
        let mut v = HashMap::new();
        spec.write_into_vars(&mut v);
        assert_eq!(RunDependencySpec::from_vars(&v), Some(spec));
    }

    // --- three-parallel-then-join (the acceptance scenario) ----------------

    #[test]
    fn join_fires_exactly_once_after_all_three_complete() {
        let all_done = vec![
            snapshot("wf-a", WorkflowStatus::Completed, &[]),
            snapshot("wf-b", WorkflowStatus::Completed, &[]),
            snapshot("wf-c", WorkflowStatus::Completed, &[]),
            join("wf-join", WorkflowStatus::Pending, &["wf-a", "wf-b", "wf-c"], UpstreamFailurePolicy::Block),
        ];
        let ready = resolve_ready_joins(&all_done);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].join_run_id, "wf-join");
        assert_eq!(ready[0].decision, JoinDecision::Fire);

        // Idempotency: once the dispatcher fires the join (status -> Running), a
        // re-evaluation must NOT return it again.
        let mut fired = all_done;
        fired[3].status = WorkflowStatus::Running;
        assert!(resolve_ready_joins(&fired).is_empty(), "a running join must not re-fire");

        // And after it completes it is likewise never returned.
        fired[3].status = WorkflowStatus::Completed;
        assert!(resolve_ready_joins(&fired).is_empty());
    }

    #[test]
    fn partial_completion_does_not_fire() {
        let partial = vec![
            snapshot("wf-a", WorkflowStatus::Completed, &[]),
            snapshot("wf-b", WorkflowStatus::Running, &[]),
            snapshot("wf-c", WorkflowStatus::Completed, &[]),
            join("wf-join", WorkflowStatus::Pending, &["wf-a", "wf-b", "wf-c"], UpstreamFailurePolicy::Block),
        ];
        assert!(resolve_ready_joins(&partial).is_empty(), "join must wait while an upstream runs");

        let all = resolve_all_joins(&partial);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].decision, JoinDecision::Wait { pending: vec!["wf-b".into()] });
    }

    #[test]
    fn missing_upstream_keeps_join_waiting_race_safe() {
        // wf-c has not been enqueued yet (not in the snapshot): the join must WAIT,
        // not fire or block, so a slightly-late upstream is still honored.
        let runs = vec![
            snapshot("wf-a", WorkflowStatus::Completed, &[]),
            snapshot("wf-b", WorkflowStatus::Completed, &[]),
            join("wf-join", WorkflowStatus::Pending, &["wf-a", "wf-b", "wf-c"], UpstreamFailurePolicy::Block),
        ];
        assert!(resolve_ready_joins(&runs).is_empty());
        let all = resolve_all_joins(&runs);
        assert_eq!(all[0].decision, JoinDecision::Wait { pending: vec!["wf-c".into()] });
    }

    // --- failed-upstream policies ------------------------------------------

    #[test]
    fn block_policy_holds_join_when_an_upstream_fails() {
        let runs = vec![
            snapshot("wf-a", WorkflowStatus::Completed, &[]),
            snapshot("wf-b", WorkflowStatus::Failed, &[]),
            snapshot("wf-c", WorkflowStatus::Completed, &[]),
            join("wf-join", WorkflowStatus::Pending, &["wf-a", "wf-b", "wf-c"], UpstreamFailurePolicy::Block),
        ];
        // Not actionable: it stays held.
        assert!(resolve_ready_joins(&runs).is_empty());
        let all = resolve_all_joins(&runs);
        assert_eq!(all[0].decision, JoinDecision::Block { failed: vec!["wf-b".into()] });
    }

    #[test]
    fn proceed_policy_fires_join_even_with_a_failed_upstream() {
        let runs = vec![
            snapshot("wf-a", WorkflowStatus::Completed, &[]),
            snapshot("wf-b", WorkflowStatus::Failed, &[]),
            snapshot("wf-c", WorkflowStatus::Completed, &[]),
            join("wf-join", WorkflowStatus::Pending, &["wf-a", "wf-b", "wf-c"], UpstreamFailurePolicy::Proceed),
        ];
        let ready = resolve_ready_joins(&runs);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].decision, JoinDecision::Fire);
    }

    #[test]
    fn proceed_policy_still_waits_for_a_running_upstream() {
        let runs = vec![
            snapshot("wf-a", WorkflowStatus::Failed, &[]),
            snapshot("wf-b", WorkflowStatus::Running, &[]),
            join("wf-join", WorkflowStatus::Pending, &["wf-a", "wf-b"], UpstreamFailurePolicy::Proceed),
        ];
        assert!(resolve_ready_joins(&runs).is_empty(), "proceed still waits until all terminal");
    }

    #[test]
    fn cancel_policy_cancels_join_on_first_failure_without_waiting() {
        // wf-b failed, wf-c still running: cancel wins immediately.
        let runs = vec![
            snapshot("wf-a", WorkflowStatus::Completed, &[]),
            snapshot("wf-b", WorkflowStatus::Failed, &[]),
            snapshot("wf-c", WorkflowStatus::Running, &[]),
            join("wf-join", WorkflowStatus::Pending, &["wf-a", "wf-b", "wf-c"], UpstreamFailurePolicy::Cancel),
        ];
        let ready = resolve_ready_joins(&runs);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].decision, JoinDecision::Cancel { failed: vec!["wf-b".into()] });
    }

    #[test]
    fn escalated_and_cancelled_upstreams_count_as_failed() {
        for failed_status in [WorkflowStatus::Escalated, WorkflowStatus::Cancelled] {
            let runs = vec![
                snapshot("wf-a", failed_status, &[]),
                join("wf-join", WorkflowStatus::Pending, &["wf-a"], UpstreamFailurePolicy::Cancel),
            ];
            let ready = resolve_ready_joins(&runs);
            assert_eq!(ready.len(), 1, "{failed_status:?} must count as a failed upstream");
            assert_eq!(ready[0].decision, JoinDecision::Cancel { failed: vec!["wf-a".into()] });
        }
    }

    // --- hold-at-enqueue ---------------------------------------------------

    #[test]
    fn should_hold_at_enqueue_reflects_barrier_state() {
        let spec =
            RunDependencySpec { upstreams: vec!["wf-a".into(), "wf-b".into()], policy: UpstreamFailurePolicy::Block };
        let one_pending =
            vec![snapshot("wf-a", WorkflowStatus::Completed, &[]), snapshot("wf-b", WorkflowStatus::Running, &[])];
        assert!(should_hold_at_enqueue(&spec, &one_pending), "hold while an upstream runs");

        let all_done =
            vec![snapshot("wf-a", WorkflowStatus::Completed, &[]), snapshot("wf-b", WorkflowStatus::Completed, &[])];
        assert!(!should_hold_at_enqueue(&spec, &all_done), "no hold once the barrier is clear");
    }

    // --- validation --------------------------------------------------------

    #[test]
    fn validate_flags_self_dependency_and_unknown_upstream() {
        let spec = RunDependencySpec {
            upstreams: vec!["wf-join".into(), "wf-a".into(), "wf-ghost".into()],
            policy: UpstreamFailurePolicy::Block,
        };
        let known: HashSet<String> = ["wf-join", "wf-a"].iter().map(|s| s.to_string()).collect();
        let errors = validate_declaration("wf-join", &spec, &known).expect_err("should error");
        assert!(errors.contains(&DependencyError::SelfDependency { run_id: "wf-join".into() }));
        assert!(errors
            .contains(&DependencyError::UnknownUpstream { run_id: "wf-join".into(), upstream: "wf-ghost".into() }));
    }

    #[test]
    fn validate_ok_for_known_upstreams() {
        let spec =
            RunDependencySpec { upstreams: vec!["wf-a".into(), "wf-b".into()], policy: UpstreamFailurePolicy::Block };
        let known: HashSet<String> = ["wf-a", "wf-b"].iter().map(|s| s.to_string()).collect();
        assert!(validate_declaration("wf-join", &spec, &known).is_ok());
    }

    // --- cycle detection ---------------------------------------------------

    #[test]
    fn detect_cycles_finds_a_two_node_cycle() {
        // wf-x depends on wf-y and vice versa.
        let runs = vec![
            join("wf-x", WorkflowStatus::Pending, &["wf-y"], UpstreamFailurePolicy::Block),
            join("wf-y", WorkflowStatus::Pending, &["wf-x"], UpstreamFailurePolicy::Block),
        ];
        let cycles = detect_cycles(&runs);
        assert_eq!(cycles.len(), 1, "exactly one cycle");
    }

    #[test]
    fn detect_cycles_none_for_a_dag() {
        // join -> {a, b}; a and b are leaves (not joins). No cycle.
        let runs = vec![
            snapshot("wf-a", WorkflowStatus::Completed, &[]),
            snapshot("wf-b", WorkflowStatus::Completed, &[]),
            join("wf-join", WorkflowStatus::Pending, &["wf-a", "wf-b"], UpstreamFailurePolicy::Block),
        ];
        assert!(detect_cycles(&runs).is_empty());
    }

    #[test]
    fn detect_cycles_finds_self_loop() {
        let runs = vec![join("wf-x", WorkflowStatus::Pending, &["wf-x"], UpstreamFailurePolicy::Block)];
        let cycles = detect_cycles(&runs);
        assert_eq!(cycles.len(), 1);
    }

    #[test]
    fn multi_join_chain_fans_in_independently() {
        // Two independent joins in one snapshot; only the satisfied one fires.
        let runs = vec![
            snapshot("a1", WorkflowStatus::Completed, &[]),
            snapshot("a2", WorkflowStatus::Completed, &[]),
            join("join-a", WorkflowStatus::Pending, &["a1", "a2"], UpstreamFailurePolicy::Block),
            snapshot("b1", WorkflowStatus::Completed, &[]),
            snapshot("b2", WorkflowStatus::Running, &[]),
            join("join-b", WorkflowStatus::Pending, &["b1", "b2"], UpstreamFailurePolicy::Block),
        ];
        let ready = resolve_ready_joins(&runs);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].join_run_id, "join-a");
    }
}
