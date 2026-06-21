//! Shell-style environment variable interpolation for workflow YAML.
//!
//! Substitution happens against the raw file contents before YAML parsing so every
//! string field (subject configs, provider tokens, env override blocks, workflow
//! metadata, etc.) accepts the same syntax uniformly.
//!
//! Supported syntax (modeled after docker-compose / POSIX shell):
//!
//! | Form              | Meaning                                        |
//! | ----------------- | ---------------------------------------------- |
//! | `${VAR}`          | Required. Errors if `VAR` is unset.            |
//! | `${VAR:-default}` | Optional. Falls back to `default` if unset.    |
//! | `${VAR:?message}` | Required with a custom error message.          |
//! | `$$`              | Literal `$`.                                   |
//!
//! Errors include the YAML file path and 1-based line number of the offending
//! reference for fast diagnosis.
//!
//! References inside YAML comments are left untouched: a `#` that begins a
//! comment (preceded by start-of-line or whitespace, outside quoted scalars
//! and block scalar content) suppresses interpolation through end of line.
//!
//! # Env-only (v0.6)
//!
//! This interpolator resolves `${VAR}` from `std::env` ONLY. Secrets are NOT
//! resolved at config-parse time anymore — `${secret.<name>}` references are
//! left verbatim in the parsed config (they are resolved later, at consume /
//! spawn time, from the OS keychain). The env pass therefore skips over any
//! `${secret.*}` reference, passing it through untouched. There is no pluggable
//! keychain resolver in the config-parse path.

use std::env;

use anyhow::{anyhow, Result};

const SECRET_PREFIX: &str = "secret.";

/// Resolve a single `${...}` reference against the process environment.
fn lookup_env(key: &str) -> Option<String> {
    env::var(key).ok()
}

/// `${secret.<name>}` references are reserved for the secret consumer (resolved
/// at consume/spawn time, not at parse). The env interpolator must leave them
/// untouched (and must not validate the body, since `.` is otherwise illegal in
/// env-var names).
fn is_secret_reference(body: &str) -> bool {
    // Honor the leading whitespace tolerance of `${ NAME }` by stripping
    // ASCII whitespace before the prefix check.
    body.trim_start().starts_with(SECRET_PREFIX)
}

/// Peek at the bytes starting at `offset` and report whether they look like
/// a well-formed `${secret.<name>}` reference. Used so that the env-interp
/// pass can preserve `$$` escapes that are protecting a literal secret
/// reference for the downstream secret consumer.
fn looks_like_secret_ref_after(bytes: &[u8], offset: usize) -> bool {
    if offset + 1 >= bytes.len() {
        return false;
    }
    if bytes[offset] != b'{' {
        return false;
    }
    let body_start = offset + 1;
    let Some(close_off) = find_matching_close(&bytes[body_start..]) else {
        return false;
    };
    let body = match std::str::from_utf8(&bytes[body_start..body_start + close_off]) {
        Ok(s) => s,
        Err(_) => return false,
    };
    is_secret_reference(body)
}

/// Interpolate shell-style `${VAR}` references in `content`.
///
/// `source_label` is included in error messages — pass the YAML file path
/// (or any human-readable identifier) so users can locate the offending file.
/// `${secret.*}` references are left verbatim.
pub fn interpolate_env(content: &str, source_label: &str) -> Result<String> {
    interpolate_env_with(content, source_label, lookup_env)
}

/// Implementation seam used by unit tests to inject a hermetic env lookup.
pub fn interpolate_env_with<F>(content: &str, source_label: &str, resolver: F) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    // Walk byte-wise but push str slices so multi-byte UTF-8 sequences are
    // preserved intact. `$` is always ASCII (0x24), so it cannot appear inside
    // a multi-byte UTF-8 sequence — splitting on `$` boundaries is safe.
    let bytes = content.as_bytes();
    let mut comments = CommentSpans::new(content);
    let mut out = String::with_capacity(content.len());
    let mut i = 0usize;
    let mut copy_from = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'$' || comments.contains(i) {
            i += 1;
            continue;
        }

        // Flush everything since the last `$` (or start) as a single str slice.
        out.push_str(&content[copy_from..i]);

        // `$$` escapes a literal `$`.  However, when the escape immediately
        // precedes a `${secret.X}` reference, the secret consumer also needs to
        // see (and consume) the `$$` so a deliberately-escaped literal
        // `${secret.X}` survives. Pass it through unchanged in that case.
        if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            if looks_like_secret_ref_after(bytes, i + 2) {
                out.push_str("$$");
            } else {
                out.push('$');
            }
            i += 2;
            copy_from = i;
            continue;
        }

        // `${...}` reference.
        if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let start = i;
            let body_start = i + 2;
            let Some(close_off) = find_matching_close(&bytes[body_start..]) else {
                let line = line_number_for_offset(content, start);
                return Err(anyhow!(
                    "workflow YAML at {} line {} contains an unterminated `${{` env-var reference",
                    source_label,
                    line
                ));
            };
            let body = &content[body_start..body_start + close_off];
            if is_secret_reference(body) {
                // Reserved for the secret consumer — copy the entire
                // reference (including `${...}`) through untouched.
                out.push_str(&content[start..=body_start + close_off]);
                i = body_start + close_off + 1;
                copy_from = i;
                continue;
            }
            let resolved = resolve_reference(body, source_label, &resolver, || line_number_for_offset(content, start))?;
            out.push_str(&resolved);
            i = body_start + close_off + 1; // skip past `}`
            copy_from = i;
            continue;
        }

        // Lone `$` not followed by `{` or `$` passes through literally so YAML
        // strings like `cost $5` aren't disturbed.
        out.push('$');
        i += 1;
        copy_from = i;
    }

    out.push_str(&content[copy_from..]);
    Ok(out)
}

/// Byte ranges of YAML comments in `content`, in ascending order.
///
/// A `#` begins a comment when it is preceded by start-of-line or whitespace,
/// is not inside a single- or double-quoted scalar, and is not inside block
/// scalar (`|` / `>`) content. Quote state carries across lines so multi-line
/// quoted scalars containing `#` are not misread as comments. The scanner is
/// deliberately conservative: when context is ambiguous it reports no comment,
/// which preserves the historical interpolate-everything behavior.
fn yaml_comment_spans(content: &str) -> Vec<(usize, usize)> {
    fn is_block_scalar_header(effective: &str) -> bool {
        let trimmed = effective.trim_end();
        let Some(token) = trimmed.rsplit([' ', '\t']).next() else {
            return false;
        };
        let mut chars = token.chars();
        if !matches!(chars.next(), Some('|') | Some('>')) {
            return false;
        }
        if !chars.all(|c| matches!(c, '+' | '-' | '0'..='9')) {
            return false;
        }
        let prefix = trimmed[..trimmed.len() - token.len()].trim_end();
        prefix.is_empty() || prefix.ends_with(':') || prefix.ends_with('-')
    }

    let mut spans = Vec::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut block_scalar_indent: Option<usize> = None;
    let mut line_start = 0usize;

    for line in content.split_inclusive('\n') {
        let line_end = line_start + line.len();
        let stripped = line.strip_suffix('\n').unwrap_or(line);
        let bytes = stripped.as_bytes();
        let indent = bytes.iter().take_while(|b| **b == b' ' || **b == b'\t').count();
        let blank = indent == bytes.len();

        if let Some(parent_indent) = block_scalar_indent {
            if blank || indent > parent_indent {
                line_start = line_end;
                continue;
            }
            block_scalar_indent = None;
        }

        let mut comment_start: Option<usize> = None;
        // A quote opens a quoted scalar only where a new scalar can begin:
        // at line start, after an indicator byte (`:`, `-`, `[`, `{`, `,`),
        // or after a whitespace-delimited anchor/tag token (`&name`, `!tag`)
        // that itself sat at a scalar-start position. A quote that appears
        // after plain-scalar content (`note: Build "docs # ...`) is plain
        // text and must not swallow a following real comment.
        let mut can_open = true;
        let mut token: Option<(bool, u8)> = None;
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if in_double {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == b'"' {
                    in_double = false;
                    can_open = false;
                }
                i += 1;
                continue;
            }
            if in_single {
                if b == b'\'' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    in_single = false;
                    can_open = false;
                }
                i += 1;
                continue;
            }
            if b == b' ' || b == b'\t' {
                if let Some((opened_at_start, first)) = token.take() {
                    can_open = can_open || (opened_at_start && matches!(first, b'&' | b'!'));
                }
                i += 1;
                continue;
            }
            match b {
                b'"' if can_open => {
                    in_double = true;
                    token = None;
                    i += 1;
                    continue;
                }
                b'\'' if can_open => {
                    in_single = true;
                    token = None;
                    i += 1;
                    continue;
                }
                b'#' if i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t' => {
                    comment_start = Some(i);
                    break;
                }
                _ => {}
            }
            if token.is_none() {
                token = Some((can_open, b));
            }
            can_open = matches!(b, b':' | b'-' | b'[' | b'{' | b',');
            i += 1;
        }

        if let Some(start) = comment_start {
            spans.push((line_start + start, line_end));
        }
        if !in_single && !in_double {
            let effective = &stripped[..comment_start.unwrap_or(bytes.len())];
            if is_block_scalar_header(effective) {
                block_scalar_indent = Some(indent);
            }
        }
        line_start = line_end;
    }

    spans
}

/// Cursor over [`yaml_comment_spans`] output for the monotonically increasing
/// offsets the interpolator walks.
struct CommentSpans {
    spans: Vec<(usize, usize)>,
    next: usize,
}

impl CommentSpans {
    fn new(content: &str) -> Self {
        Self { spans: yaml_comment_spans(content), next: 0 }
    }

    fn contains(&mut self, offset: usize) -> bool {
        while self.next < self.spans.len() && self.spans[self.next].1 <= offset {
            self.next += 1;
        }
        self.next < self.spans.len() && self.spans[self.next].0 <= offset
    }
}

/// Scan `bytes` for the first unmatched `}`. Tracks brace depth so nested
/// `${VAR:-${OTHER}}` would still be parsed coherently if we choose to support
/// nesting later. For now we don't recurse — but balancing keeps us honest.
fn find_matching_close(bytes: &[u8]) -> Option<usize> {
    let mut depth = 0i32;
    for (idx, &b) in bytes.iter().enumerate() {
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            if depth == 0 {
                return Some(idx);
            }
            depth -= 1;
        }
    }
    None
}

fn resolve_reference<F, L>(body: &str, source_label: &str, resolver: &F, line_of: L) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
    L: Fn() -> usize,
{
    // Split on whichever of ':-' / ':?' occurs first, so a modifier payload
    // containing the other token (e.g. `${KEY:?missing :-(}`) is not
    // misparsed as the wrong shape.
    let default_idx = body.find(":-");
    let required_idx = body.find(":?");
    if let Some(idx) = default_idx.filter(|idx| required_idx.is_none_or(|other| *idx < other)) {
        let name = body[..idx].trim();
        validate_name(name, source_label, &line_of)?;
        let default = &body[idx + 2..];
        return Ok(match resolver(name) {
            Some(value) => value,
            None => default.to_string(),
        });
    }
    if let Some(idx) = required_idx {
        let name = body[..idx].trim();
        validate_name(name, source_label, &line_of)?;
        let message = body[idx + 2..].trim();
        return match resolver(name) {
            Some(value) => Ok(value),
            None => Err(anyhow!(
                "workflow YAML at {} line {} requires env var {}: {}",
                source_label,
                line_of(),
                name,
                if message.is_empty() { "value is unset" } else { message }
            )),
        };
    }

    let name = body.trim();
    validate_name(name, source_label, &line_of)?;
    match resolver(name) {
        Some(value) => Ok(value),
        None => Err(anyhow!("workflow YAML at {} line {} references unset env var {}.", source_label, line_of(), name)),
    }
}

fn validate_name<L>(name: &str, source_label: &str, line_of: &L) -> Result<()>
where
    L: Fn() -> usize,
{
    if name.is_empty() {
        return Err(anyhow!(
            "workflow YAML at {} line {} has an empty `${{}}` env-var reference",
            source_label,
            line_of()
        ));
    }
    if !name.chars().next().map(|c| c == '_' || c.is_ascii_alphabetic()).unwrap_or(false) {
        return Err(anyhow!(
            "workflow YAML at {} line {} env var name `{}` must start with a letter or underscore",
            source_label,
            line_of(),
            name
        ));
    }
    if !name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        return Err(anyhow!(
            "workflow YAML at {} line {} env var name `{}` may only contain letters, digits, and underscores",
            source_label,
            line_of(),
            name
        ));
    }
    Ok(())
}

/// Scan raw YAML for `${VAR}` references whose env-var name matches a
/// sensitive token pattern (TOKEN | KEY | SECRET | PASSWORD) and that are
/// NOT declared under the `secrets:` block. Returns one human-readable
/// warning per occurrence; the caller decides how to surface them. The
/// scan is best-effort and intentionally non-fatal — authors of trusted
/// YAML may have legitimate uses.
pub fn lint_sensitive_interpolations(content: &str, source_label: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut comments = CommentSpans::new(content);
    let mut line_offset = 0usize;
    let mut in_secrets_block = false;
    let mut in_env_block = false;
    let mut env_block_indent: Option<usize> = None;
    let mut secrets_indent: Option<usize> = None;

    for (line_idx, raw_line) in content.split_inclusive('\n').enumerate() {
        let line_start = line_offset;
        line_offset += raw_line.len();
        let line = raw_line.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        // Track top-level `secrets:` block scope by indentation.
        if !trimmed.starts_with('#') && !trimmed.is_empty() {
            if let Some(top_indent) = secrets_indent {
                if indent <= top_indent && !trimmed.starts_with("secrets:") {
                    in_secrets_block = false;
                    secrets_indent = None;
                }
            }
            if trimmed.starts_with("secrets:") && indent == 0 {
                in_secrets_block = true;
                secrets_indent = Some(indent);
            }

            // Track *_env: declaration lines — those declare env var
            // names, not values, so they are not sensitive interpolations
            // even when the field name matches a token pattern.
            if let Some(env_indent) = env_block_indent {
                if indent <= env_indent {
                    in_env_block = false;
                    env_block_indent = None;
                }
            }
            if !in_env_block {
                let key = trimmed.split(':').next().unwrap_or("").trim();
                if key.ends_with("_env") && !key.is_empty() {
                    in_env_block = true;
                    env_block_indent = Some(indent);
                }
            }
        }

        if in_secrets_block || in_env_block {
            continue;
        }

        // Walk the line for `${VAR}` references.
        let line_bytes = line.as_bytes();
        let mut i = 0usize;
        while i + 1 < line_bytes.len() {
            if line_bytes[i] == b'$' && line_bytes[i + 1] == b'{' && !comments.contains(line_start + i) {
                let body_start = i + 2;
                let body_rel = &line_bytes[body_start..];
                let Some(close_off) = find_matching_close(body_rel) else {
                    break;
                };
                let body = &line[body_start..body_start + close_off];
                if !is_secret_reference(body) && looks_like_sensitive_var(body) {
                    warnings.push(format!(
                        "workflow YAML at {} line {} interpolates env var `{}` which looks like a credential; \
                         consider declaring it under `secrets:` and using `${{secret.<name>}}` instead",
                        source_label,
                        line_idx + 1,
                        body.trim(),
                    ));
                }
                i = body_start + close_off + 1;
                continue;
            }
            i += 1;
        }
    }

    warnings
}

fn looks_like_sensitive_var(body: &str) -> bool {
    let trimmed = body.trim();
    // Strip default/required modifiers (`${VAR:-default}` and `${VAR:?msg}`).
    let name = trimmed.split([':']).next().unwrap_or("").trim();
    if name.is_empty() {
        return false;
    }
    let upper = name.to_ascii_uppercase();
    upper.contains("TOKEN") || upper.contains("KEY") || upper.contains("SECRET") || upper.contains("PASSWORD")
}

fn line_number_for_offset(content: &str, offset: usize) -> usize {
    let clamped = offset.min(content.len());
    content[..clamped].bytes().filter(|b| *b == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hermetic env stub: tests pass an explicit resolver instead of mutating
    /// process env, so they need no global lock.
    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::BTreeMap<String, String> =
            pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn expands_required_var() {
        let out =
            interpolate_env_with("api_token: ${KEY}\n", "test.yaml", env_map(&[("KEY", "secret-token")])).unwrap();
        assert_eq!(out, "api_token: secret-token\n");
    }

    #[test]
    fn errors_clearly_when_required_var_unset() {
        let src = "a: 1\nb: 2\napi_token: ${KEY}\n";
        let err = interpolate_env_with(src, ".animus/workflows/agents.yaml", env_map(&[])).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("line 3"), "missing line number: {msg}");
        assert!(msg.contains("KEY"), "missing var name: {msg}");
        assert!(msg.contains(".animus/workflows/agents.yaml"), "missing source label: {msg}");
    }

    #[test]
    fn uses_default_when_var_unset_with_default_syntax() {
        let out =
            interpolate_env_with("api_url: ${KEY:-https://api.example.com}\n", "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, "api_url: https://api.example.com\n");
    }

    #[test]
    fn prefers_set_var_over_default() {
        let out = interpolate_env_with(
            "api_url: ${KEY:-https://fallback.example.com}\n",
            "test.yaml",
            env_map(&[("KEY", "https://real.example.com")]),
        )
        .unwrap();
        assert_eq!(out, "api_url: https://real.example.com\n");
    }

    #[test]
    fn handles_multiple_vars_in_one_line() {
        let out = interpolate_env_with(
            "combo: \"${KEY}-${OTHER}\"\n",
            "test.yaml",
            env_map(&[("KEY", "alpha"), ("OTHER", "beta")]),
        )
        .unwrap();
        assert_eq!(out, "combo: \"alpha-beta\"\n");
    }

    #[test]
    fn escapes_literal_dollar_with_double_dollar() {
        let out = interpolate_env_with("price: $$5.00 raw\n", "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, "price: $5.00 raw\n");
    }

    #[test]
    fn required_with_custom_message() {
        let src = "a: ${KEY:?set this in your shell}\n";
        let err = interpolate_env_with(src, "test.yaml", env_map(&[])).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("set this in your shell"), "missing custom message: {msg}");
        assert!(msg.contains("KEY"));
    }

    #[test]
    fn required_message_containing_default_token_parses_as_required() {
        let src = "a: ${KEY:?missing key :-(}\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[("KEY", "present")])).unwrap();
        assert_eq!(out, "a: present\n");

        let err = interpolate_env_with(src, "test.yaml", env_map(&[])).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("missing key :-("), "missing custom message: {msg}");
        assert!(msg.contains("KEY"), "missing var name: {msg}");
    }

    #[test]
    fn default_containing_required_token_parses_as_default() {
        let out = interpolate_env_with("a: ${KEY:-fallback :? ok}\n", "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, "a: fallback :? ok\n");
    }

    #[test]
    fn lone_dollar_passes_through() {
        let out = interpolate_env_with("note: this costs $5 in total\n", "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, "note: this costs $5 in total\n");
    }

    #[test]
    fn unterminated_reference_errors_with_line() {
        let src = "ok: yes\nbroken: ${MISSING_BRACE\n";
        let err = interpolate_env_with(src, "test.yaml", env_map(&[])).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("line 2"), "missing line: {msg}");
        assert!(msg.contains("unterminated"));
    }

    #[test]
    fn rejects_empty_name() {
        let err = interpolate_env_with("a: ${}\n", "test.yaml", env_map(&[])).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("empty"));
    }

    #[test]
    fn preserves_multibyte_utf8_around_substitution() {
        // Em-dash (U+2014) is 3 bytes in UTF-8 and previously triggered control-character
        // YAML parse errors when the interpolator walked byte-by-byte.
        let src = "note: a — b — ${KEY}\nemoji: 🚀 — done\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[("KEY", "expanded")])).unwrap();
        assert_eq!(out, "note: a — b — expanded\nemoji: 🚀 — done\n");
    }

    #[test]
    fn rejects_invalid_name() {
        let err = interpolate_env_with("a: ${1BAD}\n", "test.yaml", env_map(&[])).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("must start with"));
    }

    #[test]
    fn leaves_secret_references_untouched() {
        // `${secret.*}` is no longer resolved at parse time — it survives
        // verbatim regardless of env contents.
        let out = interpolate_env_with("token: ${secret.api}\n", "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, "token: ${secret.api}\n");
    }

    #[test]
    fn preserves_escaped_literal_secret_reference() {
        let out = interpolate_env_with("prompt: $${secret.api}\n", "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, "prompt: $${secret.api}\n");
    }

    #[test]
    fn comment_only_line_with_unset_var_is_left_untouched() {
        let src = "# export ${KEY}\nkey: value\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn comment_with_invalid_var_name_is_left_untouched() {
        let src = "# see ${docs-url}\nkey: value\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn trailing_comment_after_value_is_left_untouched() {
        let src = "key: ${KEY} # docs: ${UNSET_IN_COMMENT}\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[("KEY", "expanded")])).unwrap();
        assert_eq!(out, "key: expanded # docs: ${UNSET_IN_COMMENT}\n");
    }

    #[test]
    fn hash_inside_quoted_scalar_still_interpolates() {
        let out =
            interpolate_env_with("key: \"#not-a-comment ${KEY}\"\n", "test.yaml", env_map(&[("KEY", "expanded")]))
                .unwrap();
        assert_eq!(out, "key: \"#not-a-comment expanded\"\n");
    }

    #[test]
    fn hash_heading_inside_block_scalar_still_interpolates() {
        let src = "prompt: |\n  # Heading ${KEY}\n  body\nkey: value\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[("KEY", "expanded")])).unwrap();
        assert_eq!(out, "prompt: |\n  # Heading expanded\n  body\nkey: value\n");
    }

    #[test]
    fn comment_after_block_scalar_content_is_left_untouched() {
        let src = "prompt: |\n  body text\n# note ${KEY}\nkey: value\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn unpaired_quote_in_plain_scalar_does_not_suppress_trailing_comment() {
        let src = "directive: Build \"docs # see ${KEY}\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn quote_after_indicator_still_opens_quoted_scalar() {
        let src = "items: [a, \"#tag ${KEY}\"]\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[("KEY", "expanded")])).unwrap();
        assert_eq!(out, "items: [a, \"#tag expanded\"]\n");
    }

    #[test]
    fn quote_after_anchor_still_opens_quoted_scalar() {
        let src = "directive: &d \"build # ${KEY}\"\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[("KEY", "expanded")])).unwrap();
        assert_eq!(out, "directive: &d \"build # expanded\"\n");
    }

    #[test]
    fn quote_after_tag_still_opens_quoted_scalar() {
        let src = "directive: !!str \"build # ${KEY}\"\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[("KEY", "expanded")])).unwrap();
        assert_eq!(out, "directive: !!str \"build # expanded\"\n");
    }

    #[test]
    fn apostrophe_in_plain_scalar_does_not_suppress_later_comment() {
        let src = "note: it's fine\n# export ${KEY}\n";
        let out = interpolate_env_with(src, "test.yaml", env_map(&[])).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn lint_skips_sensitive_looking_references_in_comments() {
        let src = "# export ${LINEAR_TOKEN}\nurl: ${TEAM_URL:-https://example.com}\n";
        let warnings = lint_sensitive_interpolations(src, "test.yaml");
        assert!(warnings.is_empty(), "comment-only reference should not warn: {warnings:?}");
    }

    #[test]
    fn lint_flags_sensitive_looking_interpolations() {
        let src = "token: ${LINEAR_TOKEN}\n";
        let warnings = lint_sensitive_interpolations(src, "test.yaml");
        assert_eq!(warnings.len(), 1, "expected one sensitive-interpolation warning: {warnings:?}");
        assert!(warnings[0].contains("LINEAR_TOKEN"));
    }
}
