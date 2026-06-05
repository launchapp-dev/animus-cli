//! Basic phase-eval framework (v0.5.5).
//!
//! Each phase definition can declare an `evals:` block — a list of checks that
//! must pass before the phase advances. The runner here is the trust gate for
//! agent autonomy: it is invoked AFTER a phase produced an `advance` decision
//! and BEFORE that decision is committed to the workflow state machine.
//!
//! Scope of the basic version:
//! - Two check kinds: [`EvalKind::Command`] (run a shell command; pass when
//!   the exit code matches `expected_exit`) and [`EvalKind::LlmJudge`] (one-shot
//!   agent call; pass when the response begins with "PASS" case-insensitively).
//! - Sequential execution. Parallelism + fixture replay are deferred to a
//!   later cut.
//! - The judge backend is supplied by the daemon/plugin via the
//!   [`llm_judge_runner::JudgeBackend`] trait. The runner does not touch
//!   `SessionBackendResolver` directly so this module stays plugin-friendly;
//!   the daemon-side wrapper that routes through the resolver lives in the
//!   workflow_runner plugin.
//! - Eval results are emitted as structured [`EvalCheckResult`] records that
//!   serialize to the `animus.eval.v1` schema; callers append them to the
//!   workflow decision log.
//!
//! # Integration site
//!
//! TODO(codex-p1): the production wire-up that invokes [`run_evals`] after a
//! phase emits an `advance` decision lives in the out-of-tree workflow
//! runner plugin (`launchapp-dev/animus-workflow-runner-default`). The
//! v0.5.5 dispatch documented this as the honest-stop boundary: the
//! in-tree workflow-runner BINARY was deleted in the v0.5.1 round-4
//! fold-in, so no in-tree call site exists today. Adopting this module
//! requires bumping the workflow-runner plugin pin to read
//! `PhaseExecutionDefinition::evals`, call [`run_evals`] with an
//! [`EvalContext`] built from the phase session, gate the persisted phase
//! decision (or persist a `manual_pending` outcome when [`EvalsDecision`]
//! is `Block`), and append the resulting [`EvalCheckResult`] records to
//! `decisions.jsonl`. Until that lands, configured `evals:` blocks parse
//! and validate but are NOT enforced by the runtime.

pub mod command_runner;
pub mod llm_judge_runner;

use std::path::PathBuf;

use orchestrator_config::agent_runtime_config::{EvalCheck, EvalKind, EvalOnFail, EvalsConfig};
use serde::{Deserialize, Serialize};

pub use llm_judge_runner::JudgeBackend;

/// The `schema` value embedded in every persisted eval record. Bumping this
/// constant is a wire-compat break and requires a fold-in note in the
/// runtime-shared changelog.
pub const EVAL_RESULT_SCHEMA_ID: &str = "animus.eval.v1";

/// Default cap for the stdout/stderr excerpt that lands in
/// [`EvalCheckResult::output_excerpt`]. 2 KiB matches the basic-version
/// contract documented in the v0.5.5 dispatch.
pub const DEFAULT_EXCERPT_MAX_BYTES: usize = 2 * 1024;

/// Per-run context the daemon/plugin assembles before invoking the eval
/// runner. The runner does not derive these from disk — that is the caller's
/// responsibility so the runner stays test-friendly and side-effect free.
#[derive(Debug, Clone)]
pub struct EvalContext {
    pub phase_id: String,
    /// Default working directory used when an [`EvalCheck`] does not declare
    /// its own `working_dir`. Typically the worktree (per [`PhaseSession`])
    /// or the project root.
    pub default_working_dir: PathBuf,
    /// Optional summary of the just-produced phase output. Surfaced to llm
    /// judges as the `phase_output_summary` field; commands ignore it.
    pub phase_output_summary: Option<String>,
    /// Per-process excerpt cap override. `None` falls back to
    /// [`DEFAULT_EXCERPT_MAX_BYTES`].
    pub excerpt_max_bytes: Option<usize>,
}

impl EvalContext {
    pub fn new(phase_id: impl Into<String>, default_working_dir: impl Into<PathBuf>) -> Self {
        Self {
            phase_id: phase_id.into(),
            default_working_dir: default_working_dir.into(),
            phase_output_summary: None,
            excerpt_max_bytes: None,
        }
    }

    pub fn with_phase_output_summary(mut self, summary: impl Into<String>) -> Self {
        self.phase_output_summary = Some(summary.into());
        self
    }

    pub fn excerpt_cap(&self) -> usize {
        self.excerpt_max_bytes.unwrap_or(DEFAULT_EXCERPT_MAX_BYTES)
    }
}

/// Structured result for a single eval check. Serializes to the
/// `animus.eval.v1` schema so the daemon can append it to the workflow
/// decision log without further translation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCheckResult {
    pub schema: String,
    pub phase_id: String,
    pub check_id: String,
    pub kind: EvalKind,
    pub passed: bool,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output_excerpt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl EvalCheckResult {
    fn new(
        ctx: &EvalContext,
        check: &EvalCheck,
        passed: bool,
        duration_ms: u64,
        exit_code: Option<i32>,
        output_excerpt: String,
        error: Option<String>,
    ) -> Self {
        Self {
            schema: EVAL_RESULT_SCHEMA_ID.to_string(),
            phase_id: ctx.phase_id.clone(),
            check_id: check.id.clone(),
            kind: check.kind.clone(),
            passed,
            duration_ms,
            exit_code,
            output_excerpt,
            error,
        }
    }
}

/// What the orchestrator decided after scoring the check results. Maps
/// directly onto the YAML semantics documented in [`EvalsConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalsDecision {
    /// `pass_rate >= pass_threshold` — phase may advance.
    Advance,
    /// Gate failed and `on_fail = rework` — caller should re-execute the phase
    /// with the failure context. `rework_attempt_used` is the rework counter
    /// the caller should associate with the next attempt.
    Rework { rework_attempt_used: u32 },
    /// Gate failed and either `on_fail = block` or `max_reworks` exhausted —
    /// caller should pause the workflow.
    Block { reason: String },
}

/// Outcome of running every check in a phase's eval block.
#[derive(Debug, Clone)]
pub struct EvalsOutcome {
    pub results: Vec<EvalCheckResult>,
    pub pass_rate: f32,
    pub decision: EvalsDecision,
}

impl EvalsOutcome {
    pub fn all_passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|r| r.passed)
    }
}

/// Score a result set against the [`EvalsConfig`] gating policy. Used both
/// inline by [`run_evals`] and by tests that exercise the threshold/on_fail
/// matrix without spawning processes.
///
/// `rework_attempts_so_far` is the number of previous reworks already used
/// against this phase + evals block. Pass `0` on the first eval pass.
pub fn evaluate_outcome(
    results: Vec<EvalCheckResult>,
    config: &EvalsConfig,
    rework_attempts_so_far: u32,
) -> EvalsOutcome {
    let total = results.len() as f32;
    let passed = results.iter().filter(|r| r.passed).count() as f32;
    let pass_rate = if total > 0.0 { passed / total } else { 1.0 };

    let threshold = config.pass_threshold;
    let advances = pass_rate + f32::EPSILON >= threshold;

    let decision = if advances {
        EvalsDecision::Advance
    } else {
        match config.on_fail {
            EvalOnFail::Rework if rework_attempts_so_far < config.max_reworks => {
                EvalsDecision::Rework { rework_attempt_used: rework_attempts_so_far + 1 }
            }
            EvalOnFail::Rework => EvalsDecision::Block {
                reason: format!(
                    "eval pass_rate {:.2} below threshold {:.2} and rework budget ({}) exhausted",
                    pass_rate, threshold, config.max_reworks
                ),
            },
            EvalOnFail::Block => EvalsDecision::Block {
                reason: format!("eval pass_rate {:.2} below threshold {:.2}", pass_rate, threshold),
            },
        }
    };

    EvalsOutcome { results, pass_rate, decision }
}

/// Run every check in `config.checks` against `ctx`, route llm_judge checks
/// through `judge` (an `impl JudgeBackend`), and return a scored outcome.
///
/// Checks execute sequentially. A check that errors out (e.g. command failed
/// to spawn, judge backend returned an error) is recorded as failed with
/// `error` populated; it does NOT short-circuit subsequent checks because a
/// downstream check might still surface useful evidence for the human
/// reviewer.
pub async fn run_evals(
    config: &EvalsConfig,
    ctx: &EvalContext,
    judge: Option<&dyn JudgeBackend>,
    rework_attempts_so_far: u32,
) -> EvalsOutcome {
    let mut results = Vec::with_capacity(config.checks.len());
    for check in &config.checks {
        let result = match check.kind {
            EvalKind::Command => command_runner::run_command_check(ctx, check).await,
            EvalKind::LlmJudge => match judge {
                Some(judge) => llm_judge_runner::run_llm_judge_check(ctx, check, judge).await,
                None => EvalCheckResult::new(
                    ctx,
                    check,
                    false,
                    0,
                    None,
                    String::new(),
                    Some("no judge backend supplied — install a provider plugin and re-run".to_string()),
                ),
            },
        };
        results.push(result);
    }
    evaluate_outcome(results, config, rework_attempts_so_far)
}

/// Cap `s` to roughly `max_bytes`, splitting head + tail with an elision
/// marker. Always lands on a UTF-8 boundary so the resulting `String` is
/// valid for `serde_json` round-trip.
pub(crate) fn excerpt(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let half = max_bytes / 2;
    let head_end = floor_char_boundary(s, half);
    let tail_start = ceil_char_boundary(s, s.len().saturating_sub(half));
    let head = &s[..head_end];
    let tail = &s[tail_start..];
    format!("{head}\n…[truncated {} bytes]…\n{tail}", s.len() - head_end - (s.len() - tail_start))
}

fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_config::agent_runtime_config::{EvalCheck, EvalKind, EvalOnFail};

    fn dummy_result(passed: bool) -> EvalCheckResult {
        EvalCheckResult {
            schema: EVAL_RESULT_SCHEMA_ID.to_string(),
            phase_id: "implementation".to_string(),
            check_id: "x".to_string(),
            kind: EvalKind::Command,
            passed,
            duration_ms: 0,
            exit_code: Some(if passed { 0 } else { 1 }),
            output_excerpt: String::new(),
            error: None,
        }
    }

    fn cfg(threshold: f32, on_fail: EvalOnFail, max_reworks: u32) -> EvalsConfig {
        EvalsConfig {
            pass_threshold: threshold,
            on_fail,
            max_reworks,
            checks: vec![EvalCheck {
                id: "x".into(),
                kind: EvalKind::Command,
                command: Some("true".into()),
                args: Vec::new(),
                working_dir: None,
                timeout_secs: None,
                expected_exit: 0,
                agent: None,
                prompt: None,
            }],
        }
    }

    #[test]
    fn evaluate_outcome_advances_at_or_above_threshold() {
        let results = vec![dummy_result(true), dummy_result(true), dummy_result(false)];
        let outcome = evaluate_outcome(results, &cfg(0.5, EvalOnFail::Block, 0), 0);
        assert!((outcome.pass_rate - (2.0 / 3.0)).abs() < 1e-3);
        assert_eq!(outcome.decision, EvalsDecision::Advance);
    }

    #[test]
    fn evaluate_outcome_blocks_below_threshold_with_block_policy() {
        let results = vec![dummy_result(true), dummy_result(false), dummy_result(false)];
        let outcome = evaluate_outcome(results, &cfg(1.0, EvalOnFail::Block, 0), 0);
        match &outcome.decision {
            EvalsDecision::Block { reason } => assert!(reason.contains("pass_rate")),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_outcome_routes_to_rework_when_budget_available() {
        let results = vec![dummy_result(false)];
        let outcome = evaluate_outcome(results, &cfg(1.0, EvalOnFail::Rework, 2), 0);
        assert_eq!(outcome.decision, EvalsDecision::Rework { rework_attempt_used: 1 });
    }

    #[test]
    fn evaluate_outcome_blocks_once_rework_budget_exhausted() {
        let results = vec![dummy_result(false)];
        let outcome = evaluate_outcome(results, &cfg(1.0, EvalOnFail::Rework, 2), 2);
        match outcome.decision {
            EvalsDecision::Block { .. } => (),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_outcome_handles_empty_check_list() {
        let outcome = evaluate_outcome(Vec::new(), &cfg(1.0, EvalOnFail::Block, 0), 0);
        assert_eq!(outcome.pass_rate, 1.0);
        assert_eq!(outcome.decision, EvalsDecision::Advance);
    }

    #[test]
    fn excerpt_passes_short_strings_through() {
        assert_eq!(excerpt("hello", 100), "hello");
    }

    #[test]
    fn excerpt_truncates_long_strings_keeping_head_and_tail() {
        let s = "a".repeat(10_000);
        let trimmed = excerpt(&s, 200);
        assert!(trimmed.len() < 10_000);
        assert!(trimmed.contains("truncated"));
    }

    #[test]
    fn eval_check_result_serializes_to_v1_schema() {
        let res = EvalCheckResult {
            schema: EVAL_RESULT_SCHEMA_ID.to_string(),
            phase_id: "implementation".into(),
            check_id: "unit-tests".into(),
            kind: EvalKind::Command,
            passed: true,
            duration_ms: 1234,
            exit_code: Some(0),
            output_excerpt: "ok".into(),
            error: None,
        };
        let json = serde_json::to_value(&res).expect("serialize");
        assert_eq!(json["schema"], "animus.eval.v1");
        assert_eq!(json["check_id"], "unit-tests");
        assert_eq!(json["kind"], "command");
        assert_eq!(json["passed"], true);
        assert_eq!(json["exit_code"], 0);
    }
}
