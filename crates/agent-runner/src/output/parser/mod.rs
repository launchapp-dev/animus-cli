mod artifacts;
mod events;
mod state;
mod tool_calls;

pub use events::ParsedEvent;
pub use state::OutputParser;

#[cfg(test)]
mod tests {
    use super::{OutputParser, ParsedEvent};

    #[test]
    fn parses_json_tool_call_event() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line(
            r#"{"type":"tool_call","tool_name":"phase_transition","arguments":{"target_phase":"implement","reason":"fix review issues"}}"#,
        );
        let tool_event = events
            .into_iter()
            .find_map(|event| match event {
                ParsedEvent::ToolCall {
                    tool_name,
                    parameters,
                    ..
                } => Some((tool_name, parameters)),
                _ => None,
            })
            .expect("tool call event");

        assert_eq!(tool_event.0, "phase_transition");
        assert_eq!(
            tool_event
                .1
                .get("target_phase")
                .and_then(serde_json::Value::as_str),
            Some("implement")
        );
    }

    #[test]
    fn parses_wrapped_tool_call_event() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line(
            r#"{"type":"assistant","tool_call":{"type":"tool_call","function":{"name":"phase_transition","arguments":"{\"target_phase\":\"design\"}"}}}"#,
        );

        let tool_event = events
            .into_iter()
            .find_map(|event| match event {
                ParsedEvent::ToolCall {
                    tool_name,
                    parameters,
                    ..
                } => Some((tool_name, parameters)),
                _ => None,
            })
            .expect("tool call event");

        assert_eq!(tool_event.0, "phase_transition");
        assert_eq!(
            tool_event
                .1
                .get("target_phase")
                .and_then(serde_json::Value::as_str),
            Some("design")
        );
    }

    #[test]
    fn parses_item_wrapped_mcp_tool_call_event() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line(
            r#"{"type":"item.started","item":{"id":"item_7","type":"mcp_tool_call","server":"shortcut","tool":"documents-search","arguments":{"title":"REQ-021"}}}"#,
        );

        let tool_event = events
            .into_iter()
            .find_map(|event| match event {
                ParsedEvent::ToolCall {
                    tool_name,
                    parameters,
                    ..
                } => Some((tool_name, parameters)),
                _ => None,
            })
            .expect("tool call event");

        assert_eq!(tool_event.0, "documents-search");
        assert_eq!(
            tool_event
                .1
                .get("title")
                .and_then(serde_json::Value::as_str),
            Some("REQ-021")
        );
        assert_eq!(
            tool_event
                .1
                .get("server")
                .and_then(serde_json::Value::as_str),
            Some("shortcut")
        );
    }

    #[test]
    fn parses_phase_transition_json_fallback_signal() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line(
            r#"{"type":"phase-transition","target_phase":"design","reason":"clarify product gap"}"#,
        );
        let tool_event = events
            .into_iter()
            .find_map(|event| match event {
                ParsedEvent::ToolCall {
                    tool_name,
                    parameters,
                    ..
                } => Some((tool_name, parameters)),
                _ => None,
            })
            .expect("tool call event");

        assert_eq!(tool_event.0, "phase_transition");
        assert_eq!(
            tool_event
                .1
                .get("target_phase")
                .and_then(serde_json::Value::as_str),
            Some("design")
        );
    }

    #[test]
    fn ignores_placeholder_phase_transition_json_fallback_signal() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line(
            r#"{"type":"phase-transition","target_phase":"VALID_PHASE_ID","reason":"short plain-text reason"}"#,
        );

        assert!(
            events
                .iter()
                .all(|event| !matches!(event, ParsedEvent::ToolCall { tool_name, .. } if tool_name == "phase_transition")),
            "placeholder phase-transition signal should be ignored"
        );
    }

    #[test]
    fn strips_placeholder_reason_from_phase_transition_tool_call() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line(
            r#"{"type":"tool_call","function":{"name":"phase_transition","arguments":"{\"target_phase\":\"implement\",\"reason\":\"short plain-text reason\"}"}}"#,
        );

        let tool_event = events
            .into_iter()
            .find_map(|event| match event {
                ParsedEvent::ToolCall {
                    tool_name,
                    parameters,
                    ..
                } => Some((tool_name, parameters)),
                _ => None,
            })
            .expect("tool call event");

        assert_eq!(tool_event.0, "phase_transition");
        assert_eq!(
            tool_event
                .1
                .get("target_phase")
                .and_then(serde_json::Value::as_str),
            Some("implement")
        );
        assert!(
            tool_event.1.get("reason").is_none(),
            "placeholder reason should be dropped"
        );
    }

    #[test]
    fn parses_xml_tool_call_parameters() {
        let mut parser = OutputParser::new("claude");
        let _ = parser.parse_line("<function_calls>");
        let _ = parser.parse_line(r#"<invoke name="phase_transition">"#);
        let events = parser.parse_line(
            r#"<parameter name="target_phase">"implement"</parameter></function_calls>"#,
        );

        let tool_event = events
            .into_iter()
            .find_map(|event| match event {
                ParsedEvent::ToolCall {
                    tool_name,
                    parameters,
                    ..
                } => Some((tool_name, parameters)),
                _ => None,
            })
            .expect("tool call event");

        assert_eq!(tool_event.0, "phase_transition");
        assert_eq!(
            tool_event
                .1
                .get("target_phase")
                .and_then(serde_json::Value::as_str),
            Some("implement")
        );
    }

    #[test]
    fn does_not_emit_terminal_error_event_from_plain_output_text() {
        let mut parser = OutputParser::new("codex");
        let events = parser.parse_line(
            r#"{"type":"item.completed","item":{"type":"command_execution","aggregated_output":"error: linter warning","exit_code":0,"status":"completed"}}"#,
        );

        assert!(
            events
                .iter()
                .any(|event| matches!(event, ParsedEvent::Output { .. })),
            "expected output event for plain text line"
        );
    }
}
