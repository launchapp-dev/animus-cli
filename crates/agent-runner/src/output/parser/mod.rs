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

    #[test]
    fn malformed_json_emits_output_event_not_tool_call() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line(r#"{"type":"tool_call","tool_name":"phase_transition","#);

        assert!(
            events
                .iter()
                .all(|event| !matches!(event, ParsedEvent::ToolCall { .. })),
            "malformed JSON should not produce a tool call event"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ParsedEvent::Output { .. })),
            "malformed JSON should produce an output event"
        );
    }

    #[test]
    fn empty_line_emits_no_events() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line("");
        assert!(events.is_empty(), "empty line should produce no events");

        let events = parser.parse_line("   ");
        assert!(
            events.is_empty(),
            "whitespace-only line should produce no events"
        );
    }

    #[test]
    fn thinking_block_emits_thinking_event() {
        let mut parser = OutputParser::new("claude");
        let events_open = parser.parse_line("<thinking>");
        assert!(events_open.is_empty());

        let events_content = parser.parse_line("I need to analyze the requirements.");
        assert!(events_content.is_empty());

        let events_close = parser.parse_line("</thinking>");
        let thinking = events_close
            .iter()
            .find_map(|event| match event {
                ParsedEvent::Thinking(text) => Some(text.clone()),
                _ => None,
            })
            .expect("thinking event should be emitted on close tag");

        assert!(thinking.contains("I need to analyze the requirements."));
    }

    #[test]
    fn thinking_block_inline_emits_thinking_event() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line("<thinking>quick thought</thinking>");
        let thinking = events
            .iter()
            .find_map(|event| match event {
                ParsedEvent::Thinking(text) => Some(text.clone()),
                _ => None,
            })
            .expect("inline thinking block should emit thinking event");

        assert!(thinking.contains("quick thought"));
    }

    #[test]
    fn artifact_created_line_emits_artifact_event() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line("artifact created: src/main.rs extra text");
        let artifact = events
            .iter()
            .find_map(|event| match event {
                ParsedEvent::Artifact(info) => Some(info.clone()),
                _ => None,
            })
            .expect("artifact event should be emitted");

        assert_eq!(artifact.file_path.as_deref(), Some("src/main.rs"));
        assert_eq!(artifact.artifact_type, protocol::ArtifactType::Code);
    }

    #[test]
    fn file_created_line_emits_artifact_event() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line("file created: output.json");
        let artifact = events
            .iter()
            .find_map(|event| match event {
                ParsedEvent::Artifact(info) => Some(info.clone()),
                _ => None,
            })
            .expect("file created event should be emitted");

        assert_eq!(artifact.file_path.as_deref(), Some("output.json"));
        assert_eq!(artifact.artifact_type, protocol::ArtifactType::Data);
    }

    #[test]
    fn artifact_type_inferred_from_extension() {
        let mut parser = OutputParser::new("claude");

        let mut check = |line: &str, expected: protocol::ArtifactType| {
            let events = parser.parse_line(line);
            let artifact = events
                .iter()
                .find_map(|event| match event {
                    ParsedEvent::Artifact(info) => Some(info.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("expected artifact for: {}", line));
            assert_eq!(artifact.artifact_type, expected, "wrong type for: {}", line);
        };

        check("artifact created: photo.png", protocol::ArtifactType::Image);
        check(
            "artifact created: readme.md",
            protocol::ArtifactType::Document,
        );
        check(
            "artifact created: unknown.xyz",
            protocol::ArtifactType::File,
        );
    }

    #[test]
    fn plain_text_emits_output_event() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line("Hello, this is some plain text output.");

        let output = events
            .iter()
            .find_map(|event| match event {
                ParsedEvent::Output { text } => Some(text.clone()),
                _ => None,
            })
            .expect("plain text should produce output event");

        assert!(!output.is_empty());
    }

    #[test]
    fn incomplete_xml_tool_call_does_not_emit_until_closed() {
        let mut parser = OutputParser::new("claude");
        let events1 = parser.parse_line("<function_calls>");
        assert!(
            events1
                .iter()
                .all(|e| !matches!(e, ParsedEvent::ToolCall { .. })),
            "should not emit tool call before close tag"
        );

        let events2 = parser.parse_line(r#"<invoke name="some_tool">"#);
        assert!(
            events2
                .iter()
                .all(|e| !matches!(e, ParsedEvent::ToolCall { .. })),
            "should not emit tool call before close tag"
        );

        let events3 = parser.parse_line("</function_calls>");
        let tool_event = events3
            .iter()
            .find_map(|event| match event {
                ParsedEvent::ToolCall { tool_name, .. } => Some(tool_name.clone()),
                _ => None,
            })
            .expect("close tag should trigger tool call emission");
        assert_eq!(tool_event, "some_tool");
    }

    #[test]
    fn multiple_json_formats_parsed_in_sequence() {
        let mut parser = OutputParser::new("claude");

        let events1 = parser.parse_line(
            r#"{"type":"tool_call","tool_name":"read_file","arguments":{"path":"src/lib.rs"}}"#,
        );
        assert!(events1.iter().any(|e| matches!(e, ParsedEvent::ToolCall { tool_name, .. } if tool_name == "read_file")));

        let events2 = parser.parse_line(
            r#"{"type":"tool_use","name":"write_file","input":{"path":"out.rs","content":"fn main(){}"}}"#,
        );
        assert!(events2.iter().any(|e| matches!(e, ParsedEvent::ToolCall { tool_name, .. } if tool_name == "write_file")));

        let events3 = parser.parse_line(
            r#"{"type":"function_call","function":{"name":"search","arguments":"{\"query\":\"test\"}"}}"#,
        );
        assert!(events3.iter().any(|e| matches!(e, ParsedEvent::ToolCall { tool_name, .. } if tool_name == "search")));
    }

    #[test]
    fn json_without_tool_call_structure_emits_output() {
        let mut parser = OutputParser::new("claude");
        let events = parser.parse_line(r#"{"status":"ok","message":"all tests passed"}"#);

        assert!(
            events
                .iter()
                .all(|e| !matches!(e, ParsedEvent::ToolCall { .. })),
            "non-tool-call JSON should not emit tool call"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ParsedEvent::Output { .. })),
            "non-tool-call JSON should emit output"
        );
    }
}
