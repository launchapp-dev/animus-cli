//! Log redaction applied before persistence.
//!
//! Scrubs secret-shaped values from [`LogEntry`] fields before the logger
//! writes a JSON line to disk.
//!
//! Two parallel mechanisms run together:
//!
//! 1. **Value-content regex redaction** — string contents are scanned for
//!    `key=value` / `key: value` shapes (e.g. `api_key=sk_live_abc`,
//!    `authorization: Bearer ...`). Matches collapse to
//!    `<key>=***REDACTED***`, preserving the key so operators still see
//!    the *shape* of what was logged. Custom regexes can be added via
//!    `ANIMUS_LOG_REDACT_PATTERNS` (comma-separated).
//!
//! 2. **Key-name redaction** — when recursing into a JSON object, any
//!    `(key, value)` pair whose key matches the default secret-key set
//!    (case-insensitive, snake_case / kebab-case / `X-*-Key` variants)
//!    has its value replaced with `***REDACTED***` regardless of the
//!    value's content. This catches `meta({"api_key":"sk_live_..."})`
//!    where the value alone (`"sk_live_..."`) would not match any
//!    content regex. Override the secret-key list via
//!    `ANIMUS_LOG_REDACT_KEYS` (comma-separated names; each is matched
//!    as a case-insensitive substring against the JSON key name).
//!
//! v0.4.10: the redactor is wired into [`crate::Logger::write_entry`] so
//! every emit site picks up redaction automatically. The previous home
//! under `orchestrator_daemon_runtime::control::log_redact` re-exports
//! these symbols for backwards compatibility.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::LogEntry;

/// Environment variable for additional redaction patterns
/// (comma-separated regex strings).
pub const REDACT_PATTERNS_ENV: &str = "ANIMUS_LOG_REDACT_PATTERNS";

/// Environment variable overriding the default secret-key name list
/// (comma-separated names; case-insensitive substring match against
/// JSON object keys).
pub const REDACT_KEYS_ENV: &str = "ANIMUS_LOG_REDACT_KEYS";

/// Placeholder value substituted for redacted secrets.
pub const REDACTED_PLACEHOLDER: &str = "***REDACTED***";

const DEFAULT_PATTERN: &str = r"(?i)(api[_-]?key|password|token|secret|authorization)\s*[:=]\s*\S+";

/// Default secret key names. Matched case-insensitively as a substring
/// against JSON object keys, with `_` and `-` treated as equivalent so
/// both snake_case and kebab-case spellings hit. Includes the common
/// `X-API-Key` / `X-*-Token` header families used in HTTP metadata.
const DEFAULT_SECRET_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "token",
    "access_token",
    "accesstoken",
    "refresh_token",
    "refreshtoken",
    "id_token",
    "idtoken",
    "secret",
    "client_secret",
    "clientsecret",
    "password",
    "passwd",
    "pwd",
    "authorization",
    "bearer",
    "private_key",
    "privatekey",
    "signing_key",
    "signingkey",
    "secretkey",
    "bearertoken",
    "x-api-key",
    "x_api_key",
    "xapikey",
];

fn default_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| Regex::new(DEFAULT_PATTERN).expect("default redaction pattern compiles"))
}

fn custom_regexes() -> &'static Vec<Regex> {
    static RXS: OnceLock<Vec<Regex>> = OnceLock::new();
    RXS.get_or_init(|| {
        std::env::var(REDACT_PATTERNS_ENV)
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .filter_map(|p| Regex::new(p).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    })
}

/// Normalise a key/needle for comparison:
/// - Insert `_` between camelCase / PascalCase word boundaries, including
///   acronym→word boundaries:
///   - `secretKey` → `secret_key`
///   - `apiKey`    → `api_key`
///   - `APIToken`  → `api_token` (acronym → word)
///   - `XApiKey`   → `x_api_key` (single-letter acronym → word)
///   - `XMLHttpRequest` → `xml_http_request`
/// - Lowercase the result.
/// - Collapse `-` to `_` so kebab-case (`x-api-key`) and snake_case
///   (`x_api_key`) compare identically.
///
/// After normalisation, every key has `_` as the sole word separator,
/// which lets `matches_at_word_boundary` apply uniform boundary checks.
fn normalize_key(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(chars.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        let prev = if i > 0 { Some(chars[i - 1]) } else { None };
        let next = chars.get(i + 1).copied();
        if c.is_ascii_uppercase() {
            let after_lower_or_digit = matches!(prev, Some(p) if p.is_ascii_lowercase() || p.is_ascii_digit());
            let acronym_to_word =
                matches!(prev, Some(p) if p.is_ascii_uppercase()) && matches!(next, Some(n) if n.is_ascii_lowercase());
            if (after_lower_or_digit || acronym_to_word) && !out.ends_with('_') {
                out.push('_');
            }
        }
        let lc = c.to_ascii_lowercase();
        out.push(if lc == '-' { '_' } else { lc });
    }
    out
}

fn secret_keys() -> &'static Vec<String> {
    static KEYS: OnceLock<Vec<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let from_env = std::env::var(REDACT_KEYS_ENV)
            .ok()
            .map(|raw| raw.split(',').map(str::trim).filter(|s| !s.is_empty()).map(normalize_key).collect::<Vec<_>>());
        match from_env {
            Some(list) if !list.is_empty() => list,
            _ => DEFAULT_SECRET_KEYS.iter().map(|s| normalize_key(s)).collect(),
        }
    })
}

/// Returns `true` if `needle` appears in `haystack` at `_`-delimited
/// word boundaries (or as the whole string). Used so `token` matches
/// `access_token` and `x_api_token` but NOT `input_tokens` or
/// `max_tokens` — those are common non-secret LLM observability fields
/// that must not be clobbered.
fn matches_at_word_boundary(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    for (start, _) in haystack.match_indices(needle) {
        let end = start + needle.len();
        let left_ok = start == 0 || haystack.as_bytes()[start - 1] == b'_';
        let right_ok = end == haystack.len() || haystack.as_bytes()[end] == b'_';
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

/// Returns `true` when `key` should be treated as a secret-bearing JSON
/// field whose value must be redacted regardless of content.
fn is_secret_key(key: &str) -> bool {
    let norm = normalize_key(key);
    secret_keys().iter().any(|needle| matches_at_word_boundary(&norm, needle))
}

/// Internal helper: check key against an explicit needle list. Used by
/// tests to exercise the override mechanism without racing the
/// process-global `secret_keys()` cache (which reads
/// `ANIMUS_LOG_REDACT_KEYS` exactly once per process).
#[cfg(test)]
fn is_secret_key_with(key: &str, needles: &[String]) -> bool {
    let norm = normalize_key(key);
    needles.iter().any(|needle| matches_at_word_boundary(&norm, needle))
}

#[cfg(test)]
fn redact_json_value_with(value: &mut Value, needles: &[String]) {
    match value {
        Value::String(s) => {
            let red = redact_string(s);
            if red != *s {
                *s = red;
            }
        }
        Value::Array(arr) => {
            for v in arr {
                redact_json_value_with(v, needles);
            }
        }
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if is_secret_key_with(k, needles) {
                    *v = Value::String(REDACTED_PLACEHOLDER.to_string());
                } else {
                    redact_json_value_with(v, needles);
                }
            }
        }
        _ => {}
    }
}

/// Redact a single string in place — replaces matched key=value pairs
/// with `<key>=***REDACTED***` (or `<key>: ***REDACTED***`).
pub fn redact_string(input: &str) -> String {
    let mut out = default_regex()
        .replace_all(input, |caps: &regex::Captures<'_>| {
            let key = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            format!("{}=***REDACTED***", key)
        })
        .into_owned();
    for rx in custom_regexes() {
        out = rx.replace_all(&out, REDACTED_PLACEHOLDER).into_owned();
    }
    out
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::String(s) => {
            let red = redact_string(s);
            if red != *s {
                *s = red;
            }
        }
        Value::Array(arr) => {
            for v in arr {
                redact_json_value(v);
            }
        }
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if is_secret_key(k) {
                    *v = Value::String(REDACTED_PLACEHOLDER.to_string());
                } else {
                    redact_json_value(v);
                }
            }
        }
        _ => {}
    }
}

/// Apply redaction to a [`LogEntry`] in place. Scrubs the `msg` string
/// and any string values inside the `meta` JSON tree. Other typed fields
/// (workflow_id, model, etc.) are identifiers — not secret-bearing — so
/// we leave them alone to keep the hot path cheap.
pub fn redact_log_entry(entry: &mut LogEntry) {
    let new_msg = redact_string(&entry.msg);
    if new_msg != entry.msg {
        entry.msg = new_msg;
    }
    if let Some(meta) = entry.meta.as_mut() {
        redact_json_value(meta);
    }
    if let Some(content) = entry.content.as_mut() {
        let red = redact_string(content);
        if red != *content {
            *content = red;
        }
    }
    if let Some(err) = entry.error.as_mut() {
        let red = redact_string(err);
        if red != *err {
            *err = red;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Level;
    use serde_json::json;

    fn entry_with_msg(msg: &str) -> LogEntry {
        LogEntry {
            ts: "2026-05-22T00:00:00Z".to_string(),
            level: Level::Info,
            cat: "test".to_string(),
            msg: msg.to_string(),
            workflow_id: None,
            task_id: None,
            schedule_id: None,
            phase_id: None,
            model: None,
            tool: None,
            provider: None,
            run_id: None,
            session_id: None,
            turn: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            tool_calls: None,
            role: None,
            content: None,
            subject_id: None,
            status: None,
            from_status: None,
            to_status: None,
            branch: None,
            pr_number: None,
            mcp_tool: None,
            mcp_server: None,
            fallback_from: None,
            fallback_to: None,
            cost: None,
            exit_code: None,
            duration_ms: None,
            error: None,
            meta: None,
        }
    }

    #[test]
    fn redacts_default_api_key_pattern() {
        let mut e = entry_with_msg("calling provider with api_key=sk_live_abc123");
        redact_log_entry(&mut e);
        assert!(e.msg.contains("***REDACTED***"));
        assert!(!e.msg.contains("sk_live_abc123"));
    }

    #[test]
    fn redacts_authorization_header() {
        let mut e = entry_with_msg("request authorization: Bearer xyz");
        redact_log_entry(&mut e);
        assert!(e.msg.contains("***REDACTED***"));
        assert!(!e.msg.contains("Bearer"));
    }

    #[test]
    fn redacts_nested_meta_json() {
        let mut e = entry_with_msg("ok");
        e.meta = Some(json!({
            "outer": {
                "headers": "authorization: Bearer secret123",
                "harmless": "value",
            },
            "list": [
                "password=hunter2",
                "fine"
            ]
        }));
        redact_log_entry(&mut e);
        let meta = e.meta.as_ref().unwrap();
        let nested = meta.get("outer").and_then(|v| v.get("headers")).and_then(|v| v.as_str()).unwrap();
        assert!(nested.contains("***REDACTED***"), "nested header value not redacted: {nested}");
        let list_first = meta.get("list").and_then(|v| v.get(0)).and_then(|v| v.as_str()).unwrap();
        assert!(list_first.contains("***REDACTED***"));
        let list_second = meta.get("list").and_then(|v| v.get(1)).and_then(|v| v.as_str()).unwrap();
        assert_eq!(list_second, "fine");
    }

    #[test]
    fn preserves_non_secret_fields() {
        let mut e = entry_with_msg("no secrets here, just a plain message");
        let original = e.msg.clone();
        redact_log_entry(&mut e);
        assert_eq!(e.msg, original);
    }

    #[test]
    fn redacts_token_in_content_and_error() {
        let mut e = entry_with_msg("ok");
        e.content = Some("payload: token=abc.def.ghi".to_string());
        e.error = Some("failed: secret: tophat".to_string());
        redact_log_entry(&mut e);
        assert!(e.content.as_ref().unwrap().contains("***REDACTED***"));
        assert!(e.error.as_ref().unwrap().contains("***REDACTED***"));
    }

    #[test]
    fn redacts_value_when_key_is_secret_api_key() {
        let mut e = entry_with_msg("ok");
        e.meta = Some(json!({ "api_key": "sk_live_abc" }));
        redact_log_entry(&mut e);
        let meta = e.meta.as_ref().unwrap();
        let val = meta.get("api_key").and_then(|v| v.as_str()).unwrap();
        assert_eq!(val, REDACTED_PLACEHOLDER);
        assert!(!serde_json::to_string(meta).unwrap().contains("sk_live_abc"));
    }

    #[test]
    fn redacts_nested_secret_key_password() {
        let mut e = entry_with_msg("ok");
        e.meta = Some(json!({ "safe": { "password": "hunter2" } }));
        redact_log_entry(&mut e);
        let meta = e.meta.as_ref().unwrap();
        let val = meta.get("safe").and_then(|v| v.get("password")).and_then(|v| v.as_str()).unwrap();
        assert_eq!(val, REDACTED_PLACEHOLDER);
        assert!(!serde_json::to_string(meta).unwrap().contains("hunter2"));
    }

    #[test]
    fn key_based_redaction_handles_camel_case_keys() {
        // camelCase JSON keys split on case boundaries during
        // normalization (e.g. `secretKey` → `secret_key`), so the
        // single-word needles (`secret`, `bearer`, `token`, etc.)
        // match at word boundaries.
        let mut e = entry_with_msg("ok");
        e.meta = Some(json!({
            "privateKey": "-----BEGIN PRIVATE KEY-----",
            "signingKey": "sig-abc",
            "accessToken": "tok-xyz",
            "apiKey": "ak-123",
            "secretKey": "sk-deadbeef",
            "bearerToken": "bt-xyz",
        }));
        redact_log_entry(&mut e);
        let meta = e.meta.as_ref().unwrap();
        for key in ["privateKey", "signingKey", "accessToken", "apiKey", "secretKey", "bearerToken"] {
            let val = meta.get(key).and_then(|v| v.as_str()).unwrap_or_else(|| panic!("missing key {key} in {meta:?}"));
            assert_eq!(val, REDACTED_PLACEHOLDER, "camelCase key {key} not redacted");
        }
        let dumped = serde_json::to_string(meta).unwrap();
        for leak in ["BEGIN PRIVATE KEY", "sig-abc", "tok-xyz", "ak-123", "sk-deadbeef", "bt-xyz"] {
            assert!(!dumped.contains(leak), "leak {leak} in {dumped}");
        }
    }

    #[test]
    fn key_based_redaction_handles_kebab_and_x_headers() {
        let mut e = entry_with_msg("ok");
        e.meta = Some(json!({
            "X-API-Key": "abc.def",
            "access-token": "tok-xyz",
            "Authorization": "Bearer 123",
        }));
        redact_log_entry(&mut e);
        let meta = e.meta.as_ref().unwrap();
        for key in ["X-API-Key", "access-token", "Authorization"] {
            let val = meta.get(key).and_then(|v| v.as_str()).unwrap_or_else(|| panic!("missing key {key} in {meta:?}"));
            assert_eq!(val, REDACTED_PLACEHOLDER, "key {key} not redacted");
        }
        let dumped = serde_json::to_string(meta).unwrap();
        for leak in ["abc.def", "tok-xyz", "Bearer 123"] {
            assert!(!dumped.contains(leak), "leak {leak} in {dumped}");
        }
    }

    #[test]
    fn content_regex_still_redacts_when_key_is_safe() {
        // Key NOT in the secret-key set, but VALUE matches the
        // content regex (`api_key=value` shape) → value-content path
        // must still redact. Proves the two mechanisms compose.
        let mut e = entry_with_msg("ok");
        e.meta = Some(json!({ "description": "my api_key=sk_live_abc was leaked" }));
        redact_log_entry(&mut e);
        let meta = e.meta.as_ref().unwrap();
        let val = meta.get("description").and_then(|v| v.as_str()).unwrap();
        assert!(val.contains("***REDACTED***"), "value not redacted: {val}");
        assert!(!val.contains("sk_live_abc"), "secret leaked: {val}");
    }

    #[test]
    fn does_not_clobber_token_count_observability_fields() {
        // Regression: substring matching of `token` would falsely
        // redact common LLM observability metadata. Word-boundary
        // matching keeps these intact.
        let mut e = entry_with_msg("ok");
        e.meta = Some(json!({
            "input_tokens": 1234,
            "output_tokens": 567,
            "total_tokens": 1801,
            "max_tokens": 4096,
            "tokens": { "input": 1, "output": 2 },
        }));
        let before = e.meta.clone();
        redact_log_entry(&mut e);
        assert_eq!(e.meta, before, "token-count observability fields must NOT be redacted: {:?}", e.meta);
    }

    #[test]
    fn matches_at_word_boundary_basic_cases() {
        assert!(matches_at_word_boundary("access_token", "token"));
        assert!(matches_at_word_boundary("x_api_token", "token"));
        assert!(matches_at_word_boundary("token", "token"));
        assert!(matches_at_word_boundary("token_v2", "token"));
        assert!(!matches_at_word_boundary("input_tokens", "token"));
        assert!(!matches_at_word_boundary("max_tokens", "token"));
        assert!(!matches_at_word_boundary("tokens", "token"));
        assert!(!matches_at_word_boundary("tokenizer", "token"));
        // Whole-string camelCase normalized form.
        assert!(matches_at_word_boundary("privatekey", "privatekey"));
    }

    #[test]
    fn non_secret_object_left_untouched() {
        let mut e = entry_with_msg("ok");
        e.meta = Some(json!({ "normal": "value", "count": 42 }));
        let before = e.meta.clone();
        redact_log_entry(&mut e);
        assert_eq!(e.meta, before);
    }

    #[test]
    fn env_override_replaces_default_secret_keys() {
        // Validate the override mechanism by exercising the same code
        // path with an explicit needle list — avoids racing the
        // process-global OnceLock that caches ANIMUS_LOG_REDACT_KEYS.
        let needles: Vec<String> = vec!["custom_field".to_string()];
        let mut value = json!({
            "custom_field": "leak-me",
            "api_key": "kept-because-override-replaces-defaults",
        });
        redact_json_value_with(&mut value, &needles);
        assert_eq!(value.get("custom_field").and_then(|v| v.as_str()).unwrap(), REDACTED_PLACEHOLDER);
        // Override replaces defaults — api_key is no longer in the
        // secret-key list, so its value is processed by the content
        // regex path only (which doesn't match the bare value).
        assert_eq!(value.get("api_key").and_then(|v| v.as_str()).unwrap(), "kept-because-override-replaces-defaults");
    }

    #[test]
    fn normalize_key_collapses_case_and_separators() {
        assert_eq!(normalize_key("API_KEY"), "api_key");
        assert_eq!(normalize_key("api-key"), "api_key");
        assert_eq!(normalize_key("X-API-Key"), "x_api_key");
        // camelCase / PascalCase split on case boundary
        assert_eq!(normalize_key("secretKey"), "secret_key");
        assert_eq!(normalize_key("bearerToken"), "bearer_token");
        assert_eq!(normalize_key("apiKey"), "api_key");
        assert_eq!(normalize_key("privateKey"), "private_key");
        assert_eq!(normalize_key("accessToken"), "access_token");
        // Acronym → word boundary
        assert_eq!(normalize_key("APIToken"), "api_token");
        assert_eq!(normalize_key("APISecret"), "api_secret");
        assert_eq!(normalize_key("XApiKey"), "x_api_key");
        assert_eq!(normalize_key("XMLHttpRequest"), "xml_http_request");
    }

    #[test]
    fn key_based_redaction_handles_acronym_pascal_keys() {
        let mut e = entry_with_msg("ok");
        e.meta = Some(json!({
            "APIToken": "tok-acr",
            "APISecret": "sec-acr",
            "XApiKey": "xk-acr",
        }));
        redact_log_entry(&mut e);
        let meta = e.meta.as_ref().unwrap();
        for key in ["APIToken", "APISecret", "XApiKey"] {
            let val = meta.get(key).and_then(|v| v.as_str()).unwrap_or_else(|| panic!("missing key {key} in {meta:?}"));
            assert_eq!(val, REDACTED_PLACEHOLDER, "acronym-prefixed key {key} not redacted");
        }
        let dumped = serde_json::to_string(meta).unwrap();
        for leak in ["tok-acr", "sec-acr", "xk-acr"] {
            assert!(!dumped.contains(leak), "leak {leak} in {dumped}");
        }
    }

    #[test]
    fn write_entry_redacts_msg_in_persisted_line() {
        // Wire-up regression: redact_log_entry must run *before* the JSON
        // line is appended to events.jsonl, not just before display.
        let tmp = tempfile::tempdir().expect("tempdir");
        let logger = crate::Logger::open(tmp.path(), "test.jsonl", Level::Info);
        logger.info("phase", "calling provider with api_key=sk_live_abc123").emit();
        let path = tmp.path().join("test.jsonl");
        let contents = std::fs::read_to_string(&path).expect("read log");
        assert!(contents.contains("***REDACTED***"), "persisted line not redacted: {contents}");
        assert!(!contents.contains("sk_live_abc123"), "secret leaked to disk: {contents}");
    }
}
