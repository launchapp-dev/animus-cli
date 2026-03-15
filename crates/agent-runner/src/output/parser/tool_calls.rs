use serde_json::Value;

pub(super) fn parse_json_tool_call(line: &str) -> Option<(String, Value)> {
    let value = extract_json_object(line)?;

    if let Some(signal) = parse_phase_transition_signal(&value) {
        return Some(signal);
    }

    if let Some(call) = parse_json_tool_call_value(&value) {
        return Some(call);
    }

    if let Some(item) = value.get("item") {
        if let Some(call) = parse_json_tool_call_value(item) {
            return Some(call);
        }
        if let Some(tool_call) = item.get("tool_call") {
            if let Some(call) = parse_json_tool_call_value(tool_call) {
                return Some(call);
            }
        }
        if let Some(function_call) = item.get("function_call") {
            if let Some(call) = parse_json_tool_call_value(function_call) {
                return Some(call);
            }
        }
        if let Some(tool_calls) = item.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                if let Some(call) = parse_json_tool_call_value(tool_call) {
                    return Some(call);
                }
            }
        }
    }

    if let Some(tool_call) = value.get("tool_call") {
        if let Some(call) = parse_json_tool_call_value(tool_call) {
            return Some(call);
        }
    }

    if let Some(function_call) = value.get("function_call") {
        if let Some(call) = parse_json_tool_call_value(function_call) {
            return Some(call);
        }
    }

    if let Some(tool_calls) = value.get("tool_calls").and_then(Value::as_array) {
        for item in tool_calls {
            if let Some(call) = parse_json_tool_call_value(item) {
                return Some(call);
            }
        }
    }

    if let Some(content) = value.get("content").and_then(Value::as_array) {
        for item in content {
            if let Some(call) = parse_json_tool_call_value(item) {
                return Some(call);
            }
            if let Some(tool_call) = item.get("tool_call") {
                if let Some(call) = parse_json_tool_call_value(tool_call) {
                    return Some(call);
                }
            }
            if let Some(function_call) = item.get("function_call") {
                if let Some(call) = parse_json_tool_call_value(function_call) {
                    return Some(call);
                }
            }
        }
    }

    if let Some(content) = value.pointer("/message/content").and_then(Value::as_array) {
        for item in content {
            if let Some(call) = parse_json_tool_call_value(item) {
                return Some(call);
            }
            if let Some(tool_call) = item.get("tool_call") {
                if let Some(call) = parse_json_tool_call_value(tool_call) {
                    return Some(call);
                }
            }
            if let Some(function_call) = item.get("function_call") {
                if let Some(call) = parse_json_tool_call_value(function_call) {
                    return Some(call);
                }
            }
        }
    }

    None
}

fn parse_phase_transition_signal(value: &Value) -> Option<(String, Value)> {
    let event_type = value
        .get("type")
        .or_else(|| value.get("event"))
        .or_else(|| value.get("kind"))
        .and_then(Value::as_str)
        .map(normalize_token);

    if !matches!(event_type.as_deref(), Some("phase_transition" | "phase-transition")) {
        return None;
    }

    let target_phase = value.get("target_phase")?.clone();
    let target_phase_text = target_phase.as_str().map(str::trim).unwrap_or_default();
    if is_placeholder_phase_transition_token(target_phase_text) {
        return None;
    }

    let mut params = serde_json::Map::new();
    params.insert("target_phase".to_string(), target_phase);
    if let Some(reason) = value.get("reason") {
        let include_reason = reason.as_str().map(|text| !is_placeholder_phase_transition_token(text)).unwrap_or(true);
        if include_reason {
            params.insert("reason".to_string(), reason.clone());
        }
    }

    Some(("phase_transition".to_string(), Value::Object(params)))
}

fn parse_json_tool_call_value(value: &Value) -> Option<(String, Value)> {
    let event_type = value
        .get("type")
        .or_else(|| value.get("event"))
        .or_else(|| value.get("kind"))
        .and_then(Value::as_str)
        .map(normalize_token);

    let tool_name = value
        .get("tool_name")
        .and_then(Value::as_str)
        .or_else(|| value.get("tool").and_then(Value::as_str))
        .or_else(|| value.get("name").and_then(Value::as_str))
        .or_else(|| {
            value.get("function").and_then(Value::as_object).and_then(|obj| obj.get("name")).and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .get("function_call")
                .and_then(Value::as_object)
                .and_then(|obj| obj.get("name"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|name| !name.is_empty())?
        .to_string();

    let normalized_event = event_type.as_deref().unwrap_or_default();
    let looks_like_tool_call = matches!(
        normalized_event,
        "" | "tool_call"
            | "tool_use"
            | "function_call"
            | "mcp_tool_call"
            | "mcp_call"
            | "tool-call"
            | "tool-use"
            | "function-call"
    );

    if !looks_like_tool_call {
        return None;
    }

    let mut parameters = value
        .get("parameters")
        .or_else(|| value.get("arguments"))
        .or_else(|| value.get("args"))
        .or_else(|| value.get("input"))
        .or_else(|| value.get("tool_input"))
        .or_else(|| value.pointer("/function/arguments"))
        .or_else(|| value.pointer("/function_call/arguments"))
        .cloned()
        .map(normalize_arguments_value)
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    if let Some(server_name) =
        value.get("server").and_then(Value::as_str).map(str::trim).filter(|server| !server.is_empty())
    {
        if let Some(object) = parameters.as_object_mut() {
            object.insert("server".to_string(), Value::String(server_name.to_string()));
        }
    }

    if matches!(normalize_token(&tool_name).as_str(), "phase_transition" | "phase-transition") {
        let target_phase_text =
            parameters.get("target_phase").and_then(Value::as_str).map(str::trim).unwrap_or_default();

        if is_placeholder_phase_transition_token(target_phase_text) {
            return None;
        }

        let strip_reason = parameters
            .get("reason")
            .and_then(Value::as_str)
            .map(is_placeholder_phase_transition_token)
            .unwrap_or(false);
        if strip_reason {
            if let Some(obj) = parameters.as_object_mut() {
                obj.remove("reason");
            }
        }
    }

    Some((tool_name, parameters))
}

fn is_placeholder_phase_transition_token(value: &str) -> bool {
    let normalized = normalize_token(value);
    if normalized.is_empty() {
        return true;
    }

    matches!(
        normalized.as_str(),
        "<phase-id>"
            | "<why>"
            | "phase-id"
            | "why"
            | "valid_phase_id"
            | "valid-phase-id"
            | "target_phase"
            | "target-phase"
            | "reason"
            | "short plain-text reason"
            | "short plain text reason"
            | "plain-text reason"
            | "plain text reason"
    ) || normalized.starts_with('<')
        || normalized.ends_with('>')
}

fn normalize_arguments_value(value: Value) -> Value {
    match value {
        Value::String(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        other => other,
    }
}

pub(super) fn parse_xml_tool_parameters(content: &str) -> Option<Value> {
    let mut params = serde_json::Map::new();
    let mut search_from = 0usize;

    while let Some(relative_start) = content[search_from..].find("<parameter") {
        let start = search_from + relative_start;
        let slice = &content[start..];
        let tag_end = slice.find('>')?;
        let tag = &slice[..=tag_end];
        let name = extract_attr_value(tag, "<parameter", "name")?;

        let value_start = start + tag_end + 1;
        let value_slice = &content[value_start..];
        let value_end = value_slice.find("</parameter>")?;

        let raw_value = value_slice[..value_end].trim();
        let normalized = normalize_arguments_value(Value::String(raw_value.trim_matches('"').to_string()));

        params.insert(name, normalized);
        search_from = value_start + value_end + "</parameter>".len();
    }

    if params.is_empty() {
        None
    } else {
        Some(Value::Object(params))
    }
}

fn extract_json_object(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return serde_json::from_str(trimmed).ok();
    }

    for (start_idx, ch) in trimmed.char_indices() {
        if ch != '{' {
            continue;
        }

        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;

        for (offset, current) in trimmed[start_idx..].char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                    continue;
                }
                if current == '\\' {
                    escaped = true;
                    continue;
                }
                if current == '"' {
                    in_string = false;
                }
                continue;
            }

            match current {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    if depth == 0 {
                        let end_idx = start_idx + offset;
                        let candidate = &trimmed[start_idx..=end_idx];
                        if let Ok(value) = serde_json::from_str::<Value>(candidate) {
                            return Some(value);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    None
}

fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub(super) fn extract_tool_name(line: &str) -> Option<String> {
    if let Some(name) = extract_attr_value(line, "<invoke ", "name") {
        return Some(name);
    }

    if let Some(name) = extract_attr_value(line, "<tool_use", "name") {
        return Some(name);
    }

    if let Some(name) = extract_attr_value(line, "<function ", "name") {
        return Some(name);
    }

    if let Some(tool) = line.split("tool:").nth(1) {
        return Some(tool.split_whitespace().next()?.to_string());
    }

    None
}

fn extract_attr_value(line: &str, tag_prefix: &str, attr: &str) -> Option<String> {
    let tag_start = line.find(tag_prefix)?;
    let tag = &line[tag_start..];
    let key = format!("{attr}=\"");
    let key_start = tag.find(&key)?;
    let value_start = key_start + key.len();
    let rest = &tag[value_start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
