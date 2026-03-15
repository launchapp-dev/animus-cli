use serde_json::Value;

use crate::session::session_event::SessionEvent;

pub(crate) fn parse_opencode_json_line(line: &str) -> Vec<SessionEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return vec![SessionEvent::TextDelta { text: line.to_string() }];
    };

    if value.get("type").and_then(Value::as_str) == Some("text") {
        if let Some(text) = value.get("text").and_then(Value::as_str) {
            return vec![SessionEvent::TextDelta { text: text.to_string() }];
        }
    }

    if let Some(text) = value.get("content").and_then(Value::as_str) {
        return vec![SessionEvent::FinalText { text: text.to_string() }];
    }

    Vec::new()
}
