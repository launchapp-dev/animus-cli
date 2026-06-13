//! Minimal left-aligned column table renderer shared by the human-mode
//! `list` / `inspect` output paths. Matches the visual style of
//! `animus plugin list` (uppercase headers, two-space gutters, the last
//! column un-padded).

/// Render a table to stdout: an uppercase header row followed by one line
/// per row. Each column is padded to the widest cell (header included);
/// the final column is left un-padded to avoid trailing whitespace.
pub(crate) fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let col_count = headers.len();
    if col_count == 0 {
        return;
    }
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(col_count) {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }
    println!("{}", render_row(&headers.iter().map(|h| h.to_string()).collect::<Vec<_>>(), &widths));
    for row in rows {
        println!("{}", render_row(row, &widths));
    }
}

fn render_row(cells: &[String], widths: &[usize]) -> String {
    let last = widths.len().saturating_sub(1);
    let mut out = String::new();
    for (i, width) in widths.iter().enumerate() {
        let empty = String::new();
        let cell = cells.get(i).unwrap_or(&empty);
        if i == last {
            out.push_str(cell);
        } else {
            out.push_str(&format!("{cell:<width$}  "));
        }
    }
    out
}

/// Render an RFC 3339 timestamp as a compact relative age (e.g. `5m`, `3h`,
/// `2d`). Falls back to the raw string when it cannot be parsed.
pub(crate) fn format_age(rfc3339: &str) -> String {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return rfc3339.to_string();
    };
    let secs = (chrono::Utc::now() - parsed.with_timezone(&chrono::Utc)).num_seconds();
    if secs < 0 {
        return "0s".to_string();
    }
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_row_pads_all_but_last_column() {
        let widths = [4, 6];
        let row = render_row(&["ab".to_string(), "cd".to_string()], &widths);
        assert_eq!(row, "ab    cd");
    }

    #[test]
    fn render_row_tolerates_missing_trailing_cells() {
        let widths = [3, 3];
        let row = render_row(&["x".to_string()], &widths);
        assert_eq!(row, "x    ");
    }

    #[test]
    fn format_age_returns_raw_string_on_parse_failure() {
        assert_eq!(format_age("not-a-timestamp"), "not-a-timestamp");
    }

    #[test]
    fn format_age_buckets_by_unit() {
        let now = chrono::Utc::now();
        let ten_min_ago = (now - chrono::Duration::minutes(10)).to_rfc3339();
        assert_eq!(format_age(&ten_min_ago), "10m");
        let three_hours_ago = (now - chrono::Duration::hours(3)).to_rfc3339();
        assert_eq!(format_age(&three_hours_ago), "3h");
    }
}
