use serde_json::{json, Value};

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
            let tokens = value.get("tokens").cloned().unwrap_or(Value::Null);
            let metadata = json!({
                "type": "oai_runner_usage",
                "tokens": tokens
            });
            vec![SessionEvent::Metadata { metadata }]
        }
        "session_summary" => {
            let tokens = value.get("tokens").cloned().unwrap_or(Value::Null);
            let metadata = json!({
                "type": "oai_runner_session_summary",
                "tokens": tokens
            });
            vec![SessionEvent::Metadata { metadata }]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_metadata_event_to_session_metadata() {
        let line = r#"{"type":"metadata","tokens":{"input":150,"output":75},"cost_usd":0.0015}"#;
        let events = parse_oai_runner_json_line(line);
        assert_eq!(events.len(), 1);
        if let SessionEvent::Metadata { metadata } = &events[0] {
            assert_eq!(metadata["type"], "oai_runner_usage");
            assert_eq!(metadata["tokens"]["input"], 150);
            assert_eq!(metadata["tokens"]["output"], 75);
        } else {
            panic!("expected Metadata event");
        }
    }

    #[test]
    fn parses_session_summary_event_to_session_metadata() {
        let line = r#"{"type":"session_summary","tokens":{"total_input":300,"total_output":150,"total":450,"requests":2}}"#;
        let events = parse_oai_runner_json_line(line);
        assert_eq!(events.len(), 1);
        if let SessionEvent::Metadata { metadata } = &events[0] {
            assert_eq!(metadata["type"], "oai_runner_session_summary");
            assert_eq!(metadata["tokens"]["total_input"], 300);
        } else {
            panic!("expected Metadata event");
        }
    }
}
