use serde_json::Value;

use crate::ipc::collect_json_payload_lines;

pub fn traverse_payload<T>(
    payload: &Value,
    object_keys: &[&str],
    text_keys: &[&str],
    extractor: &dyn Fn(&Value) -> Option<T>,
    text_extractor: &dyn Fn(&str) -> Option<T>,
) -> Option<T> {
    if let Some(result) = extractor(payload) {
        return Some(result);
    }
    for key in object_keys {
        if let Some(nested) = payload.get(*key) {
            if let Some(result) = extractor(nested) {
                return Some(result);
            }
        }
    }
    for key in text_keys {
        if let Some(text) = payload.get(*key).and_then(Value::as_str) {
            if let Some(result) = text_extractor(text) {
                return Some(result);
            }
        }
    }
    None
}

pub fn traverse_text<T>(
    text: &str,
    extractor: &dyn Fn(&Value) -> Option<T>,
    text_extractor: &dyn Fn(&str) -> Option<T>,
    object_keys: &[&str],
    text_keys: &[&str],
) -> Option<T> {
    let mut last_match = None;
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(result) =
            traverse_payload(&payload, object_keys, text_keys, extractor, text_extractor)
        {
            last_match = Some(result);
        }
    }
    if last_match.is_some() {
        return last_match;
    }
    text_extractor(text)
}

pub fn parse_phase_decision_from_text(
    text: &str,
    phase_id: &str,
) -> Option<orchestrator_core::PhaseDecision> {
    traverse_text(
        text,
        &|payload| extract_phase_decision(payload, phase_id),
        &|_raw| None,
        &["phase_decision", "decision"],
        &[],
    )
}

fn extract_phase_decision(payload: &Value, phase_id: &str) -> Option<orchestrator_core::PhaseDecision> {
    if let Some(nested) = payload.get("phase_decision") {
        if let Some(decision) = try_parse_decision(nested, phase_id) {
            return Some(decision);
        }
    }
    if let Some(nested) = payload.get("decision") {
        if let Some(decision) = try_parse_decision(nested, phase_id) {
            return Some(decision);
        }
    }
    try_parse_decision(payload, phase_id)
}

fn try_parse_decision(value: &Value, phase_id: &str) -> Option<orchestrator_core::PhaseDecision> {
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
    if !kind.eq_ignore_ascii_case("phase_decision") {
        return None;
    }

    let verdict_str = value.get("verdict").and_then(Value::as_str)?;
    let verdict = match verdict_str.trim().to_ascii_lowercase().as_str() {
        "advance" => orchestrator_core::PhaseDecisionVerdict::Advance,
        "rework" => orchestrator_core::PhaseDecisionVerdict::Rework,
        "fail" => orchestrator_core::PhaseDecisionVerdict::Fail,
        "skip" => orchestrator_core::PhaseDecisionVerdict::Skip,
        _ => return None,
    };

    let confidence = value
        .get("confidence")
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0.0) as f32;

    let risk_str = value
        .get("risk")
        .and_then(Value::as_str)
        .unwrap_or("medium");
    let risk = match risk_str.trim().to_ascii_lowercase().as_str() {
        "low" => orchestrator_core::WorkflowDecisionRisk::Low,
        "high" => orchestrator_core::WorkflowDecisionRisk::High,
        _ => orchestrator_core::WorkflowDecisionRisk::Medium,
    };

    let reason = value
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let evidence = value
        .get("evidence")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|ev| {
                    let kind_str = ev.get("kind").and_then(Value::as_str).unwrap_or("custom");
                    let kind = orchestrator_core::parse_evidence_kind(kind_str);
                    let description = ev
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Some(orchestrator_core::PhaseEvidence {
                        kind,
                        description,
                        file_path: ev.get("file_path").and_then(Value::as_str).map(ToOwned::to_owned),
                        value: ev.get("value").cloned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let guardrail_violations = value
        .get("guardrail_violations")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let commit_message = value
        .get("commit_message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    let target_phase = value
        .get("target_phase")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    Some(orchestrator_core::PhaseDecision {
        kind: "phase_decision".to_string(),
        phase_id: phase_id.to_string(),
        verdict,
        confidence,
        risk,
        reason,
        evidence,
        guardrail_violations,
        commit_message,
        target_phase,
    })
}

pub fn parse_commit_message_from_text(text: &str) -> Option<String> {
    traverse_text(
        text,
        &extract_commit_message_from_payload,
        &|_| None,
        &["phase_decision", "decision"],
        &[],
    )
}

fn extract_commit_message_from_payload(payload: &Value) -> Option<String> {
    if let Some(msg) = payload.get("commit_message").and_then(Value::as_str) {
        let trimmed = msg.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for key in &["phase_decision", "decision"] {
        if let Some(nested) = payload.get(*key) {
            if let Some(msg) = nested.get("commit_message").and_then(Value::as_str) {
                let trimmed = msg.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

pub fn fallback_implementation_commit_message(phase_id: &str, subject_title: &str) -> String {
    let phase_label = phase_id.replace('_', " ").replace('-', " ");
    let title = subject_title.trim();
    if title.is_empty() {
        format!("chore: {phase_label} phase completed")
    } else {
        let title_short: String = title.chars().take(60).collect();
        format!("feat({phase_label}): {title_short}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_phase_decision_from_nested_json() {
        let text = r#"{"kind":"implementation_result","commit_message":"feat: add login","phase_decision":{"kind":"phase_decision","phase_id":"implementation","verdict":"advance","confidence":0.95,"risk":"low","reason":"Done","evidence":[]}}"#;
        let decision = parse_phase_decision_from_text(text, "implementation").unwrap();
        assert_eq!(decision.verdict, orchestrator_core::PhaseDecisionVerdict::Advance);
        assert!((decision.confidence - 0.95).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_phase_decision_standalone() {
        let text = r#"{"kind":"phase_decision","phase_id":"triage","verdict":"skip","confidence":0.8,"risk":"low","reason":"already done","evidence":[]}"#;
        let decision = parse_phase_decision_from_text(text, "triage").unwrap();
        assert_eq!(decision.verdict, orchestrator_core::PhaseDecisionVerdict::Skip);
    }

    #[test]
    fn parse_commit_message_from_nested() {
        let text = r#"{"kind":"implementation_result","commit_message":"feat: add feature","phase_decision":{"kind":"phase_decision","verdict":"advance"}}"#;
        let msg = parse_commit_message_from_text(text).unwrap();
        assert_eq!(msg, "feat: add feature");
    }

    #[test]
    fn fallback_commit_message_with_title() {
        let msg = fallback_implementation_commit_message("implementation", "Add login");
        assert!(msg.contains("Add login"));
    }

    #[test]
    fn fallback_commit_message_empty_title() {
        let msg = fallback_implementation_commit_message("implementation", "");
        assert!(msg.contains("implementation"));
    }
}
