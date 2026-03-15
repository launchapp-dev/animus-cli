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
        _ => Vec::new(),
    }
}
