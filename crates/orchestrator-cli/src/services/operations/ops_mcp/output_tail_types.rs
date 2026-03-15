use crate::invalid_input_error;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputTailEventType {
    Output,
    Error,
    Thinking,
}

impl OutputTailEventType {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Output => "output",
            Self::Error => "error",
            Self::Thinking => "thinking",
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
        _ => Err(invalid_input_error(format!("invalid event type '{value}'; expected one of: output|error|thinking"))),
    }
}
