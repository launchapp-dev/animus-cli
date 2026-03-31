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
        "tool_call" => {
            let tool_name = value.get("tool_name").and_then(Value::as_str).unwrap_or("unknown_tool").to_string();
            let arguments = value.get("arguments").cloned().unwrap_or_else(|| serde_json::json!({}));
            vec![SessionEvent::ToolCall { tool_name, arguments, server: None }]
        }
        "tool_result" => {
            let tool_name = value.get("tool_name").and_then(Value::as_str).unwrap_or("unknown_tool").to_string();
            let output = value.get("output").cloned().or_else(|| value.get("result").cloned()).unwrap_or(Value::Null);
            let success = value.get("success").and_then(Value::as_bool).unwrap_or(true);
            vec![SessionEvent::ToolResult { tool_name, output, success }]
        }
        "tool_error" => {
            let tool_name = value.get("tool_name").and_then(Value::as_str).unwrap_or("unknown_tool");
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .or_else(|| value.get("message").and_then(Value::as_str))
                .unwrap_or("oai-runner tool failed");
            vec![SessionEvent::Error { message: format!("{tool_name}: {message}"), recoverable: false }]
        }
        "metadata" | "cost" | "session_summary" => vec![SessionEvent::Metadata { metadata: value }],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::parse_oai_runner_json_line;
    use crate::session::SessionEvent;

    #[test]
    fn parse_oai_runner_preserves_tool_call_and_result_events() {
        let tool_call = parse_oai_runner_json_line(
            r#"{"type":"tool_call","tool_name":"read_file","arguments":{"path":"alpha.txt"}}"#,
        );
        assert!(matches!(
            tool_call.as_slice(),
            [SessionEvent::ToolCall { tool_name, arguments, server: None }]
            if tool_name == "read_file" && arguments == &json!({"path":"alpha.txt"})
        ));

        let tool_result = parse_oai_runner_json_line(
            r#"{"type":"tool_result","tool_name":"read_file","output":{"content":"alpha"}}"#,
        );
        assert!(matches!(
            tool_result.as_slice(),
            [SessionEvent::ToolResult { tool_name, output, success }]
            if tool_name == "read_file" && output == &json!({"content":"alpha"}) && *success
        ));
    }

    #[test]
    fn parse_oai_runner_preserves_metadata_and_final_text() {
        let metadata = parse_oai_runner_json_line(r#"{"type":"metadata","tokens":{"input":1}}"#);
        assert!(matches!(
            metadata.as_slice(),
            [SessionEvent::Metadata { metadata: value }]
            if value == &json!({"type":"metadata","tokens":{"input":1}})
        ));

        let final_text = parse_oai_runner_json_line(r#"{"type":"result","text":"PINEAPPLE_42"}"#);
        assert_eq!(final_text, vec![SessionEvent::FinalText { text: "PINEAPPLE_42".to_string() }]);
    }
}
