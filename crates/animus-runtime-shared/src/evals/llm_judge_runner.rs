//! LLM judge runner. Dispatches a one-shot agent call against the configured
//! `agent` profile, captures the response, and reports pass/fail based on
//! whether the first non-whitespace token is "PASS" (case-insensitive).
//!
//! The runner does not own provider dispatch — it delegates to a
//! [`JudgeBackend`] supplied by the caller. In production the daemon supplies
//! an impl that routes through
//! `orchestrator_plugin_host::session::SessionBackendResolver`; tests use a
//! [`MockJudgeBackend`] under `#[cfg(test)]`. This keeps the runner
//! plugin-friendly and avoids tight coupling between `animus-runtime-shared`
//! and the live session machinery.

use std::time::Instant;

use async_trait::async_trait;
use orchestrator_config::agent_runtime_config::EvalCheck;

use super::{excerpt, EvalCheckResult, EvalContext};

/// Trait the eval runner uses to dispatch a one-shot judge call. The
/// production implementation lives next to the workflow runner; tests use
/// the in-memory mock below.
#[async_trait]
pub trait JudgeBackend: Send + Sync {
    /// Run the judge prompt against the requested agent profile. The
    /// implementation must NOT carry any cross-call state from previous
    /// judge invocations — each call is independent. `phase_output_summary`
    /// is forwarded so the judge prompt can refer to the just-produced
    /// phase output.
    async fn judge(&self, request: JudgeRequest) -> Result<JudgeResponse, JudgeError>;
}

/// Input for a single judge dispatch.
#[derive(Debug, Clone)]
pub struct JudgeRequest {
    pub phase_id: String,
    pub check_id: String,
    pub agent: String,
    pub prompt: String,
    pub phase_output_summary: Option<String>,
}

/// Successful judge response. `text` is the raw response body the judge
/// emitted; the runner inspects it for the leading `PASS` token.
#[derive(Debug, Clone)]
pub struct JudgeResponse {
    pub text: String,
}

/// Errors surfaced by [`JudgeBackend::judge`]. These bubble up into the
/// resulting [`EvalCheckResult`] as `error` with `passed = false`.
#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    #[error("agent profile '{0}' not configured")]
    AgentNotFound(String),
    #[error("provider plugin missing for judge call: {0}")]
    ProviderMissing(String),
    #[error("judge dispatch failed: {0}")]
    DispatchFailed(String),
    #[error("judge response was empty")]
    EmptyResponse,
}

pub(super) async fn run_llm_judge_check(
    ctx: &EvalContext,
    check: &EvalCheck,
    judge: &dyn JudgeBackend,
) -> EvalCheckResult {
    let start = Instant::now();
    let agent = match check.agent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => a.to_string(),
        None => {
            return EvalCheckResult::new(
                ctx,
                check,
                false,
                start.elapsed().as_millis() as u64,
                None,
                String::new(),
                Some("agent field is empty — should have been caught by validation".to_string()),
            );
        }
    };
    let prompt = match check.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => p.to_string(),
        None => {
            return EvalCheckResult::new(
                ctx,
                check,
                false,
                start.elapsed().as_millis() as u64,
                None,
                String::new(),
                Some("prompt field is empty — should have been caught by validation".to_string()),
            );
        }
    };

    let request = JudgeRequest {
        phase_id: ctx.phase_id.clone(),
        check_id: check.id.clone(),
        agent,
        prompt,
        phase_output_summary: ctx.phase_output_summary.clone(),
    };

    let cap = ctx.excerpt_cap();
    let duration_ms_so_far = |start: Instant| start.elapsed().as_millis() as u64;

    match judge.judge(request).await {
        Ok(JudgeResponse { text }) => {
            let first_token = text.split_whitespace().next().unwrap_or("");
            // Strip a trailing punctuation char so prompts like "PASS." /
            // "PASS," / "PASS!" still count — but words that merely START
            // with "PASS" (PASSIVE, PASSAGE, ...) do NOT.
            let normalized = first_token.trim_end_matches(|c: char| c.is_ascii_punctuation());
            let passed = normalized.eq_ignore_ascii_case("PASS");
            let excerpt_text = excerpt(&text, cap);
            EvalCheckResult::new(ctx, check, passed, duration_ms_so_far(start), None, excerpt_text, None)
        }
        Err(err) => EvalCheckResult::new(
            ctx,
            check,
            false,
            duration_ms_so_far(start),
            None,
            String::new(),
            Some(err.to_string()),
        ),
    }
}

#[cfg(test)]
pub(crate) struct MockJudgeBackend {
    pub responses: std::sync::Mutex<Vec<Result<String, JudgeError>>>,
}

#[cfg(test)]
impl MockJudgeBackend {
    pub fn with_responses(responses: Vec<Result<String, JudgeError>>) -> Self {
        Self { responses: std::sync::Mutex::new(responses) }
    }
}

#[cfg(test)]
#[async_trait]
impl JudgeBackend for MockJudgeBackend {
    async fn judge(&self, _request: JudgeRequest) -> Result<JudgeResponse, JudgeError> {
        let next = self.responses.lock().expect("mock judge mutex").pop().unwrap_or(Err(JudgeError::EmptyResponse));
        next.map(|text| JudgeResponse { text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evals::{run_evals, EvalsConfig, EvalsDecision};
    use orchestrator_config::agent_runtime_config::{EvalKind, EvalOnFail};
    use tempfile::TempDir;

    fn judge_check(id: &str, agent: &str, prompt: &str) -> EvalCheck {
        EvalCheck {
            id: id.into(),
            kind: EvalKind::LlmJudge,
            command: None,
            args: Vec::new(),
            working_dir: None,
            timeout_secs: None,
            expected_exit: 0,
            agent: Some(agent.into()),
            prompt: Some(prompt.into()),
        }
    }

    #[tokio::test]
    async fn llm_judge_passes_on_pass_prefix() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = EvalContext::new("implementation", tmp.path().to_path_buf());
        let backend = MockJudgeBackend::with_responses(vec![Ok("PASS — looks clean".into())]);
        let result = run_llm_judge_check(&ctx, &judge_check("q", "po-reviewer", "Verdict?"), &backend).await;
        assert!(result.passed, "expected PASS to drive passed=true, got {result:?}");
        assert_eq!(result.kind, EvalKind::LlmJudge);
        assert_eq!(result.exit_code, None);
    }

    #[tokio::test]
    async fn llm_judge_requires_pass_as_standalone_token() {
        // Regression for codex round-2 P2: a response that merely STARTS with
        // "PASS" (e.g. PASSIVE, PASSAGE) must not satisfy the gate. Only the
        // bare PASS token — optionally followed by punctuation — counts.
        let tmp = TempDir::new().expect("tmp");
        let ctx = EvalContext::new("implementation", tmp.path().to_path_buf());
        for bad in
            ["PASSIVE failure", "PASSAGE — this needs work", "PASSING by — see comments", "Passage details below"]
        {
            let backend = MockJudgeBackend::with_responses(vec![Ok(bad.to_string())]);
            let result = run_llm_judge_check(&ctx, &judge_check("q", "po-reviewer", "Verdict?"), &backend).await;
            assert!(!result.passed, "must NOT accept '{bad}' as PASS, got {result:?}");
        }
        for good in ["PASS", "pass", "Pass.", "PASS!", "pass, ship it"] {
            let backend = MockJudgeBackend::with_responses(vec![Ok(good.to_string())]);
            let result = run_llm_judge_check(&ctx, &judge_check("q", "po-reviewer", "Verdict?"), &backend).await;
            assert!(result.passed, "must accept '{good}' as PASS, got {result:?}");
        }
    }

    #[tokio::test]
    async fn llm_judge_fails_when_pass_token_absent() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = EvalContext::new("implementation", tmp.path().to_path_buf());
        let backend = MockJudgeBackend::with_responses(vec![Ok("Nope, this needs work".into())]);
        let result = run_llm_judge_check(&ctx, &judge_check("q", "po-reviewer", "Verdict?"), &backend).await;
        assert!(!result.passed);
    }

    #[tokio::test]
    async fn llm_judge_surfaces_dispatch_error_as_failure() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = EvalContext::new("implementation", tmp.path().to_path_buf());
        let backend = MockJudgeBackend::with_responses(vec![Err(JudgeError::ProviderMissing("claude".into()))]);
        let result = run_llm_judge_check(&ctx, &judge_check("q", "po-reviewer", "Verdict?"), &backend).await;
        assert!(!result.passed);
        assert!(result.error.as_deref().unwrap_or("").contains("provider plugin missing"), "got {:?}", result.error);
    }

    #[tokio::test]
    async fn run_evals_routes_through_judge_when_configured() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = EvalContext::new("implementation", tmp.path().to_path_buf());
        let config = EvalsConfig {
            pass_threshold: 1.0,
            on_fail: EvalOnFail::Block,
            max_reworks: 0,
            checks: vec![judge_check("q", "po-reviewer", "Does it pass?")],
        };
        let backend = MockJudgeBackend::with_responses(vec![Ok("PASS".into())]);
        let outcome = run_evals(&config, &ctx, Some(&backend), 0).await;
        assert_eq!(outcome.decision, EvalsDecision::Advance);
        assert!(outcome.all_passed());
    }

    #[tokio::test]
    async fn run_evals_marks_judge_check_failed_when_backend_absent() {
        let tmp = TempDir::new().expect("tmp");
        let ctx = EvalContext::new("implementation", tmp.path().to_path_buf());
        let config = EvalsConfig {
            pass_threshold: 1.0,
            on_fail: EvalOnFail::Block,
            max_reworks: 0,
            checks: vec![judge_check("q", "po-reviewer", "Does it pass?")],
        };
        let outcome = run_evals(&config, &ctx, None, 0).await;
        assert!(matches!(outcome.decision, EvalsDecision::Block { .. }));
        assert!(outcome.results.iter().all(|r| !r.passed));
    }
}
