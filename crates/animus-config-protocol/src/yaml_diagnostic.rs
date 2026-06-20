use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Rustc-style diagnostic for YAML workflow config parse / shape errors.
///
/// Renders as:
///
/// ```text
/// error: <message>
///   --> <file>:<line>:<col>
///    |
/// NN |     <line of source>
///    |     ^^^^^^^^^^ expected:
///    |                 - <shape A>
///    |                 - <shape B>
///    = help: did you mean `<suggestion>`?
///    = note: <note>
/// ```
#[derive(Debug, Clone, Serialize)]
pub struct YamlDiagnostic {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<usize>,
    /// Stable diagnostic code, e.g. `yaml.invalid_worktree`.
    pub code: String,
    /// One-line human message.
    pub message: String,
    /// Valid shapes the parser accepts at this position.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected: Vec<String>,
    /// Optional "did you mean ...?" suggestion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Optional extra note (e.g. version migration hint).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Source excerpt rendered alongside the carets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<YamlExcerpt>,
}

#[derive(Debug, Clone, Serialize)]
pub struct YamlExcerpt {
    pub start_line: usize,
    pub lines: Vec<String>,
    /// 0-based column range to underline on the focal line.
    pub underline: (usize, usize),
    /// Index into `lines` of the focal line (0-based).
    pub focal: usize,
}

impl YamlDiagnostic {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            file: None,
            line: None,
            col: None,
            code: code.into(),
            message: message.into(),
            expected: Vec::new(),
            suggestion: None,
            note: None,
            excerpt: None,
        }
    }

    pub fn with_file(mut self, file: impl Into<PathBuf>) -> Self {
        self.file = Some(file.into());
        self
    }

    pub fn with_location(mut self, line: usize, col: usize) -> Self {
        self.line = Some(line);
        self.col = Some(col);
        self
    }

    pub fn with_expected<I, S>(mut self, expected: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.expected = expected.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_suggestion(mut self, s: impl Into<String>) -> Self {
        self.suggestion = Some(s.into());
        self
    }

    pub fn with_note(mut self, s: impl Into<String>) -> Self {
        self.note = Some(s.into());
        self
    }

    /// Attach a source excerpt extracted from `yaml_str`. `line` is 1-based.
    /// `col_start` and `col_end` are 1-based column positions on `line` to
    /// underline (inclusive..exclusive). If `col_end <= col_start`, the
    /// underline is widened to cover the rest of the line.
    pub fn with_excerpt_from(mut self, yaml_str: &str, line: usize, col_start: usize, col_end: usize) -> Self {
        if line == 0 {
            return self;
        }
        let all_lines: Vec<&str> = yaml_str.lines().collect();
        if all_lines.is_empty() {
            return self;
        }
        let focal_idx = line.saturating_sub(1).min(all_lines.len() - 1);
        let start_idx = focal_idx.saturating_sub(1);
        let end_idx = (focal_idx + 1).min(all_lines.len() - 1);
        let lines: Vec<String> = all_lines[start_idx..=end_idx].iter().map(|s| s.to_string()).collect();
        let focal = focal_idx - start_idx;
        let focal_len = all_lines[focal_idx].chars().count();
        let cs = col_start.saturating_sub(1).min(focal_len);
        let ce = if col_end > col_start { col_end.saturating_sub(1).min(focal_len) } else { focal_len };
        let ce = ce.max(cs + 1).min(focal_len.max(cs + 1));
        self.excerpt = Some(YamlExcerpt { start_line: start_idx + 1, lines, underline: (cs, ce), focal });
        self
    }
}

impl fmt::Display for YamlDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "error: {}", self.message)?;
        if let Some(file) = &self.file {
            match (self.line, self.col) {
                (Some(l), Some(c)) => writeln!(f, "  --> {}:{}:{}", file.display(), l, c)?,
                (Some(l), None) => writeln!(f, "  --> {}:{}", file.display(), l)?,
                _ => writeln!(f, "  --> {}", file.display())?,
            }
        }
        let gutter_width = match (&self.excerpt, self.line) {
            (Some(e), _) => format!("{}", e.start_line + e.lines.len().saturating_sub(1)).len(),
            (None, Some(l)) => format!("{}", l).len(),
            (None, None) => 1,
        };
        let pad = " ".repeat(gutter_width);

        if let Some(excerpt) = &self.excerpt {
            writeln!(f, "{} |", pad)?;
            for (offset, line) in excerpt.lines.iter().enumerate() {
                let lineno = excerpt.start_line + offset;
                let num = format!("{:>width$}", lineno, width = gutter_width);
                writeln!(f, "{} | {}", num, line)?;
                if offset == excerpt.focal {
                    let (cs, ce) = excerpt.underline;
                    let leading: String = line.chars().take(cs).map(|c| if c == '\t' { '\t' } else { ' ' }).collect();
                    let span = ce.saturating_sub(cs).max(1);
                    let carets: String = "^".repeat(span);
                    if self.expected.is_empty() {
                        writeln!(f, "{} | {}{}", pad, leading, carets)?;
                    } else {
                        let mut iter = self.expected.iter();
                        let first = iter.next().unwrap();
                        writeln!(f, "{} | {}{} expected one of:", pad, leading, carets)?;
                        let indent: String = " ".repeat(cs + span + 1);
                        writeln!(f, "{} | {}- {}", pad, indent, first)?;
                        for ex in iter {
                            writeln!(f, "{} | {}- {}", pad, indent, ex)?;
                        }
                    }
                }
            }
        } else if !self.expected.is_empty() {
            writeln!(f, "{} = expected one of:", pad)?;
            for ex in &self.expected {
                writeln!(f, "{}     - {}", pad, ex)?;
            }
        }

        if let Some(s) = &self.suggestion {
            writeln!(f, "{} = help: did you mean `{}`?", pad, s)?;
        }
        if let Some(n) = &self.note {
            writeln!(f, "{} = note: {}", pad, n)?;
        }
        Ok(())
    }
}

impl std::error::Error for YamlDiagnostic {}

/// Best-effort Levenshtein distance for short strings. Returns
/// `usize::MAX` if either input is empty.
pub fn edit_distance(a: &str, b: &str) -> usize {
    if a.is_empty() || b.is_empty() {
        return usize::MAX;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let n = a.len();
    let m = b.len();
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr: Vec<usize> = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1].eq_ignore_ascii_case(&b[j - 1]) { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// Choose the closest candidate to `input` within `max_distance` (Levenshtein).
/// Returns `None` if no candidate is within range.
pub fn closest_match<'a>(input: &str, candidates: &[&'a str], max_distance: usize) -> Option<&'a str> {
    let mut best: Option<(&'a str, usize)> = None;
    for cand in candidates {
        let d = edit_distance(input, cand);
        if d <= max_distance {
            match best {
                Some((_, bd)) if d >= bd => {}
                _ => best = Some((*cand, d)),
            }
        }
    }
    best.map(|(s, _)| s)
}

/// Wrap a raw `serde_yaml::Error` into a `YamlDiagnostic`, attaching the
/// file path and a source excerpt when location info is available.
pub fn wrap_serde_yaml_error(err: &serde_yaml::Error, yaml_str: &str, source_path: Option<&Path>) -> YamlDiagnostic {
    let mut diag = YamlDiagnostic::new("yaml.parse_failed", err.to_string());
    if let Some(path) = source_path {
        diag = diag.with_file(path.to_path_buf());
    }
    if let Some(loc) = err.location() {
        let line = loc.line();
        let col = loc.column();
        let trimmed_msg = strip_trailing_location(&diag.message);
        diag.message = trimmed_msg;
        diag = diag.with_location(line, col).with_excerpt_from(yaml_str, line, col, col + 1);
    }
    diag
}

/// serde_yaml appends ` at line N column M` to the error message string.
/// Strip it since the location is rendered separately in the rustc-style header.
fn strip_trailing_location(msg: &str) -> String {
    if let Some(idx) = msg.rfind(" at line ") {
        let tail = &msg[idx + " at line ".len()..];
        if tail.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return msg[..idx].to_string();
        }
    }
    msg.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_basic() {
        assert_eq!(edit_distance("phases", "phasess"), 1);
        assert_eq!(edit_distance("agent", "agnet"), 2);
        assert_eq!(edit_distance("auto", "optional"), 6);
        assert_eq!(edit_distance("yes", "true"), 4);
    }

    #[test]
    fn closest_match_picks_within_threshold() {
        let cands = ["auto", "required", "skip"];
        assert_eq!(closest_match("skp", &cands, 2), Some("skip"));
        assert_eq!(closest_match("aut", &cands, 2), Some("auto"));
        assert_eq!(closest_match("xxxxxxxx", &cands, 2), None);
    }

    #[test]
    fn diagnostic_renders_rustc_style() {
        let yaml = "phases:\n  build:\n    worktree: no\n";
        let diag = YamlDiagnostic::new("yaml.invalid_worktree", "invalid `worktree:` value")
            .with_file("/tmp/x.yaml")
            .with_location(3, 5)
            .with_expected(vec!["string: \"auto\" | \"required\" | \"skip\"", "boolean: true | false"])
            .with_suggestion("false")
            .with_excerpt_from(yaml, 3, 5, 19);
        let rendered = format!("{}", diag);
        assert!(rendered.contains("error: invalid `worktree:` value"));
        assert!(rendered.contains("--> /tmp/x.yaml:3:5"));
        assert!(rendered.contains("worktree: no"));
        assert!(rendered.contains("^^^"));
        assert!(rendered.contains("expected one of:"));
        assert!(rendered.contains("did you mean `false`"));
    }
}
