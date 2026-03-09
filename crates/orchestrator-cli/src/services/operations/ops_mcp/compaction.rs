use serde_json::Value;

pub(super) fn compact_json_text(raw: String) -> String {
    compact_json_str(raw.as_str()).unwrap_or(raw)
}

pub(super) fn compact_json_str(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let parsed = serde_json::from_str::<Value>(trimmed).ok()?;
    let compact = serde_json::to_string(&parsed).ok()?;
    (compact.len() < raw.len()).then_some(compact)
}
