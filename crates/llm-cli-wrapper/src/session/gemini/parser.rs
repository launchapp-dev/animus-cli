use serde_json::{json, Value};

use crate::session::session_event::SessionEvent;

pub(crate) fn parse_gemini_json_chunk(chunk: &str) -> Vec<SessionEvent> {
    let trimmed = chunk.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return vec![SessionEvent::TextDelta { text: chunk.to_string() }];
    };

    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    if event_type == "partialResult" {
        if let Some(text) = value.pointer("/partialResult/text").and_then(Value::as_str) {
            return vec![SessionEvent::TextDelta { text: text.to_string() }];
        }
    }

    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return vec![SessionEvent::TextDelta { text: text.to_string() }];
    }

    let mut events = Vec::new();

    if let Some(session_id) = value.get("session_id") {
        events.push(SessionEvent::Metadata {
            metadata: json!({
                "type": "gemini_session",
                "session_id": session_id,
            }),
        });
    }

    if let Some(stats) = value.get("stats") {
        events.push(SessionEvent::Metadata {
            metadata: json!({
                "type": "gemini_stats",
                "stats": stats,
            }),
        });
    }

    if let Some(text) = extract_gemini_final_text(&value) {
        events.push(SessionEvent::FinalText { text });
    }

    events
}

fn extract_gemini_final_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("response").and_then(Value::as_str) {
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }

    if let Some(text) = value.pointer("/content/text").and_then(Value::as_str) {
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }

    if let Some(parts) = value.pointer("/content/parts").and_then(Value::as_array) {
        let mut text = String::new();
        for part in parts {
            if let Some(segment) = part.get("text").and_then(Value::as_str) {
                text.push_str(segment);
            }
        }
        if !text.is_empty() {
            return Some(text);
        }
    }

    if let Some(candidates) = value.get("candidates").and_then(Value::as_array) {
        for candidate in candidates {
            if let Some(parts) = candidate.pointer("/content/parts").and_then(Value::as_array) {
                let mut text = String::new();
                for part in parts {
                    if let Some(segment) = part.get("text").and_then(Value::as_str) {
                        text.push_str(segment);
                    }
                }
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
    }

    None
}
