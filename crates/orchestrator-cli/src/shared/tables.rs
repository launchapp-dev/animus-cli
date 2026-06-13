use serde_json::Value;

/// Render a simple fixed-width column table to stdout. Each header is paired
/// with one cell per row; columns are left-padded to the widest cell (or the
/// header width, whichever is larger). The final column is not padded so long
/// trailing values (titles, paths) don't trail whitespace.
pub(crate) fn render_table(headers: &[&str], rows: &[Vec<String>]) {
    if headers.is_empty() {
        return;
    }
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(cols) {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }
    println!("{}", format_row(headers.iter().map(|h| h.to_string()).collect::<Vec<_>>(), &widths));
    for row in rows {
        println!("{}", format_row(row.clone(), &widths));
    }
}

fn format_row(cells: Vec<String>, widths: &[usize]) -> String {
    let last = widths.len().saturating_sub(1);
    let mut out = String::new();
    for (i, width) in widths.iter().enumerate() {
        let cell = cells.get(i).map(String::as_str).unwrap_or("");
        if i == last {
            out.push_str(cell);
        } else {
            out.push_str(&format!("{cell:<width$}  "));
        }
    }
    out
}

/// Format a priority value as a `p0`..`p4` bucket for human display.
///
/// Accepts both the numeric form backends store (the subject protocol uses a
/// `0..=4` scale: 0 = none, 1 = low, 2 = medium, 3 = high, 4 = critical) and a
/// textual form already in bucket vocabulary (`p1`, `high`, `critical`, ...).
/// Numeric `0..=4` map to `p0..p4`; recognized words map to the same buckets;
/// anything else falls back to the trimmed string form (or `--` when absent).
pub(crate) fn format_priority(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "--".to_string(),
        Some(Value::Number(n)) => n.as_i64().map(priority_bucket_from_num).unwrap_or_else(|| n.to_string()),
        Some(Value::String(s)) => format_priority_str(s),
        Some(other) => other.to_string(),
    }
}

fn format_priority_str(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "--".to_string();
    }
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "p0" | "0" | "none" => "p0".to_string(),
        "p1" | "1" | "low" => "p1".to_string(),
        "p2" | "2" | "medium" | "normal" => "p2".to_string(),
        "p3" | "3" | "high" => "p3".to_string(),
        "p4" | "4" | "critical" | "urgent" => "p4".to_string(),
        _ => trimmed.to_string(),
    }
}

fn priority_bucket_from_num(n: i64) -> String {
    if (0..=4).contains(&n) {
        format!("p{n}")
    } else {
        n.to_string()
    }
}

/// Normalize a subject identifier to the backend-qualified `<kind>:<native>`
/// form. Bare ids gain the `<kind>:` prefix; already-qualified ids
/// (containing a `:`) pass through unchanged. Empty input is returned as-is so
/// callers keep their own emptiness validation.
pub(crate) fn qualify_subject_id(id: &str, kind: &str) -> String {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.contains(':') {
        return trimmed.to_string();
    }
    format!("{}:{trimmed}", kind.trim())
}

/// Strip a leading `<kind>:` qualifier from a subject id, returning the bare
/// native id. Ids without a `:` pass through unchanged. Used where the
/// downstream surface (task store, workflow run) keys on bare ids.
pub(crate) fn bare_subject_id(id: &str) -> String {
    let trimmed = id.trim();
    match trimmed.split_once(':') {
        Some((_prefix, native)) if !native.is_empty() => native.to_string(),
        _ => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_priority_maps_numbers_and_words_to_buckets() {
        assert_eq!(format_priority(Some(&json!(0))), "p0");
        assert_eq!(format_priority(Some(&json!(2))), "p2");
        assert_eq!(format_priority(Some(&json!(4))), "p4");
        assert_eq!(format_priority(Some(&json!("p1"))), "p1");
        assert_eq!(format_priority(Some(&json!("high"))), "p3");
        assert_eq!(format_priority(Some(&json!("critical"))), "p4");
        assert_eq!(format_priority(Some(&json!("low"))), "p1");
    }

    #[test]
    fn format_priority_handles_missing_and_unknown() {
        assert_eq!(format_priority(None), "--");
        assert_eq!(format_priority(Some(&Value::Null)), "--");
        assert_eq!(format_priority(Some(&json!("  "))), "--");
        assert_eq!(format_priority(Some(&json!("blocker"))), "blocker");
        assert_eq!(format_priority(Some(&json!(9))), "9");
    }

    #[test]
    fn qualify_adds_prefix_only_when_bare() {
        assert_eq!(qualify_subject_id("TASK-001", "task"), "task:TASK-001");
        assert_eq!(qualify_subject_id("task:TASK-001", "task"), "task:TASK-001");
        assert_eq!(qualify_subject_id("linear:ENG-9", "task"), "linear:ENG-9");
        assert_eq!(qualify_subject_id("  TASK-2  ", "task"), "task:TASK-2");
        assert_eq!(qualify_subject_id("", "task"), "");
    }

    #[test]
    fn bare_strips_known_prefix() {
        assert_eq!(bare_subject_id("task:TASK-001"), "TASK-001");
        assert_eq!(bare_subject_id("TASK-001"), "TASK-001");
        assert_eq!(bare_subject_id("linear:ENG-9"), "ENG-9");
        assert_eq!(bare_subject_id("  task:TASK-2 "), "TASK-2");
    }

    #[test]
    fn render_table_pads_columns_to_widest_cell() {
        // Smoke test: ensure it doesn't panic on ragged rows.
        render_table(&["A", "BB"], &[vec!["x".to_string()], vec!["yyyy".to_string(), "z".to_string()]]);
    }
}
