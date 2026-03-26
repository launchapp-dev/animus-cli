use crate::invalid_input_error;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputTailEventType {
    Output,
    Error,
    Thinking,
    ToolCall,
    ToolResult,
    Artifact,
    Metadata,
    Finished,
}

impl OutputTailEventType {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Output => "output",
            Self::Error => "error",
            Self::Thinking => "thinking",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Artifact => "artifact",
            Self::Metadata => "metadata",
            Self::Finished => "finished",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct OutputTailEventRecord {
    pub(super) event_type: String,
    pub(super) run_id: String,
    pub(super) text: String,
    pub(super) source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) stream_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) data: Option<Value>,
}

#[derive(Debug, Clone)]
pub(super) struct OutputTailResolution {
    pub(super) run_id: String,
    pub(super) run_dir: PathBuf,
    pub(super) resolved_from: &'static str,
}

pub(super) fn parse_output_tail_event_type(value: &str) -> Result<OutputTailEventType> {
    match value.trim().to_ascii_lowercase().as_str() {
        "output" => Ok(OutputTailEventType::Output),
        "error" => Ok(OutputTailEventType::Error),
        "thinking" => Ok(OutputTailEventType::Thinking),
        "tool_call" => Ok(OutputTailEventType::ToolCall),
        "tool_result" => Ok(OutputTailEventType::ToolResult),
        "artifact" => Ok(OutputTailEventType::Artifact),
        "metadata" => Ok(OutputTailEventType::Metadata),
        "finished" => Ok(OutputTailEventType::Finished),
        _ => Err(invalid_input_error(format!(
            "invalid event type '{value}'; expected one of: output|error|thinking|tool_call|tool_result|artifact|metadata|finished"
        ))),
    }
}
