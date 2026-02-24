use serde_json::Value;

use super::types::VisionRefinementProposal;
use crate::collect_json_payload_lines;

fn parse_vision_refinement_proposal(value: &Value) -> Option<VisionRefinementProposal> {
    let proposal = serde_json::from_value::<VisionRefinementProposal>(value.clone()).ok()?;
    proposal.has_any_content().then_some(proposal)
}

fn extract_code_fence_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("```") {
        let after_start = &remaining[start + 3..];
        let Some(end) = after_start.find("```") else {
            break;
        };
        let block = &after_start[..end];
        let block = if let Some(newline) = block.find('\n') {
            let (header, body) = block.split_at(newline);
            let header = header.trim();
            if header.is_empty()
                || header
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == ' ')
            {
                body
            } else {
                block
            }
        } else {
            block
        };
        let trimmed = block.trim();
        if !trimmed.is_empty() {
            candidates.push(trimmed.to_string());
        }
        remaining = &after_start[end + 3..];
    }
    candidates
}

fn extract_balanced_json_candidates(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut candidates = Vec::new();
    let mut index = 0usize;

    while index < bytes.len() {
        let start_ch = bytes[index] as char;
        if start_ch != '{' && start_ch != '[' {
            index = index.saturating_add(1);
            continue;
        }

        let mut stack = vec![start_ch];
        let mut in_string = false;
        let mut escaped = false;
        let mut end_index = None;
        let mut cursor = index.saturating_add(1);
        while cursor < bytes.len() {
            let ch = bytes[cursor] as char;
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                cursor = cursor.saturating_add(1);
                continue;
            }

            match ch {
                '"' => in_string = true,
                '{' | '[' => stack.push(ch),
                '}' => {
                    if stack.pop() != Some('{') {
                        break;
                    }
                }
                ']' => {
                    if stack.pop() != Some('[') {
                        break;
                    }
                }
                _ => {}
            }

            if stack.is_empty() {
                end_index = Some(cursor);
                break;
            }
            cursor = cursor.saturating_add(1);
        }

        if let Some(end) = end_index {
            let segment = text[index..=end].trim();
            if !segment.is_empty() {
                candidates.push(segment.to_string());
            }
            index = end.saturating_add(1);
        } else {
            index = index.saturating_add(1);
        }
    }

    candidates
}

fn parse_vision_refinement_from_json_text(text: &str) -> Option<VisionRefinementProposal> {
    let value = serde_json::from_str::<Value>(text.trim()).ok()?;
    parse_vision_refinement_from_payload(&value)
}

pub(super) fn parse_vision_refinement_from_payload(
    payload: &Value,
) -> Option<VisionRefinementProposal> {
    if let Some(proposal) = parse_vision_refinement_proposal(payload) {
        return Some(proposal);
    }

    match payload {
        Value::Array(items) => {
            for item in items {
                if let Some(proposal) = parse_vision_refinement_from_payload(item) {
                    return Some(proposal);
                }
            }
        }
        Value::Object(object) => {
            for key in [
                "proposal",
                "refinement",
                "vision_refinement",
                "data",
                "payload",
                "result",
                "output",
                "item",
            ] {
                if let Some(value) = object.get(key) {
                    if let Some(proposal) = parse_vision_refinement_from_payload(value) {
                        return Some(proposal);
                    }
                }
            }

            for key in ["text", "message", "content", "output_text", "delta"] {
                if let Some(raw) = object.get(key).and_then(Value::as_str) {
                    if let Some(proposal) = parse_vision_refinement_from_json_text(raw) {
                        return Some(proposal);
                    }
                }
            }
        }
        _ => {}
    }

    None
}

pub(super) fn parse_vision_refinement_from_text(text: &str) -> Option<VisionRefinementProposal> {
    for (_raw, payload) in collect_json_payload_lines(text) {
        if let Some(proposal) = parse_vision_refinement_from_payload(&payload) {
            return Some(proposal);
        }
    }

    for block in extract_code_fence_candidates(text) {
        if let Some(proposal) = parse_vision_refinement_from_json_text(&block) {
            return Some(proposal);
        }
    }

    for candidate in extract_balanced_json_candidates(text) {
        if let Some(proposal) = parse_vision_refinement_from_json_text(&candidate) {
            return Some(proposal);
        }
    }

    parse_vision_refinement_from_json_text(text)
}
