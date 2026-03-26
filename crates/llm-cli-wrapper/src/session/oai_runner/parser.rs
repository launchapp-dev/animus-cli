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
        "tool_call" => value
            .get("tool_name")
            .and_then(Value::as_str)
            .map(|tool_name| {
                vec![SessionEvent::ToolCall {
                    tool_name: tool_name.to_string(),
                    arguments: value.get("arguments").cloned().unwrap_or_else(|| json!({})),
                    server: value.get("server").and_then(Value::as_str).map(ToString::to_string),
                }]
            })
            .unwrap_or_default(),
        "tool_result" => value
            .get("tool_name")
            .and_then(Value::as_str)
            .map(|tool_name| {
                vec![SessionEvent::ToolResult {
                    tool_name: tool_name.to_string(),
                    output: value.get("output").cloned().unwrap_or(Value::Null),
                    success: true,
                }]
            })
            .unwrap_or_default(),
        "tool_error" => value
            .get("tool_name")
            .and_then(Value::as_str)
            .map(|tool_name| {
                vec![SessionEvent::ToolResult {
                    tool_name: tool_name.to_string(),
                    output: json!({
                        "error": value.get("error").cloned().unwrap_or(Value::Null),
                    }),
                    success: false,
                }]
            })
            .unwrap_or_default(),
        "metadata" | "cost" | "session_summary" => vec![SessionEvent::Metadata { metadata: value }],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_oai_runner_json_line;
    use crate::session::session_event::SessionEvent;
    use serde_json::json;

    #[test]
    fn parses_oai_runner_structured_tool_and_metadata_events() {
        let tool_call = parse_oai_runner_json_line(
            r#"{"type":"tool_call","tool_name":"search_query","arguments":{"q":"rust"},"server":"web"}"#,
        );
        assert_eq!(
            tool_call,
            vec![SessionEvent::ToolCall {
                tool_name: "search_query".to_string(),
                arguments: json!({"q":"rust"}),
                server: Some("web".to_string()),
            }]
        );

        let tool_result =
            parse_oai_runner_json_line(r#"{"type":"tool_result","tool_name":"search_query","output":{"hits":3}}"#);
        assert_eq!(
            tool_result,
            vec![SessionEvent::ToolResult {
                tool_name: "search_query".to_string(),
                output: json!({"hits":3}),
                success: true,
            }]
        );

        let tool_error =
            parse_oai_runner_json_line(r#"{"type":"tool_error","tool_name":"search_query","error":"boom"}"#);
        assert_eq!(
            tool_error,
            vec![SessionEvent::ToolResult {
                tool_name: "search_query".to_string(),
                output: json!({"error":"boom"}),
                success: false,
            }]
        );

        let metadata = parse_oai_runner_json_line(
            r#"{"type":"metadata","tokens":{"input":10,"output":4,"total":14},"cost_usd":0.01}"#,
        );
        assert!(matches!(metadata.first(), Some(SessionEvent::Metadata { .. })));
    }
}
