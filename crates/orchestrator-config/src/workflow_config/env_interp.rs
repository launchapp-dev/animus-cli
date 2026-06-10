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

use std::collections::BTreeMap;
use std::env;
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{anyhow, Result};

use super::types::SecretRef;

const SECRET_PREFIX: &str = "secret.";

/// Process-wide hook the workflow-YAML interpolator queries as a
/// fallback when `${VAR}` is not present in `std::env`. The CLI
/// installs a real implementation that reads from the OS keychain;
/// embedders that never set one keep the historical "env-only" lookup.
pub trait WorkflowSecretResolver: Send + Sync + 'static {
    /// Return the value for `key` from the installed secret store, or
    /// `None` if the key is not present.
    fn resolve(&self, key: &str) -> Option<String>;
}

fn workflow_secret_resolver_slot() -> &'static RwLock<Option<Arc<dyn WorkflowSecretResolver>>> {
    static SLOT: OnceLock<RwLock<Option<Arc<dyn WorkflowSecretResolver>>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install the process-wide workflow secret resolver. First-installer-wins.
pub fn install_workflow_secret_resolver(resolver: Arc<dyn WorkflowSecretResolver>) -> bool {
    let mut guard = workflow_secret_resolver_slot().write().expect("workflow secret resolver lock poisoned");
    if guard.is_some() {
        return false;
    }
    *guard = Some(resolver);
    true
}

/// Test-only: unconditionally replace the installed resolver.
pub fn install_workflow_secret_resolver_for_test(resolver: Arc<dyn WorkflowSecretResolver>) {
    let mut guard = workflow_secret_resolver_slot().write().expect("workflow secret resolver lock poisoned");
    *guard = Some(resolver);
}

/// Test-only: clear the installed resolver so the interpolator falls
/// back to env-only lookups.
pub fn clear_workflow_secret_resolver_for_test() {
    let mut guard = workflow_secret_resolver_slot().write().expect("workflow secret resolver lock poisoned");
    *guard = None;
}

fn current_workflow_secret_resolver() -> Option<Arc<dyn WorkflowSecretResolver>> {
    workflow_secret_resolver_slot().read().expect("workflow secret resolver lock poisoned").clone()
}

/// Resolve a single `${...}` reference against the process environment.
///
/// This is factored out so tests can stub the environment via `EnvVarGuard`
/// without needing to plumb a custom resolver through the call sites.
fn lookup_env(key: &str) -> Option<String> {
    env::var(key).ok()
}

/// Public lookup chain: `std::env` first, then the installed
/// [`WorkflowSecretResolver`] (typically a keychain-backed store). The
/// chain is pure when no resolver is installed.
fn lookup_env_then_secret_store(key: &str) -> Option<String> {
    if let Some(value) = lookup_env(key) {
        return Some(value);
    }
    current_workflow_secret_resolver().and_then(|store| store.resolve(key))
}

/// `${secret.<name>}` references are reserved for the dedicated secrets
/// interpolation pass. The env interpolator must leave them untouched (and
/// must not validate the body, since `.` is otherwise illegal in env-var
/// names).
fn is_secret_reference(body: &str) -> bool {
    // Honor the leading whitespace tolerance of `${ NAME }` by stripping
    // ASCII whitespace before the prefix check.
    body.trim_start().starts_with(SECRET_PREFIX)
}

/// Peek at the bytes starting at `offset` and report whether they look like
/// a well-formed `${secret.<name>}` reference. Used so that the env-interp
/// pass can preserve `$$` escapes that are protecting a literal secret
/// reference for the downstream secrets pass.
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
pub fn interpolate_env(content: &str, source_label: &str) -> Result<String> {
    interpolate_env_with(content, source_label, lookup_env_then_secret_store)
}

/// Implementation seam used by unit tests to inject a hermetic env lookup.
pub(crate) fn interpolate_env_with<F>(content: &str, source_label: &str, resolver: F) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    // Walk byte-wise but push str slices so multi-byte UTF-8 sequences are
    // preserved intact. `$` is always ASCII (0x24), so it cannot appear inside
    // a multi-byte UTF-8 sequence — splitting on `$` boundaries is safe.
    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut i = 0usize;
    let mut copy_from = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }

        // Flush everything since the last `$` (or start) as a single str slice.
        out.push_str(&content[copy_from..i]);

        // `$$` escapes a literal `$`.  However, when the escape immediately
        // precedes a `${secret.X}` reference, the secrets pass also needs to
        // see (and consume) the `$$` so a deliberately-escaped literal
        // `${secret.X}` survives both passes. Pass it through unchanged in
        // that case — the secrets interpolator handles the collapse.
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
                // Reserved for the dedicated secrets pass — copy the entire
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
        return Ok(resolver(name).unwrap_or_else(|| default.to_string()));
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

/// Resolve every `${secret.<name>}` reference in `content` against the
/// declared `secrets` block, reading the mapped env var at compile time.
///
/// - Unknown secret names error with the file path and 1-based line number.
/// - Required-but-unset env vars error with the same location info.
/// - Optional unset secrets resolve to an empty string.
pub fn interpolate_secrets(content: &str, source_label: &str, secrets: &BTreeMap<String, SecretRef>) -> Result<String> {
    interpolate_secrets_with(content, source_label, secrets, lookup_env_then_secret_store)
}

pub(crate) fn interpolate_secrets_with<F>(
    content: &str,
    source_label: &str,
    secrets: &BTreeMap<String, SecretRef>,
    resolver: F,
) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut i = 0usize;
    let mut copy_from = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        out.push_str(&content[copy_from..i]);

        // `$$` escapes — handled by interpolate_env earlier, but if a caller
        // skips that pass we still want to preserve them coherently.
        if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            out.push('$');
            i += 2;
            copy_from = i;
            continue;
        }

        if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let start = i;
            let body_start = i + 2;
            let Some(close_off) = find_matching_close(&bytes[body_start..]) else {
                let line = line_number_for_offset(content, start);
                return Err(anyhow!(
                    "workflow YAML at {} line {} contains an unterminated `${{` reference",
                    source_label,
                    line
                ));
            };
            let body = &content[body_start..body_start + close_off];
            if !is_secret_reference(body) {
                // Not a secret — preserve as-is so non-secret env vars that
                // the env pass passed through (e.g. unknown future syntax)
                // are not consumed here.
                out.push_str(&content[start..=body_start + close_off]);
                i = body_start + close_off + 1;
                copy_from = i;
                continue;
            }

            let resolved = resolve_secret_reference(body, source_label, secrets, &resolver, || {
                line_number_for_offset(content, start)
            })?;
            out.push_str(&resolved);
            i = body_start + close_off + 1;
            copy_from = i;
            continue;
        }

        out.push('$');
        i += 1;
        copy_from = i;
    }

    out.push_str(&content[copy_from..]);
    Ok(out)
}

fn resolve_secret_reference<F, L>(
    body: &str,
    source_label: &str,
    secrets: &BTreeMap<String, SecretRef>,
    resolver: &F,
    line_of: L,
) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
    L: Fn() -> usize,
{
    let key = body.trim().strip_prefix(SECRET_PREFIX).unwrap_or("").trim();
    if key.is_empty() {
        return Err(anyhow!(
            "workflow YAML at {} line {} has an empty `${{secret.}}` reference",
            source_label,
            line_of()
        ));
    }
    let Some(secret) = secrets.get(key) else {
        return Err(anyhow!(
            "workflow YAML at {} line {} references undeclared secret `{}`; add it under the top-level `secrets:` block",
            source_label,
            line_of(),
            key
        ));
    };

    let env_name = secret.env.trim();
    if env_name.is_empty() {
        return Err(anyhow!(
            "workflow YAML at {} line {} secret `{}` has an empty `env` mapping",
            source_label,
            line_of(),
            key
        ));
    }

    match resolver(env_name) {
        Some(value) => Ok(value),
        None if secret.required => Err(anyhow!(
            "workflow YAML at {} line {} secret `{}` requires env var {} to be set",
            source_label,
            line_of(),
            key,
            env_name
        )),
        // Optional and unset — resolve to empty string.
        None => Ok(String::new()),
    }
}

/// Resolve both `${VAR}` and `${secret.<name>}` references in a single pass
/// over `content`, dispatching per reference on the `secret.` prefix.
///
/// Substituted values are never re-scanned, so env or secret values that
/// happen to contain `$$`, `${`, or `${secret.X}` are emitted verbatim
/// instead of being collapsed, failing compilation, or resolved as secrets.
/// Error line numbers are always computed against the original content.
pub fn interpolate_env_and_secrets(
    content: &str,
    source_label: &str,
    secrets: &BTreeMap<String, SecretRef>,
) -> Result<String> {
    interpolate_env_and_secrets_with(content, source_label, secrets, lookup_env_then_secret_store)
}

/// Like [`interpolate_env_and_secrets`], but also returns the map of secret
/// name → resolved value for every `${secret.<name>}` reference that was
/// substituted (ordinary `${VAR}` env interpolations are not collected).
/// Callers use the map to redact resolved secret values from any
/// user-visible diagnostics built from the substituted content.
pub(crate) fn interpolate_env_and_secrets_collecting(
    content: &str,
    source_label: &str,
    secrets: &BTreeMap<String, SecretRef>,
) -> Result<(String, BTreeMap<String, String>)> {
    interpolate_env_and_secrets_with_resolutions(content, source_label, secrets, lookup_env_then_secret_store)
}

pub(crate) fn interpolate_env_and_secrets_with<F>(
    content: &str,
    source_label: &str,
    secrets: &BTreeMap<String, SecretRef>,
    resolver: F,
) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    interpolate_env_and_secrets_with_resolutions(content, source_label, secrets, resolver).map(|(out, _)| out)
}

fn interpolate_env_and_secrets_with_resolutions<F>(
    content: &str,
    source_label: &str,
    secrets: &BTreeMap<String, SecretRef>,
    resolver: F,
) -> Result<(String, BTreeMap<String, String>)>
where
    F: Fn(&str) -> Option<String>,
{
    let mut resolutions: BTreeMap<String, String> = BTreeMap::new();
    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut i = 0usize;
    let mut copy_from = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        out.push_str(&content[copy_from..i]);

        // `$$` escapes a literal `$`. In the combined pass the following
        // `{...}` (if any) is then copied through verbatim, so `$${VAR}` and
        // `$${secret.X}` both survive as literal references.
        if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            out.push('$');
            i += 2;
            copy_from = i;
            continue;
        }

        if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let start = i;
            let body_start = i + 2;
            let Some(close_off) = find_matching_close(&bytes[body_start..]) else {
                let line = line_number_for_offset(content, start);
                return Err(anyhow!(
                    "workflow YAML at {} line {} contains an unterminated `${{` reference",
                    source_label,
                    line
                ));
            };
            let body = &content[body_start..body_start + close_off];
            let resolved = if is_secret_reference(body) {
                let value = resolve_secret_reference(body, source_label, secrets, &resolver, || {
                    line_number_for_offset(content, start)
                })?;
                let key = body.trim().strip_prefix(SECRET_PREFIX).unwrap_or("").trim();
                resolutions.insert(key.to_string(), value.clone());
                value
            } else {
                resolve_reference(body, source_label, &resolver, || line_number_for_offset(content, start))?
            };
            out.push_str(&resolved);
            i = body_start + close_off + 1;
            copy_from = i;
            continue;
        }

        out.push('$');
        i += 1;
        copy_from = i;
    }

    out.push_str(&content[copy_from..]);
    Ok((out, resolutions))
}

/// Scan raw YAML for `${VAR}` references whose env-var name matches a
/// sensitive token pattern (TOKEN | KEY | SECRET | PASSWORD) and that are
/// NOT declared under the `secrets:` block. Returns one human-readable
/// warning per occurrence; the caller decides how to surface them. The
/// scan is best-effort and intentionally non-fatal — authors of trusted
/// YAML may have legitimate uses.
pub fn lint_sensitive_interpolations(content: &str, source_label: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut in_secrets_block = false;
    let mut in_env_block = false;
    let mut env_block_indent: Option<usize> = None;
    let mut secrets_indent: Option<usize> = None;

    for (line_idx, line) in content.lines().enumerate() {
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
            if line_bytes[i] == b'$' && line_bytes[i + 1] == b'{' {
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
    use crate::test_support::{env_lock, EnvVarGuard};

    const KEY: &str = "ANIMUS_TEST_ENV_INTERP_VALUE";
    const OTHER_KEY: &str = "ANIMUS_TEST_ENV_INTERP_OTHER";

    #[test]
    fn expands_required_var() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEY, "secret-token");
        let out = interpolate_env(&format!("api_token: ${{{}}}\n", KEY), "test.yaml").unwrap();
        assert_eq!(out, "api_token: secret-token\n");
    }

    #[test]
    fn errors_clearly_when_required_var_unset() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEY);
        let src = format!("a: 1\nb: 2\napi_token: ${{{}}}\n", KEY);
        let err = interpolate_env(&src, ".animus/workflows/agents.yaml").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("line 3"), "missing line number: {msg}");
        assert!(msg.contains(KEY), "missing var name: {msg}");
        assert!(msg.contains(".animus/workflows/agents.yaml"), "missing source label: {msg}");
    }

    #[test]
    fn uses_default_when_var_unset_with_default_syntax() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEY);
        let out = interpolate_env(&format!("api_url: ${{{}:-https://api.example.com}}\n", KEY), "test.yaml").unwrap();
        assert_eq!(out, "api_url: https://api.example.com\n");
    }

    #[test]
    fn prefers_set_var_over_default() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEY, "https://real.example.com");
        let out =
            interpolate_env(&format!("api_url: ${{{}:-https://fallback.example.com}}\n", KEY), "test.yaml").unwrap();
        assert_eq!(out, "api_url: https://real.example.com\n");
    }

    #[test]
    fn handles_multiple_vars_in_one_line() {
        let _g = env_lock().lock().unwrap();
        let _v1 = EnvVarGuard::set(KEY, "alpha");
        let _v2 = EnvVarGuard::set(OTHER_KEY, "beta");
        let out = interpolate_env(&format!("combo: \"${{{}}}-${{{}}}\"\n", KEY, OTHER_KEY), "test.yaml").unwrap();
        assert_eq!(out, "combo: \"alpha-beta\"\n");
    }

    #[test]
    fn escapes_literal_dollar_with_double_dollar() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEY);
        let out = interpolate_env("price: $$5.00 raw\n", "test.yaml").unwrap();
        assert_eq!(out, "price: $5.00 raw\n");
    }

    #[test]
    fn required_with_custom_message() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEY);
        let src = format!("a: ${{{}:?set this in your shell}}\n", KEY);
        let err = interpolate_env(&src, "test.yaml").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("set this in your shell"), "missing custom message: {msg}");
        assert!(msg.contains(KEY));
    }

    #[test]
    fn required_message_containing_default_token_parses_as_required() {
        let _g = env_lock().lock().unwrap();
        let _set = EnvVarGuard::set(KEY, "present");
        let src = format!("a: ${{{}:?missing key :-(}}\n", KEY);
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, "a: present\n");

        let _unset = EnvVarGuard::unset(KEY);
        let err = interpolate_env(&src, "test.yaml").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("missing key :-("), "missing custom message: {msg}");
        assert!(msg.contains(KEY), "missing var name: {msg}");
    }

    #[test]
    fn default_containing_required_token_parses_as_default() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEY);
        let out = interpolate_env(&format!("a: ${{{}:-fallback :? ok}}\n", KEY), "test.yaml").unwrap();
        assert_eq!(out, "a: fallback :? ok\n");
    }

    #[test]
    fn lone_dollar_passes_through() {
        let _g = env_lock().lock().unwrap();
        let out = interpolate_env("note: this costs $5 in total\n", "test.yaml").unwrap();
        assert_eq!(out, "note: this costs $5 in total\n");
    }

    #[test]
    fn unterminated_reference_errors_with_line() {
        let _g = env_lock().lock().unwrap();
        let src = "ok: yes\nbroken: ${MISSING_BRACE\n";
        let err = interpolate_env(src, "test.yaml").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("line 2"), "missing line: {msg}");
        assert!(msg.contains("unterminated"));
    }

    #[test]
    fn rejects_empty_name() {
        let _g = env_lock().lock().unwrap();
        let err = interpolate_env("a: ${}\n", "test.yaml").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("empty"));
    }

    #[test]
    fn preserves_multibyte_utf8_around_substitution() {
        // Em-dash (U+2014) is 3 bytes in UTF-8 and previously triggered control-character
        // YAML parse errors when the interpolator walked byte-by-byte.
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEY, "expanded");
        let src = format!("note: a — b — ${{{}}}\nemoji: 🚀 — done\n", KEY);
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, "note: a — b — expanded\nemoji: 🚀 — done\n");
    }

    #[test]
    fn rejects_invalid_name() {
        let _g = env_lock().lock().unwrap();
        let err = interpolate_env("a: ${1BAD}\n", "test.yaml").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("must start with"));
    }

    /// Stub resolver used by the keychain-fallback tests below.
    struct StubResolver(std::collections::BTreeMap<String, String>);

    impl WorkflowSecretResolver for StubResolver {
        fn resolve(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    const KEYCHAIN_FALLBACK_KEY: &str = "ANIMUS_TEST_KEYCHAIN_FALLBACK";

    #[test]
    fn falls_back_to_workflow_secret_resolver_when_env_unset() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEYCHAIN_FALLBACK_KEY);
        let mut map = std::collections::BTreeMap::new();
        map.insert(KEYCHAIN_FALLBACK_KEY.to_string(), "from-keychain".to_string());
        install_workflow_secret_resolver_for_test(Arc::new(StubResolver(map)));

        let out = interpolate_env(&format!("token: ${{{}}}\n", KEYCHAIN_FALLBACK_KEY), "test.yaml").unwrap();
        assert_eq!(out, "token: from-keychain\n");

        clear_workflow_secret_resolver_for_test();
    }

    #[test]
    fn env_var_wins_over_workflow_secret_resolver_on_collision() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEYCHAIN_FALLBACK_KEY, "from-env");
        let mut map = std::collections::BTreeMap::new();
        map.insert(KEYCHAIN_FALLBACK_KEY.to_string(), "from-keychain".to_string());
        install_workflow_secret_resolver_for_test(Arc::new(StubResolver(map)));

        let out = interpolate_env(&format!("token: ${{{}}}\n", KEYCHAIN_FALLBACK_KEY), "test.yaml").unwrap();
        assert_eq!(out, "token: from-env\n");

        clear_workflow_secret_resolver_for_test();
    }

    fn secret_map(name: &str, env: &str) -> BTreeMap<String, SecretRef> {
        let mut secrets = BTreeMap::new();
        secrets.insert(name.to_string(), SecretRef { env: env.to_string(), required: true, description: None });
        secrets
    }

    #[test]
    fn combined_pass_does_not_collapse_double_dollar_in_env_values() {
        let resolver = |key: &str| (key == "VAR").then(|| "pa$$word".to_string());
        let out = interpolate_env_and_secrets_with("a: ${VAR}\n", "test.yaml", &BTreeMap::new(), resolver).unwrap();
        assert_eq!(out, "a: pa$$word\n");
    }

    #[test]
    fn combined_pass_accepts_env_values_containing_open_brace_reference() {
        let resolver = |key: &str| (key == "VAR").then(|| "literal ${ inside".to_string());
        let out = interpolate_env_and_secrets_with("a: ${VAR}\n", "test.yaml", &BTreeMap::new(), resolver).unwrap();
        assert_eq!(out, "a: literal ${ inside\n");
    }

    #[test]
    fn combined_pass_does_not_resolve_secret_references_inside_env_values() {
        let resolver = |key: &str| match key {
            "VAR" => Some("${secret.api}".to_string()),
            "REAL_SECRET" => Some("should-not-leak".to_string()),
            _ => None,
        };
        let secrets = secret_map("api", "REAL_SECRET");
        let out = interpolate_env_and_secrets_with("a: ${VAR}\n", "test.yaml", &secrets, resolver).unwrap();
        assert_eq!(out, "a: ${secret.api}\n");
    }

    #[test]
    fn combined_pass_collapses_each_double_dollar_exactly_once() {
        let out = interpolate_env_and_secrets_with("a: $$$$\n", "test.yaml", &BTreeMap::new(), |_| None).unwrap();
        assert_eq!(out, "a: $$\n");
    }

    #[test]
    fn combined_pass_resolves_env_and_secret_references_together() {
        let resolver = |key: &str| match key {
            "VAR" => Some("env-value".to_string()),
            "SECRET_ENV" => Some("secret-value".to_string()),
            _ => None,
        };
        let secrets = secret_map("api", "SECRET_ENV");
        let out =
            interpolate_env_and_secrets_with("a: ${VAR}\nb: ${secret.api}\n", "test.yaml", &secrets, resolver).unwrap();
        assert_eq!(out, "a: env-value\nb: secret-value\n");
    }

    #[test]
    fn combined_pass_preserves_escaped_literal_secret_reference() {
        let resolver = |key: &str| (key == "SECRET_ENV").then(|| "should-not-leak".to_string());
        let secrets = secret_map("api", "SECRET_ENV");
        let out =
            interpolate_env_and_secrets_with("prompt: $${secret.api}\n", "test.yaml", &secrets, resolver).unwrap();
        assert_eq!(out, "prompt: ${secret.api}\n");
    }

    #[test]
    fn required_default_still_applies_when_neither_env_nor_resolver_has_key() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEYCHAIN_FALLBACK_KEY);
        install_workflow_secret_resolver_for_test(Arc::new(StubResolver(std::collections::BTreeMap::new())));

        let out = interpolate_env(
            &format!("url: ${{{}:-https://default.example.com}}\n", KEYCHAIN_FALLBACK_KEY),
            "test.yaml",
        )
        .unwrap();
        assert_eq!(out, "url: https://default.example.com\n");

        clear_workflow_secret_resolver_for_test();
    }
}
