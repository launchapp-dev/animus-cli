use super::output_tail_types::{OutputTailEventRecord, OutputTailEventType};
use crate::event_matches_run;
use anyhow::{Context, Result};
use protocol::{AgentRunEvent, OutputStreamType, RunId};
use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

pub(super) fn read_output_tail_events(
    events_path: &Path,
    run_id: &RunId,
    event_types: &[OutputTailEventType],
    limit: usize,
) -> Result<Vec<OutputTailEventRecord>> {
    let file = match fs::File::open(events_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read events log {}", events_path.display()));
        }
    };

    let mut reader = BufReader::new(file);
    let mut line_buffer = Vec::new();
    let mut tail = VecDeque::new();
    loop {
        line_buffer.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut line_buffer)
            .with_context(|| format!("failed to read events log {}", events_path.display()))?;
        if bytes_read == 0 {
            break;
        }

        let line = String::from_utf8_lossy(&line_buffer);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AgentRunEvent>(line) else {
            continue;
        };
        if !event_matches_run(&event, run_id) {
            continue;
        }
        let Some(record) = normalize_tail_event(event, event_types) else {
            continue;
        };
        if tail.len() == limit {
            let _ = tail.pop_front();
        }
        tail.push_back(record);
    }
    Ok(tail.into_iter().collect())
}

fn output_stream_type_label(stream_type: OutputStreamType) -> &'static str {
    match stream_type {
        OutputStreamType::Stdout => "stdout",
        OutputStreamType::Stderr => "stderr",
        OutputStreamType::System => "system",
    }
}

fn normalize_tail_event(event: AgentRunEvent, event_types: &[OutputTailEventType]) -> Option<OutputTailEventRecord> {
    match event {
        AgentRunEvent::OutputChunk { run_id, stream_type, text } => {
            if !event_types.contains(&OutputTailEventType::Output) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::Output.as_str().to_string(),
                run_id: run_id.0,
                text,
                source_kind: "output_chunk".to_string(),
                stream_type: Some(output_stream_type_label(stream_type).to_string()),
                data: None,
            })
        }
        AgentRunEvent::Error { run_id, error } => {
            if !event_types.contains(&OutputTailEventType::Error) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::Error.as_str().to_string(),
                run_id: run_id.0,
                text: error,
                source_kind: "error".to_string(),
                stream_type: None,
                data: None,
            })
        }
        AgentRunEvent::Thinking { run_id, content } => {
            if !event_types.contains(&OutputTailEventType::Thinking) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::Thinking.as_str().to_string(),
                run_id: run_id.0,
                text: content,
                source_kind: "thinking".to_string(),
                stream_type: None,
                data: None,
            })
        }
        AgentRunEvent::ToolCall { run_id, tool_info } => {
            if !event_types.contains(&OutputTailEventType::ToolCall) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::ToolCall.as_str().to_string(),
                run_id: run_id.0,
                text: tool_info.tool_name.clone(),
                source_kind: "tool_call".to_string(),
                stream_type: None,
                data: Some(serde_json::json!(tool_info)),
            })
        }
        AgentRunEvent::ToolResult { run_id, result_info } => {
            if !event_types.contains(&OutputTailEventType::ToolResult) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::ToolResult.as_str().to_string(),
                run_id: run_id.0,
                text: result_info.tool_name.clone(),
                source_kind: "tool_result".to_string(),
                stream_type: None,
                data: Some(serde_json::json!(result_info)),
            })
        }
        AgentRunEvent::Artifact { run_id, artifact_info } => {
            if !event_types.contains(&OutputTailEventType::Artifact) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::Artifact.as_str().to_string(),
                run_id: run_id.0,
                text: artifact_info.artifact_id.clone(),
                source_kind: "artifact".to_string(),
                stream_type: None,
                data: Some(serde_json::json!(artifact_info)),
            })
        }
        AgentRunEvent::Metadata { run_id, cost, tokens, data } => {
            if !event_types.contains(&OutputTailEventType::Metadata) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::Metadata.as_str().to_string(),
                run_id: run_id.0,
                text: "metadata".to_string(),
                source_kind: "metadata".to_string(),
                stream_type: None,
                data: Some(serde_json::json!({
                    "cost": cost,
                    "tokens": tokens,
                    "data": data,
                })),
            })
        }
        AgentRunEvent::Finished { run_id, exit_code, duration_ms } => {
            if !event_types.contains(&OutputTailEventType::Finished) {
                return None;
            }
            Some(OutputTailEventRecord {
                event_type: OutputTailEventType::Finished.as_str().to_string(),
                run_id: run_id.0,
                text: format!("exit={}", exit_code.unwrap_or_default()),
                source_kind: "finished".to_string(),
                stream_type: None,
                data: Some(serde_json::json!({
                    "exit_code": exit_code,
                    "duration_ms": duration_ms,
                })),
            })
        }
        AgentRunEvent::Started { .. } => None,
    }
}
