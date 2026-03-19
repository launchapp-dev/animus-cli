use std::collections::VecDeque;

use serde_json::Value;

use crate::ipc::collect_json_payload_lines;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseFailureKind {
    TransientRunner,
    ProviderExhaustion { reason: String },
    TargetUnavailable,
    Unknown,
}

impl PhaseFailureKind {
    pub fn is_transient_runner(&self) -> bool {
        matches!(self, PhaseFailureKind::TransientRunner)
    }

    pub fn should_failover_target(&self) -> bool {
        matches!(self, PhaseFailureKind::ProviderExhaustion { .. } | PhaseFailureKind::TargetUnavailable)
    }

    pub fn exhaustion_reason(&self) -> Option<&str> {
        match self {
            PhaseFailureKind::ProviderExhaustion { reason } => Some(reason),
            _ => None,
        }
    }
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
    PhaseFailureKind::Unknown
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
