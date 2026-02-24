use serde_json::json;

use super::artifacts::extract_artifact;
use super::events::ParsedEvent;
use super::tool_calls::{extract_tool_name, parse_json_tool_call, parse_xml_tool_parameters};

pub struct OutputParser {
    thinking_buffer: String,
    tool_buffer: String,
    in_thinking: bool,
    in_tool_call: bool,
    current_tool: Option<String>,
}

impl OutputParser {
    pub fn new() -> Self {
        Self {
            thinking_buffer: String::new(),
            tool_buffer: String::new(),
            in_thinking: bool::default(),
            in_tool_call: bool::default(),
            current_tool: None,
        }
    }

    pub fn parse_line(&mut self, line: &str) -> Vec<ParsedEvent> {
        let mut events = Vec::new();

        if let Some((tool_name, parameters)) = parse_json_tool_call(line) {
            events.push(ParsedEvent::ToolCall {
                tool_name,
                parameters,
            });
        }

        if line.contains("<thinking>") {
            self.in_thinking = true;
            self.thinking_buffer.clear();
        }

        if self.in_thinking {
            self.thinking_buffer.push_str(line);
            self.thinking_buffer.push('\n');
        }

        if line.contains("</thinking>") {
            self.in_thinking = false;
            if !self.thinking_buffer.is_empty() {
                events.push(ParsedEvent::Thinking(self.thinking_buffer.clone()));
                self.thinking_buffer.clear();
            }
        }

        if line.contains("<function_calls>") || line.contains("<tool_use") {
            self.in_tool_call = true;
            self.tool_buffer.clear();
            self.current_tool = extract_tool_name(line);
        }

        if self.in_tool_call {
            self.tool_buffer.push_str(line);
            self.tool_buffer.push('\n');
            if self.current_tool.is_none() {
                self.current_tool = extract_tool_name(line);
            }
        }

        if line.contains("</function_calls>") || line.contains("</tool_use>") {
            self.in_tool_call = false;
            if let Some(tool_name) = self.current_tool.take() {
                let tool_content = self.tool_buffer.clone();
                let parameters = parse_xml_tool_parameters(&tool_content)
                    .unwrap_or_else(|| json!({ "content": tool_content }));
                events.push(ParsedEvent::ToolCall {
                    tool_name,
                    parameters,
                });
                self.tool_buffer.clear();
            }
        }

        if line.contains("artifact created:") || line.contains("file created:") {
            if let Some(artifact) = extract_artifact(line) {
                events.push(ParsedEvent::Artifact(artifact));
            }
        }

        if !self.in_thinking && !self.in_tool_call && !line.trim().is_empty() {
            events.push(ParsedEvent::Output);
        }

        events
    }
}

impl Default for OutputParser {
    fn default() -> Self {
        Self::new()
    }
}
