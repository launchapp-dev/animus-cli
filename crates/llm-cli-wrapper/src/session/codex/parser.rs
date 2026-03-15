use serde_json::{json, Value};

use crate::session::session_event::SessionEvent;

pub(crate) fn parse_codex_stdout_line(line: &str) -> Vec<SessionEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return vec![SessionEvent::TextDelta { text: line.to_string() }];
    };

    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "thread.started" | "turn.started" => vec![SessionEvent::Metadata { metadata: value }],
        "turn.completed" => parse_codex_turn_completed(&value),
        "item.completed" => parse_codex_item_completed(&value),
        _ => Vec::new(),
    }
}

fn parse_codex_turn_completed(value: &Value) -> Vec<SessionEvent> {
    let usage = value.get("usage").cloned().unwrap_or_else(|| json!({}));
    vec![SessionEvent::Metadata {
        metadata: json!({
            "type": "codex_usage",
            "usage": usage,
        }),
    }]
}

fn parse_codex_item_completed(value: &Value) -> Vec<SessionEvent> {
    let Some(item) = value.get("item") else {
        return Vec::new();
    };

    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    match item_type {
        "reasoning" => item
            .get("text")
            .and_then(Value::as_str)
            .map(|text| vec![SessionEvent::Thinking { text: text.to_string() }])
            .unwrap_or_default(),
        "agent_message" | "message" => parse_codex_message_item(item),
        _ => Vec::new(),
    }
}

fn parse_codex_message_item(item: &Value) -> Vec<SessionEvent> {
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        if !text.is_empty() {
            return vec![SessionEvent::FinalText { text: text.to_string() }];
        }
    }

    let Some(content) = item.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut text = String::new();
    for block in content {
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(block_type, "output_text" | "text") {
            if let Some(segment) = block.get("text").and_then(Value::as_str) {
                text.push_str(segment);
            }
        }
    }

    if text.is_empty() {
        Vec::new()
    } else {
        vec![SessionEvent::FinalText { text }]
    }
}
