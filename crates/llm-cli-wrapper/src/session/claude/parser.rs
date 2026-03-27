use serde_json::{json, Value};

use crate::session::session_event::SessionEvent;

pub(crate) fn parse_claude_stdout_line(line: &str) -> Vec<SessionEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return vec![SessionEvent::TextDelta { text: line.to_string() }];
    };

    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "system" => parse_claude_system_event(&value),
        "assistant" => parse_claude_assistant_event(&value),
        "result" => parse_claude_result_event(&value),
        "content_block_start" => parse_claude_content_block_start(&value),
        "content_block_delta" => parse_claude_content_block_delta(&value),
        "rate_limit_event" => vec![SessionEvent::Metadata { metadata: value }],
        "user" => parse_claude_user_event(&value),
        _ => Vec::new(),
    }
}

fn parse_claude_system_event(value: &Value) -> Vec<SessionEvent> {
    vec![SessionEvent::Metadata { metadata: value.clone() }]
}

fn parse_claude_assistant_event(value: &Value) -> Vec<SessionEvent> {
    let mut events = Vec::new();

    if let Some(usage) = value.pointer("/message/usage") {
        events.push(SessionEvent::Metadata {
            metadata: json!({
                "type": "claude_usage",
                "usage": usage,
                "session_id": value.get("session_id").cloned().unwrap_or(Value::Null),
            }),
        });
    }

    if let Some(content) = value.pointer("/message/content").and_then(Value::as_array) {
        let mut text = String::new();
        for block in content {
            let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
            match block_type {
                "text" => {
                    if let Some(segment) = block.get("text").and_then(Value::as_str) {
                        text.push_str(segment);
                    }
                }
                "thinking" => {
                    if let Some(segment) = block.get("thinking").and_then(Value::as_str) {
                        events.push(SessionEvent::Thinking { text: segment.to_string() });
                    }
                }
                "tool_use" => {
                    let tool_name = block.get("name").and_then(Value::as_str).unwrap_or("unknown_tool").to_string();
                    let arguments = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    events.push(SessionEvent::ToolCall { tool_name, arguments, server: None });
                }
                "tool_result" => {
                    let tool_name =
                        block.get("tool_name").and_then(Value::as_str).unwrap_or("unknown_tool").to_string();
                    events.push(SessionEvent::ToolResult {
                        tool_name,
                        output: block.get("content").cloned().unwrap_or(Value::Null),
                        success: !block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    });
                }
                _ => {}
            }
        }
        if !text.is_empty() {
            events.push(SessionEvent::FinalText { text });
        }
    }

    events
}

fn parse_claude_user_event(value: &Value) -> Vec<SessionEvent> {
    let content = value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .or_else(|| value.get("content").and_then(Value::as_array));

    let Some(content) = content else {
        return Vec::new();
    };

    let mut events = Vec::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }

        let tool_name = resolve_tool_name_from_result(block, value);

        let output = block
            .get("content")
            .cloned()
            .or_else(|| value.pointer("/tool_use_result/content").cloned())
            .unwrap_or(Value::Null);

        let is_error = block.get("is_error").and_then(Value::as_bool).unwrap_or(false);

        events.push(SessionEvent::ToolResult { tool_name, output, success: !is_error });
    }
    events
}

fn resolve_tool_name_from_result(block: &Value, root: &Value) -> String {
    if let Some(items) = block.get("content").and_then(Value::as_array) {
        for item in items {
            if item.get("type").and_then(Value::as_str) == Some("tool_reference") {
                if let Some(name) = item.get("tool_name").and_then(Value::as_str) {
                    return name.to_string();
                }
            }
        }
    }

    if let Some(tool) = root.pointer("/tool_use_result/structuredContent/tool").and_then(Value::as_str) {
        return tool.to_string();
    }

    if let Some(content) = root.pointer("/tool_use_result/content").and_then(Value::as_str) {
        if let Ok(parsed) = serde_json::from_str::<Value>(content) {
            if let Some(tool) = parsed.get("tool").and_then(Value::as_str) {
                return tool.to_string();
            }
        }
    }

    block.get("tool_use_id").and_then(Value::as_str).unwrap_or("unknown").to_string()
}

fn parse_claude_result_event(value: &Value) -> Vec<SessionEvent> {
    if value.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        let message = value.get("result").and_then(Value::as_str).unwrap_or("claude session failed").to_string();
        return vec![SessionEvent::Error { message, recoverable: false }];
    }

    let Some(text) = value.get("result").and_then(Value::as_str) else {
        return Vec::new();
    };

    vec![SessionEvent::FinalText { text: text.to_string() }]
}

fn parse_claude_content_block_start(value: &Value) -> Vec<SessionEvent> {
    let Some(block) = value.get("content_block") else {
        return Vec::new();
    };

    match block.get("type").and_then(Value::as_str).unwrap_or("") {
        "text" => block
            .get("text")
            .and_then(Value::as_str)
            .map(|text| vec![SessionEvent::TextDelta { text: text.to_string() }])
            .unwrap_or_default(),
        "thinking" => block
            .get("thinking")
            .and_then(Value::as_str)
            .map(|text| vec![SessionEvent::Thinking { text: text.to_string() }])
            .unwrap_or_default(),
        "tool_use" => {
            let tool_name = block.get("name").and_then(Value::as_str).unwrap_or("unknown_tool").to_string();
            let arguments = block.get("input").cloned().unwrap_or_else(|| json!({}));
            vec![SessionEvent::ToolCall { tool_name, arguments, server: None }]
        }
        _ => Vec::new(),
    }
}

fn parse_claude_content_block_delta(value: &Value) -> Vec<SessionEvent> {
    let Some(delta) = value.get("delta") else {
        return Vec::new();
    };

    match delta.get("type").and_then(Value::as_str).unwrap_or("") {
        "text_delta" => delta
            .get("text")
            .and_then(Value::as_str)
            .map(|text| vec![SessionEvent::TextDelta { text: text.to_string() }])
            .unwrap_or_default(),
        "thinking_delta" => delta
            .get("thinking")
            .and_then(Value::as_str)
            .map(|text| vec![SessionEvent::Thinking { text: text.to_string() }])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}
