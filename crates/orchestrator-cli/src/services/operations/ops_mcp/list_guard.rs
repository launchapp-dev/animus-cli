use anyhow::Result;
use serde_json::{json, Value};

use crate::invalid_input_error;

use super::{
    list_profiles::list_tool_profile, ListGuardInput, ListSizeGuardMode, ListSizeGuardResult,
    ListToolProfile, DEFAULT_MCP_LIST_LIMIT, DEFAULT_MCP_LIST_MAX_TOKENS, MAX_MCP_LIST_LIMIT,
    MAX_MCP_LIST_MAX_TOKENS, MCP_LIST_RESULT_SCHEMA, MIN_MCP_LIST_MAX_TOKENS,
};

pub(super) fn list_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(DEFAULT_MCP_LIST_LIMIT)
        .max(1)
        .min(MAX_MCP_LIST_LIMIT)
}

fn list_offset(offset: Option<usize>) -> usize {
    offset.unwrap_or(0)
}

pub(super) fn list_max_tokens(max_tokens: Option<usize>) -> usize {
    max_tokens
        .unwrap_or(DEFAULT_MCP_LIST_MAX_TOKENS)
        .max(MIN_MCP_LIST_MAX_TOKENS)
        .min(MAX_MCP_LIST_MAX_TOKENS)
}

fn estimate_json_tokens(value: &Value) -> usize {
    let char_count = serde_json::to_string(value)
        .map(|serialized| serialized.chars().count())
        .unwrap_or_default();
    ((char_count + 3) / 4).max(1)
}

pub(super) fn build_guarded_list_result(
    tool_name: &str,
    data: Value,
    guard: ListGuardInput,
) -> Result<Value> {
    let profile = list_tool_profile(tool_name).ok_or_else(|| {
        invalid_input_error(format!(
            "unsupported MCP list tool '{tool_name}' for paginated response"
        ))
    })?;
    let all_items = data.as_array().cloned().ok_or_else(|| {
        invalid_input_error(format!(
            "{tool_name} expected list data as JSON array but received {}",
            value_kind(&data)
        ))
    })?;

    let limit = list_limit(guard.limit);
    let offset = list_offset(guard.offset);
    let max_tokens = list_max_tokens(guard.max_tokens);

    let total = all_items.len();
    let start = offset.min(total);
    let page_items: Vec<Value> = all_items.into_iter().skip(start).take(limit).collect();
    let returned = page_items.len();
    let has_more = start.saturating_add(returned) < total;
    let next_offset = has_more.then_some(start.saturating_add(returned));
    let size_guard = apply_list_size_guard(page_items, profile, max_tokens);

    Ok(json!({
        "schema": MCP_LIST_RESULT_SCHEMA,
        "tool": tool_name,
        "items": size_guard.items,
        "pagination": {
            "limit": limit,
            "offset": start,
            "returned": returned,
            "total": total,
            "has_more": has_more,
            "next_offset": next_offset,
        },
        "size_guard": {
            "max_tokens_hint": max_tokens,
            "estimated_tokens": size_guard.estimated_tokens,
            "mode": size_guard.mode.as_str(),
            "truncated": size_guard.truncated,
        }
    }))
}

fn apply_list_size_guard(
    full_page_items: Vec<Value>,
    profile: ListToolProfile,
    max_tokens: usize,
) -> ListSizeGuardResult {
    let full_value = Value::Array(full_page_items.clone());
    let full_tokens = estimate_json_tokens(&full_value);
    if full_tokens <= max_tokens {
        return ListSizeGuardResult {
            items: full_page_items,
            estimated_tokens: full_tokens,
            mode: ListSizeGuardMode::Full,
            truncated: false,
        };
    }

    let summary_items: Vec<Value> = full_page_items
        .iter()
        .cloned()
        .map(|item| retain_fields(item, profile.summary_fields))
        .collect();
    let summary_value = Value::Array(summary_items.clone());
    let summary_tokens = estimate_json_tokens(&summary_value);
    if summary_tokens <= max_tokens {
        return ListSizeGuardResult {
            items: summary_items,
            estimated_tokens: summary_tokens,
            mode: ListSizeGuardMode::SummaryFields,
            truncated: true,
        };
    }

    let summary_only_item = build_summary_only_digest(&full_page_items, profile, max_tokens);
    let summary_only_items = vec![summary_only_item];
    let summary_only_tokens = estimate_json_tokens(&Value::Array(summary_only_items.clone()));
    ListSizeGuardResult {
        items: summary_only_items,
        estimated_tokens: summary_only_tokens,
        mode: ListSizeGuardMode::SummaryOnly,
        truncated: true,
    }
}

fn build_summary_only_digest(
    items: &[Value],
    profile: ListToolProfile,
    max_tokens: usize,
) -> Value {
    let mut ids = Vec::new();
    let mut status_counts = std::collections::BTreeMap::new();

    for item in items {
        if ids.len() < 10 {
            if let Some(raw_id) = find_text_field(item, profile.digest_id_fields) {
                ids.push(clamp_text(&raw_id, 64));
            }
        }
        if let Some(raw_status) = find_text_field(item, profile.digest_status_fields) {
            let status = clamp_text(&raw_status, 32);
            *status_counts.entry(status).or_insert(0usize) += 1;
        }
    }

    let mut status_entries: Vec<(String, usize)> = status_counts.into_iter().collect();
    let mut omitted_status_item_count = 0usize;

    loop {
        let digest = build_summary_only_digest_value(
            items.len(),
            &ids,
            &status_entries,
            omitted_status_item_count,
        );
        if estimate_json_tokens(&digest) <= max_tokens {
            return digest;
        }

        if let Some((_, count)) = status_entries.pop() {
            omitted_status_item_count = omitted_status_item_count.saturating_add(count);
            continue;
        }

        if ids.pop().is_some() {
            continue;
        }

        return digest;
    }
}

fn build_summary_only_digest_value(
    item_count: usize,
    ids: &[String],
    status_entries: &[(String, usize)],
    omitted_status_item_count: usize,
) -> Value {
    let mut status_counts = serde_json::Map::new();
    for (status, count) in status_entries {
        status_counts.insert(status.clone(), json!(*count));
    }

    let mut digest = serde_json::Map::new();
    digest.insert("kind".to_string(), json!("summary_only"));
    digest.insert("item_count".to_string(), json!(item_count));
    digest.insert("ids".to_string(), json!(ids));
    digest.insert("status_counts".to_string(), Value::Object(status_counts));
    if omitted_status_item_count > 0 {
        digest.insert(
            "omitted_status_item_count".to_string(),
            json!(omitted_status_item_count),
        );
    }
    Value::Object(digest)
}

fn find_text_field(value: &Value, fields: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    fields.iter().find_map(|field| {
        let raw = object.get(*field)?;
        match raw {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(boolean) => Some(boolean.to_string()),
            _ => None,
        }
    })
}

fn clamp_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let trimmed: String = value.chars().take(max_chars - 3).collect();
    format!("{trimmed}...")
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn retain_fields(value: Value, fields: &[&str]) -> Value {
    match value {
        Value::Object(map) => {
            let filtered: serde_json::Map<String, Value> = map
                .into_iter()
                .filter(|(key, _)| fields.contains(&key.as_str()))
                .collect();
            Value::Object(filtered)
        }
        other => other,
    }
}
