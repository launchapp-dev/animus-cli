use serde_json::Value;

use crate::session::session_event::SessionEvent;

pub(crate) fn parse_oai_runner_json_line(line: &str) -> Vec<SessionEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return vec![SessionEvent::TextDelta { text: line.to_string() }];
    };

    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "text_chunk" => value
            .get("text")
            .and_then(Value::as_str)
            .map(|text| vec![SessionEvent::TextDelta { text: text.to_string() }])
            .unwrap_or_default(),
        "result" => value
            .get("text")
            .and_then(Value::as_str)
            .map(|text| vec![SessionEvent::FinalText { text: text.to_string() }])
            .unwrap_or_default(),
        "metadata" => {
            let input = value.pointer("/tokens/input").and_then(Value::as_u64).unwrap_or(0);
            let output = value.pointer("/tokens/output").and_then(Value::as_u64).unwrap_or(0);
            if input == 0 && output == 0 {
                return Vec::new();
            }
            vec![SessionEvent::Metadata {
                metadata: serde_json::json!({
                    "type": "oai_runner_usage",
                    "tokens": { "input": input, "output": output }
                }),
            }]
        }
        "session_summary" => {
            let total_input = value.pointer("/tokens/total_input").and_then(Value::as_u64).unwrap_or(0);
            let total_output = value.pointer("/tokens/total_output").and_then(Value::as_u64).unwrap_or(0);
            if total_input == 0 && total_output == 0 {
                return Vec::new();
            }
            vec![SessionEvent::Metadata {
                metadata: serde_json::json!({
                    "type": "oai_runner_session_summary",
                    "tokens": {
                        "total_input": total_input,
                        "total_output": total_output,
                        "total": value.pointer("/tokens/total").and_then(Value::as_u64).unwrap_or(total_input + total_output),
                        "requests": value.pointer("/tokens/requests").and_then(Value::as_u64).unwrap_or(0)
                    }
                }),
            }]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_event_emits_session_metadata() {
        let line = r#"{"type":"metadata","tokens":{"input":1500,"output":300}}"#;
        let events = parse_oai_runner_json_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            SessionEvent::Metadata { metadata } => {
                assert_eq!(metadata["type"], "oai_runner_usage");
                assert_eq!(metadata["tokens"]["input"], 1500);
                assert_eq!(metadata["tokens"]["output"], 300);
            }
            _ => panic!("expected Metadata event"),
        }
    }

    #[test]
    fn metadata_event_with_zero_tokens_is_ignored() {
        let line = r#"{"type":"metadata","tokens":{"input":0,"output":0}}"#;
        assert!(parse_oai_runner_json_line(line).is_empty());
    }

    #[test]
    fn session_summary_event_emits_session_metadata() {
        let line = r#"{"type":"session_summary","tokens":{"total_input":5000,"total_output":1000,"total":6000,"requests":4}}"#;
        let events = parse_oai_runner_json_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            SessionEvent::Metadata { metadata } => {
                assert_eq!(metadata["type"], "oai_runner_session_summary");
                assert_eq!(metadata["tokens"]["total_input"], 5000);
                assert_eq!(metadata["tokens"]["total_output"], 1000);
                assert_eq!(metadata["tokens"]["total"], 6000);
                assert_eq!(metadata["tokens"]["requests"], 4);
            }
            _ => panic!("expected Metadata event"),
        }
    }

    #[test]
    fn unknown_json_event_type_is_ignored() {
        let line = r#"{"type":"tool_call","tool_name":"bash"}"#;
        assert!(parse_oai_runner_json_line(line).is_empty());
    }
}
