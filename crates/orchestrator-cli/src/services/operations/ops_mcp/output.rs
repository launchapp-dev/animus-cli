use super::{
    daemon::resolve_daemon_events_project_root,
    normalize_non_empty,
    output_tail_events::read_output_tail_events,
    output_tail_resolution::resolve_output_tail_resolution,
    output_tail_types::{parse_output_tail_event_type, OutputTailEventType},
    OutputTailInput, DEFAULT_OUTPUT_TAIL_LIMIT, MAX_OUTPUT_TAIL_LIMIT, OUTPUT_TAIL_SCHEMA,
};
use crate::invalid_input_error;
use anyhow::Result;
use protocol::RunId;
use serde_json::{json, Value};

pub(super) fn build_output_tail_result(default_project_root: &str, input: OutputTailInput) -> Result<Value> {
    let project_root = resolve_daemon_events_project_root(default_project_root, input.project_root);
    let run_id = normalize_non_empty(input.run_id);
    let task_id = normalize_non_empty(input.task_id);
    let event_types = parse_output_tail_event_types(input.event_types)?;
    let limit = output_tail_limit(input.limit);
    let resolved = resolve_output_tail_resolution(project_root.as_str(), run_id, task_id)?;
    let resolved_run_id = RunId(resolved.run_id.clone());
    let events_path = resolved.run_dir.join("events.jsonl");
    let events = read_output_tail_events(&events_path, &resolved_run_id, &event_types, limit)?;

    Ok(json!({
        "schema": OUTPUT_TAIL_SCHEMA,
        "resolved_run_id": resolved_run_id.0,
        "resolved_from": resolved.resolved_from,
        "events_path": events_path.display().to_string(),
        "limit": limit,
        "event_types": event_types
            .iter()
            .map(|event_type| event_type.as_str())
            .collect::<Vec<_>>(),
        "count": events.len(),
        "events": events,
    }))
}

fn output_tail_limit(limit: Option<usize>) -> usize {
    let normalized = limit.unwrap_or(DEFAULT_OUTPUT_TAIL_LIMIT).max(1);
    normalized.min(MAX_OUTPUT_TAIL_LIMIT)
}

fn parse_output_tail_event_types(raw: Option<Vec<String>>) -> Result<Vec<OutputTailEventType>> {
    let values = match raw {
        Some(values) if values.is_empty() => {
            return Err(invalid_input_error(
                "event_types must include at least one of: output|error|thinking|tool_call|tool_result|artifact|metadata|finished",
            ));
        }
        Some(values) => values,
        None => {
            return Ok(vec![OutputTailEventType::Output, OutputTailEventType::Thinking]);
        }
    };

    let mut parsed = Vec::new();
    for value in values {
        let event_type = parse_output_tail_event_type(value.as_str())?;
        if !parsed.contains(&event_type) {
            parsed.push(event_type);
        }
    }
    Ok(parsed)
}
