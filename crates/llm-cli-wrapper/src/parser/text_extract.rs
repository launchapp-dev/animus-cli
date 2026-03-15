use serde_json::Value;

use super::text_events::NormalizedTextEvent;
use crate::cli::launch::parse_cli_type;
use crate::cli::types::CliType;

pub fn extract_text_from_line(line: &str, tool: &str) -> NormalizedTextEvent {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return NormalizedTextEvent::Ignored;
    }

    let Ok(obj) = serde_json::from_str::<Value>(trimmed) else {
        return NormalizedTextEvent::Ignored;
    };

    let cli_type = parse_cli_type(tool);

    match cli_type {
        Some(CliType::Claude) => extract_claude(&obj),
        Some(CliType::Codex) => extract_codex(&obj),
        Some(CliType::Gemini) => extract_gemini(&obj),
        Some(CliType::OaiRunner) => extract_oai_runner(&obj),
        Some(CliType::OpenCode) => extract_opencode(&obj),
        _ => extract_generic(&obj),
    }
}

fn extract_claude(obj: &Value) -> NormalizedTextEvent {
    let event_type = obj.get("type").and_then(Value::as_str).unwrap_or("");

    match event_type {
        "content_block_delta" => {
            if let Some(text) = obj.pointer("/delta/text").and_then(Value::as_str) {
                return NormalizedTextEvent::TextChunk { text: text.to_string() };
            }
        }
        "result" => {
            if let Some(text) = obj.get("result").and_then(Value::as_str) {
                return NormalizedTextEvent::FinalResult { text: text.to_string() };
            }
            if let Some(text) = obj.pointer("/result/text").and_then(Value::as_str) {
                return NormalizedTextEvent::FinalResult { text: text.to_string() };
            }
        }
        "assistant" => {
            if let Some(content) = obj.pointer("/message/content").and_then(Value::as_array) {
                let mut text = String::new();
                for block in content {
                    if block.get("type").and_then(Value::as_str) == Some("text") {
                        if let Some(t) = block.get("text").and_then(Value::as_str) {
                            text.push_str(t);
                        }
                    }
                }
                if !text.is_empty() {
                    return NormalizedTextEvent::FinalResult { text };
                }
            }
        }
        "content_block_start" => {
            if let Some(text) = obj.pointer("/content_block/text").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                return NormalizedTextEvent::TextChunk { text: text.to_string() };
            }
        }
        _ => {
            if let Some(text) = obj.get("content").and_then(Value::as_str) {
                return NormalizedTextEvent::TextChunk { text: text.to_string() };
            }
        }
    }

    NormalizedTextEvent::Ignored
}

fn extract_codex(obj: &Value) -> NormalizedTextEvent {
    let event_type = obj.get("type").and_then(Value::as_str).unwrap_or("");

    if !matches!(event_type, "item.completed" | "item.started" | "") {
        return NormalizedTextEvent::Ignored;
    }

    let Some(item) = obj.get("item") else {
        return NormalizedTextEvent::Ignored;
    };

    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    if !matches!(item_type, "agent_message" | "message" | "") {
        return NormalizedTextEvent::Ignored;
    }

    if let Some(text) = item.get("text").and_then(Value::as_str) {
        if !text.is_empty() {
            return NormalizedTextEvent::FinalResult { text: text.to_string() };
        }
    }

    if let Some(content) = item.get("content").and_then(Value::as_array) {
        let mut text = String::new();
        for block in content {
            let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(block_type, "output_text" | "text" | "") {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
        }
        if !text.is_empty() {
            return NormalizedTextEvent::FinalResult { text };
        }
    }

    NormalizedTextEvent::Ignored
}

fn extract_gemini(obj: &Value) -> NormalizedTextEvent {
    let event_type = obj.get("type").and_then(Value::as_str).unwrap_or("");

    if event_type == "partialResult" {
        if let Some(text) = obj.pointer("/partialResult/text").and_then(Value::as_str) {
            return NormalizedTextEvent::TextChunk { text: text.to_string() };
        }
    }

    if let Some(text) = obj.get("text").and_then(Value::as_str) {
        return NormalizedTextEvent::TextChunk { text: text.to_string() };
    }

    if let Some(text) = obj.get("response").and_then(Value::as_str) {
        return NormalizedTextEvent::FinalResult { text: text.to_string() };
    }

    if let Some(text) = obj.pointer("/content/text").and_then(Value::as_str) {
        return NormalizedTextEvent::FinalResult { text: text.to_string() };
    }

    if let Some(parts) = obj.pointer("/content/parts").and_then(Value::as_array) {
        let mut text = String::new();
        for part in parts {
            if let Some(t) = part.get("text").and_then(Value::as_str) {
                text.push_str(t);
            }
        }
        if !text.is_empty() {
            return NormalizedTextEvent::FinalResult { text };
        }
    }

    if let Some(candidates) = obj.get("candidates").and_then(Value::as_array) {
        for candidate in candidates {
            if let Some(parts) = candidate.pointer("/content/parts").and_then(Value::as_array) {
                let mut text = String::new();
                for part in parts {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    }
                }
                if !text.is_empty() {
                    return NormalizedTextEvent::FinalResult { text };
                }
            }
        }
    }

    NormalizedTextEvent::Ignored
}

fn extract_oai_runner(obj: &Value) -> NormalizedTextEvent {
    let event_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "text_chunk" => {
            if let Some(text) = obj.get("text").and_then(Value::as_str) {
                return NormalizedTextEvent::TextChunk { text: text.to_string() };
            }
        }
        "result" => {
            if let Some(text) = obj.get("text").and_then(Value::as_str) {
                return NormalizedTextEvent::FinalResult { text: text.to_string() };
            }
        }
        _ => {}
    }
    NormalizedTextEvent::Ignored
}

fn extract_opencode(obj: &Value) -> NormalizedTextEvent {
    let event_type = obj.get("type").and_then(Value::as_str).unwrap_or("");

    if event_type == "text" {
        if let Some(text) = obj.get("text").and_then(Value::as_str) {
            return NormalizedTextEvent::TextChunk { text: text.to_string() };
        }
    }

    if let Some(text) = obj.get("content").and_then(Value::as_str) {
        return NormalizedTextEvent::TextChunk { text: text.to_string() };
    }

    NormalizedTextEvent::Ignored
}

fn extract_generic(obj: &Value) -> NormalizedTextEvent {
    if let Some(text) = obj.get("text").and_then(Value::as_str) {
        return NormalizedTextEvent::TextChunk { text: text.to_string() };
    }
    if let Some(text) = obj.get("content").and_then(Value::as_str) {
        return NormalizedTextEvent::TextChunk { text: text.to_string() };
    }
    NormalizedTextEvent::Ignored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_content_block_delta() {
        let line = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello world"}}"#;
        assert_eq!(
            extract_text_from_line(line, "claude"),
            NormalizedTextEvent::TextChunk { text: "Hello world".into() }
        );
    }

    #[test]
    fn claude_result_string() {
        let line = r#"{"type":"result","subtype":"success","result":"Final answer here"}"#;
        assert_eq!(
            extract_text_from_line(line, "claude"),
            NormalizedTextEvent::FinalResult { text: "Final answer here".into() }
        );
    }

    #[test]
    fn claude_assistant_message() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Some response"}]}}"#;
        assert_eq!(
            extract_text_from_line(line, "claude"),
            NormalizedTextEvent::FinalResult { text: "Some response".into() }
        );
    }

    #[test]
    fn codex_item_completed_text() {
        let line = r#"{"type":"item.completed","item":{"type":"agent_message","text":"Done!"}}"#;
        assert_eq!(extract_text_from_line(line, "codex"), NormalizedTextEvent::FinalResult { text: "Done!".into() });
    }

    #[test]
    fn codex_item_completed_content_array() {
        let line = r#"{"type":"item.completed","item":{"type":"agent_message","content":[{"type":"output_text","text":"Result text"}]}}"#;
        assert_eq!(
            extract_text_from_line(line, "codex"),
            NormalizedTextEvent::FinalResult { text: "Result text".into() }
        );
    }

    #[test]
    fn codex_ignores_command_execution() {
        let line = r#"{"type":"item.completed","item":{"type":"command_execution","exit_code":0}}"#;
        assert_eq!(extract_text_from_line(line, "codex"), NormalizedTextEvent::Ignored);
    }

    #[test]
    fn gemini_partial_result() {
        let line = r#"{"type":"partialResult","partialResult":{"text":"Streaming chunk"}}"#;
        assert_eq!(
            extract_text_from_line(line, "gemini"),
            NormalizedTextEvent::TextChunk { text: "Streaming chunk".into() }
        );
    }

    #[test]
    fn gemini_root_text() {
        let line = r#"{"text":"Direct text"}"#;
        assert_eq!(
            extract_text_from_line(line, "gemini"),
            NormalizedTextEvent::TextChunk { text: "Direct text".into() }
        );
    }

    #[test]
    fn gemini_content_parts() {
        let line = r#"{"content":{"parts":[{"text":"Part one"},{"text":" part two"}]}}"#;
        assert_eq!(
            extract_text_from_line(line, "gemini"),
            NormalizedTextEvent::FinalResult { text: "Part one part two".into() }
        );
    }

    #[test]
    fn gemini_candidates() {
        let line = r#"{"candidates":[{"content":{"parts":[{"text":"Candidate text"}]}}]}"#;
        assert_eq!(
            extract_text_from_line(line, "gemini"),
            NormalizedTextEvent::FinalResult { text: "Candidate text".into() }
        );
    }

    #[test]
    fn oai_runner_text_chunk() {
        let line = r#"{"type":"text_chunk","text":"Hello"}"#;
        assert_eq!(extract_text_from_line(line, "oai-runner"), NormalizedTextEvent::TextChunk { text: "Hello".into() });
    }

    #[test]
    fn oai_runner_result() {
        let line = r#"{"type":"result","text":"Final output"}"#;
        assert_eq!(
            extract_text_from_line(line, "oai-runner"),
            NormalizedTextEvent::FinalResult { text: "Final output".into() }
        );
    }

    #[test]
    fn oai_runner_tool_call_ignored() {
        let line = r#"{"type":"tool_call","tool_name":"bash","arguments":{}}"#;
        assert_eq!(extract_text_from_line(line, "oai-runner"), NormalizedTextEvent::Ignored);
    }

    #[test]
    fn opencode_text_type() {
        let line = r#"{"type":"text","text":"OpenCode output"}"#;
        assert_eq!(
            extract_text_from_line(line, "opencode"),
            NormalizedTextEvent::TextChunk { text: "OpenCode output".into() }
        );
    }

    #[test]
    fn opencode_content_fallback() {
        let line = r#"{"content":"Fallback content"}"#;
        assert_eq!(
            extract_text_from_line(line, "opencode"),
            NormalizedTextEvent::TextChunk { text: "Fallback content".into() }
        );
    }

    #[test]
    fn generic_text_field() {
        let line = r#"{"text":"generic output"}"#;
        assert_eq!(
            extract_text_from_line(line, "unknown-tool"),
            NormalizedTextEvent::TextChunk { text: "generic output".into() }
        );
    }

    #[test]
    fn non_json_line_ignored() {
        assert_eq!(extract_text_from_line("plain text line", "claude"), NormalizedTextEvent::Ignored);
    }

    #[test]
    fn empty_line_ignored() {
        assert_eq!(extract_text_from_line("", "claude"), NormalizedTextEvent::Ignored);
    }

    #[test]
    fn whitespace_line_ignored() {
        assert_eq!(extract_text_from_line("   ", "claude"), NormalizedTextEvent::Ignored);
    }
}
