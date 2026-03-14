use cli_wrapper::{extract_text_from_line, NormalizedTextEvent};
use serde_json::json;

use super::artifacts::extract_artifact;
use super::events::ParsedEvent;
use super::tool_calls::{extract_tool_name, parse_json_tool_call, parse_xml_tool_parameters};

pub struct OutputParser {
    tool: String,
    thinking_buffer: String,
    tool_buffer: String,
    json_accum: String,
    json_depth: i32,
    in_thinking: bool,
    in_tool_call: bool,
    in_json_accum: bool,
    current_tool: Option<String>,
}

impl OutputParser {
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            thinking_buffer: String::new(),
            tool_buffer: String::new(),
            json_accum: String::new(),
            json_depth: 0,
            in_thinking: bool::default(),
            in_tool_call: bool::default(),
            in_json_accum: false,
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
            let trimmed = line.trim();
            let is_json_parseable = serde_json::from_str::<serde_json::Value>(trimmed).is_ok();

            if !is_json_parseable && (trimmed == "{" || self.in_json_accum) {
                if trimmed == "{" && !self.in_json_accum {
                    self.in_json_accum = true;
                    self.json_accum.clear();
                    self.json_depth = 0;
                }
                self.json_accum.push_str(trimmed);
                self.json_accum.push('\n');
                for ch in trimmed.chars() {
                    match ch {
                        '{' => self.json_depth += 1,
                        '}' => self.json_depth -= 1,
                        _ => {}
                    }
                }
                if self.json_depth <= 0 {
                    self.in_json_accum = false;
                    let accumulated = self.json_accum.clone();
                    self.json_accum.clear();
                    self.json_depth = 0;
                    let text = match extract_text_from_line(&accumulated, &self.tool) {
                        NormalizedTextEvent::TextChunk { text }
                        | NormalizedTextEvent::FinalResult { text } => text,
                        NormalizedTextEvent::Ignored => accumulated,
                    };
                    events.push(ParsedEvent::Output(text));
                }
            } else {
                let text = match extract_text_from_line(line, &self.tool) {
                    NormalizedTextEvent::TextChunk { text }
                    | NormalizedTextEvent::FinalResult { text } => text,
                    NormalizedTextEvent::Ignored => line.to_string(),
                };
                events.push(ParsedEvent::Output(text));
            }
        }

        events
    }
}

impl Default for OutputParser {
    fn default() -> Self {
        Self::new("")
    }
}
