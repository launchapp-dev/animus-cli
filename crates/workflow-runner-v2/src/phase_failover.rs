use std::collections::VecDeque;

use serde_json::Value;

use crate::ipc::collect_json_payload_lines;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseFailureKind {
    TransientRunner,
    ProviderExhaustion { reason: String },
    TargetUnavailable,
    ContextExceeded,
    OutputInvalid,
    Unknown,
}

impl PhaseFailureKind {
    pub fn is_transient_runner(&self) -> bool {
        matches!(self, PhaseFailureKind::TransientRunner)
    }

    pub fn should_failover_target(&self) -> bool {
        matches!(
            self,
            PhaseFailureKind::ProviderExhaustion { .. }
                | PhaseFailureKind::TargetUnavailable
                | PhaseFailureKind::ContextExceeded
        )
    }

    pub fn exhaustion_reason(&self) -> Option<&str> {
        match self {
            PhaseFailureKind::ProviderExhaustion { reason } => Some(reason),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorClass {
    Transient,
    RateLimited,
    AuthFailure,
    ProviderUnavailable,
    ContextExceeded,
    OutputInvalid,
    Permanent,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecommendedAction {
    RetryTransient,
    FallbackModel,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedPhaseError {
    pub error_class: ErrorClass,
    pub provider: Option<String>,
    pub recommended_action: RecommendedAction,
}

pub fn classify_phase_failure(message: &str) -> PhaseFailureKind {
    if is_transient_runner_pattern(message) {
        return PhaseFailureKind::TransientRunner;
    }
    if let Some(reason) = extract_provider_exhaustion_reason(message) {
        return PhaseFailureKind::ProviderExhaustion { reason };
    }
    if is_target_unavailable_pattern(message) {
        return PhaseFailureKind::TargetUnavailable;
    }
    if is_context_exceeded_pattern(message) {
        return PhaseFailureKind::ContextExceeded;
    }
    if is_output_invalid_pattern(message) {
        return PhaseFailureKind::OutputInvalid;
    }
    PhaseFailureKind::Unknown
}

pub fn classify_phase_failure_structured(message: &str) -> ClassifiedPhaseError {
    let kind = classify_phase_failure(message);
    let provider = extract_provider_name(message);
    match kind {
        PhaseFailureKind::TransientRunner => ClassifiedPhaseError {
            error_class: ErrorClass::Transient,
            provider,
            recommended_action: RecommendedAction::RetryTransient,
        },
        PhaseFailureKind::ProviderExhaustion { ref reason } => {
            let normalized = reason.to_ascii_lowercase();
            if normalized.contains("rate limit") || normalized.contains("rate-limit") {
                ClassifiedPhaseError {
                    error_class: ErrorClass::RateLimited,
                    provider,
                    recommended_action: RecommendedAction::FallbackModel,
                }
            } else if normalized.contains("authentication") || normalized.contains("auth") {
                ClassifiedPhaseError {
                    error_class: ErrorClass::AuthFailure,
                    provider,
                    recommended_action: RecommendedAction::Abort,
                }
            } else {
                ClassifiedPhaseError {
                    error_class: ErrorClass::ProviderUnavailable,
                    provider,
                    recommended_action: RecommendedAction::FallbackModel,
                }
            }
        }
        PhaseFailureKind::TargetUnavailable => ClassifiedPhaseError {
            error_class: ErrorClass::Permanent,
            provider,
            recommended_action: RecommendedAction::FallbackModel,
        },
        PhaseFailureKind::ContextExceeded => ClassifiedPhaseError {
            error_class: ErrorClass::ContextExceeded,
            provider,
            recommended_action: RecommendedAction::FallbackModel,
        },
        PhaseFailureKind::OutputInvalid => ClassifiedPhaseError {
            error_class: ErrorClass::OutputInvalid,
            provider,
            recommended_action: RecommendedAction::RetryTransient,
        },
        PhaseFailureKind::Unknown => ClassifiedPhaseError {
            error_class: ErrorClass::Unknown,
            provider,
            recommended_action: RecommendedAction::Abort,
        },
    }
}

fn extract_provider_name(message: &str) -> Option<String> {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("anthropic") || normalized.contains("claude") {
        Some("anthropic".to_string())
    } else if normalized.contains("openai") || normalized.contains("gpt") {
        Some("openai".to_string())
    } else if normalized.contains("google") || normalized.contains("gemini") {
        Some("google".to_string())
    } else {
        None
    }
}

fn is_transient_runner_pattern(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("failed to connect runner")
        || normalized.contains("runner disconnected before workflow")
        || normalized.contains("connection refused")
        || normalized.contains("connection reset by peer")
        || normalized.contains("broken pipe")
        || normalized.contains("timed out")
        || normalized.contains("timeout")
}

fn is_context_exceeded_pattern(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("context length exceeded")
        || normalized.contains("context window exceeded")
        || normalized.contains("maximum context length")
        || normalized.contains("token limit exceeded")
        || normalized.contains("too many tokens")
        || normalized.contains("prompt too long")
        || normalized.contains("context_length_exceeded")
        || normalized.contains("max_tokens_exceeded")
}

fn is_output_invalid_pattern(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("failed to parse phase output")
        || normalized.contains("invalid phase output")
        || normalized.contains("output parse error")
        || normalized.contains("malformed response")
        || normalized.contains("unexpected output format")
        || normalized.contains("phase result deserialization failed")
}

fn is_target_unavailable_pattern(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("missing runtime contract launch for ai cli")
        || normalized.contains("failed to spawn cli process")
        || normalized.contains("no such file or directory")
        || normalized.contains("command not found")
        || normalized.contains("unsupported tool")
        || normalized.contains("unknown model")
        || normalized.contains("invalid model")
        || normalized.contains("missing api key")
        || normalized.contains("missing cli")
        || normalized.contains("model not available")
}

fn extract_provider_exhaustion_reason(text: &str) -> Option<String> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(reason) = provider_exhaustion_reason_from_payload(&payload) {
            return Some(reason);
        }
    }

    let normalized = text.to_ascii_lowercase();
    if normalized.contains("insufficient_quota")
        || normalized.contains("quota exceeded")
        || normalized.contains("quota_exceeded")
    {
        return Some("provider quota exceeded".to_string());
    }
    if normalized.contains("rate limit")
        || normalized.contains("rate-limit")
        || normalized.contains("too many requests")
    {
        return Some("provider rate limit exceeded".to_string());
    }
    if normalized.contains("\"has_credits\":false")
        || normalized.contains("\"balance\":\"0\"")
        || normalized.contains("\"balance\":0")
        || normalized.contains("credits exhausted")
        || normalized.contains("credit balance exhausted")
    {
        return Some("provider credits exhausted".to_string());
    }
    if normalized.contains("secondary") && normalized.contains("used_percent") {
        return Some("secondary token budget exhausted".to_string());
    }
    if normalized.contains("authentication_error")
        || normalized.contains("invalid authentication credentials")
        || normalized.contains("failed to authenticate")
    {
        return Some("provider authentication failed".to_string());
    }

    None
}

pub struct PhaseFailureClassifier;

impl PhaseFailureClassifier {
    pub fn is_transient_runner_error_message(message: &str) -> bool {
        classify_phase_failure(message).is_transient_runner()
    }

    pub fn provider_exhaustion_reason_from_text(text: &str) -> Option<String> {
        match classify_phase_failure(text) {
            PhaseFailureKind::ProviderExhaustion { reason } => Some(reason),
            _ => None,
        }
    }

    pub fn should_failover_target(message: &str) -> bool {
        classify_phase_failure(message).should_failover_target()
    }

    pub fn push_phase_diagnostic_line(lines: &mut VecDeque<String>, text: &str) {
        const MAX_LINE_CHARS: usize = 320;
        const MAX_LINES: usize = 24;
        let mut normalized = text.trim().replace('\n', " ");
        if normalized.chars().count() > MAX_LINE_CHARS {
            normalized = normalized.chars().take(MAX_LINE_CHARS).collect::<String>();
        }
        if normalized.is_empty() {
            return;
        }
        if lines.len() >= MAX_LINES {
            lines.pop_front();
        }
        lines.push_back(normalized);
    }

    pub fn summarize_phase_diagnostics(lines: &VecDeque<String>) -> Option<String> {
        if lines.is_empty() {
            return None;
        }
        Some(lines.iter().cloned().collect::<Vec<_>>().join(" | "))
    }

    pub fn classify_structured(message: &str) -> ClassifiedPhaseError {
        classify_phase_failure_structured(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_runner_errors_retry() {
        let cases = [
            "failed to connect runner",
            "runner disconnected before workflow",
            "connection reset by peer",
            "timed out waiting for response",
        ];
        for msg in cases {
            let result = classify_phase_failure_structured(msg);
            assert_eq!(result.error_class, ErrorClass::Transient, "msg: {}", msg);
            assert_eq!(result.recommended_action, RecommendedAction::RetryTransient, "msg: {}", msg);
        }
    }

    #[test]
    fn rate_limit_errors_fallback() {
        let cases = ["rate limit exceeded", "too many requests", "rate-limit hit"];
        for msg in cases {
            let result = classify_phase_failure_structured(msg);
            assert_eq!(result.error_class, ErrorClass::RateLimited, "msg: {}", msg);
            assert_eq!(result.recommended_action, RecommendedAction::FallbackModel, "msg: {}", msg);
        }
    }

    #[test]
    fn auth_failure_errors_abort() {
        let cases = ["authentication_error occurred", "invalid authentication credentials", "failed to authenticate"];
        for msg in cases {
            let result = classify_phase_failure_structured(msg);
            assert_eq!(result.error_class, ErrorClass::AuthFailure, "msg: {}", msg);
            assert_eq!(result.recommended_action, RecommendedAction::Abort, "msg: {}", msg);
        }
    }

    #[test]
    fn quota_errors_fallback() {
        let cases = ["insufficient_quota for this model", "quota exceeded", "provider credits exhausted"];
        for msg in cases {
            let result = classify_phase_failure_structured(msg);
            assert_eq!(result.error_class, ErrorClass::ProviderUnavailable, "msg: {}", msg);
            assert_eq!(result.recommended_action, RecommendedAction::FallbackModel, "msg: {}", msg);
        }
    }

    #[test]
    fn context_exceeded_errors_fallback() {
        let cases = [
            "context length exceeded",
            "context window exceeded for this request",
            "maximum context length reached",
            "too many tokens in prompt",
            "prompt too long",
            "context_length_exceeded",
        ];
        for msg in cases {
            let kind = classify_phase_failure(msg);
            assert_eq!(kind, PhaseFailureKind::ContextExceeded, "msg: {}", msg);
            let result = classify_phase_failure_structured(msg);
            assert_eq!(result.error_class, ErrorClass::ContextExceeded, "msg: {}", msg);
            assert_eq!(result.recommended_action, RecommendedAction::FallbackModel, "msg: {}", msg);
        }
    }

    #[test]
    fn output_invalid_errors_retry() {
        let cases = [
            "failed to parse phase output",
            "invalid phase output received",
            "output parse error in phase result",
            "malformed response from agent",
        ];
        for msg in cases {
            let kind = classify_phase_failure(msg);
            assert_eq!(kind, PhaseFailureKind::OutputInvalid, "msg: {}", msg);
            let result = classify_phase_failure_structured(msg);
            assert_eq!(result.error_class, ErrorClass::OutputInvalid, "msg: {}", msg);
            assert_eq!(result.recommended_action, RecommendedAction::RetryTransient, "msg: {}", msg);
        }
    }

    #[test]
    fn target_unavailable_errors_fallback() {
        let cases = [
            "unsupported tool requested",
            "missing runtime contract launch for ai cli",
            "command not found: claude",
        ];
        for msg in cases {
            let result = classify_phase_failure_structured(msg);
            assert_eq!(result.error_class, ErrorClass::Permanent, "msg: {}", msg);
            assert_eq!(result.recommended_action, RecommendedAction::FallbackModel, "msg: {}", msg);
        }
    }

    #[test]
    fn unknown_errors_abort() {
        let result = classify_phase_failure_structured("some completely unknown error");
        assert_eq!(result.error_class, ErrorClass::Unknown);
        assert_eq!(result.recommended_action, RecommendedAction::Abort);
    }

    #[test]
    fn provider_extracted_for_anthropic_messages() {
        let result = classify_phase_failure_structured("anthropic api timed out");
        assert_eq!(result.provider, Some("anthropic".to_string()));
        assert_eq!(result.error_class, ErrorClass::Transient);
    }

    #[test]
    fn context_exceeded_should_failover_target() {
        assert!(PhaseFailureKind::ContextExceeded.should_failover_target());
    }

    #[test]
    fn output_invalid_should_not_failover_target() {
        assert!(!PhaseFailureKind::OutputInvalid.should_failover_target());
    }

    #[test]
    fn classifier_struct_classify_structured_delegates() {
        let result = PhaseFailureClassifier::classify_structured("context length exceeded");
        assert_eq!(result.error_class, ErrorClass::ContextExceeded);
        assert_eq!(result.recommended_action, RecommendedAction::FallbackModel);
    }
}

fn parse_numeric_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|number| number as f64))
        .or_else(|| value.as_u64().map(|number| number as f64))
        .or_else(|| value.as_str().and_then(|raw| raw.trim().parse::<f64>().ok()))
}

fn provider_exhaustion_reason_from_payload(payload: &Value) -> Option<String> {
    let secondary_used_percent =
        payload.pointer("/event_msg/token_count/secondary/used_percent").and_then(parse_numeric_value);
    if let Some(used_percent) = secondary_used_percent {
        if used_percent >= 100.0 {
            return Some(format!("secondary token budget exhausted ({:.0}% used)", used_percent));
        }
    }

    let has_credits = payload.pointer("/event_msg/token_count/credits/has_credits").and_then(Value::as_bool);
    if has_credits == Some(false) {
        return Some("provider credits exhausted".to_string());
    }

    let credit_balance = payload.pointer("/event_msg/token_count/credits/balance").and_then(parse_numeric_value);
    if let Some(balance) = credit_balance {
        if balance <= 0.0 {
            return Some("provider credit balance exhausted".to_string());
        }
    }

    let error_code = payload.pointer("/error/code").and_then(Value::as_str).map(|value| value.to_ascii_lowercase());
    if let Some(code) = error_code {
        if code.contains("insufficient_quota")
            || code.contains("quota")
            || code.contains("rate_limit")
            || code.contains("rate-limit")
        {
            return Some(format!("provider returned {}", code));
        }
    }

    let error_type = payload.pointer("/error/type").and_then(Value::as_str).map(|value| value.to_ascii_lowercase());
    if let Some(kind) = error_type {
        if kind.contains("insufficient_quota")
            || kind.contains("quota")
            || kind.contains("rate_limit")
            || kind.contains("rate-limit")
            || kind.contains("authentication_error")
            || kind.contains("auth_error")
        {
            return Some(format!("provider returned {}", kind));
        }
    }

    None
}
