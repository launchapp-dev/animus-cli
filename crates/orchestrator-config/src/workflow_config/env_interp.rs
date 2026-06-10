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
    lookup_env_then_secret_store_tagged(key).map(|resolved| resolved.value)
}

/// A resolved `${VAR}` value tagged with whether it came from the installed
/// secret store (keychain) rather than the process environment. The combined
/// interpolation pass uses the tag to collect keychain-resolved values into
/// the diagnostic redaction map.
pub(crate) struct ResolvedEnvValue {
    pub(crate) value: String,
    pub(crate) from_secret_store: bool,
}

fn lookup_env_then_secret_store_tagged(key: &str) -> Option<ResolvedEnvValue> {
    if let Some(value) = lookup_env(key) {
        return Some(ResolvedEnvValue { value, from_secret_store: false });
    }
    current_workflow_secret_resolver()
        .and_then(|store| store.resolve(key))
        .map(|value| ResolvedEnvValue { value, from_secret_store: true })
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
/// offsets the interpolators walk.
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
    resolve_reference_tagged(
        body,
        source_label,
        &|key: &str| resolver(key).map(|value| ResolvedEnvValue { value, from_secret_store: false }),
        line_of,
    )
    .map(|(value, _)| value)
}

/// Resolve a non-secret `${...}` reference. Returns the substituted value
/// plus `Some(<env var name>)` when the value came from the installed secret
/// store (so the caller can add it to the diagnostic redaction map); values
/// from the process environment or `:-` defaults return `None`.
fn resolve_reference_tagged<F, L>(
    body: &str,
    source_label: &str,
    resolver: &F,
    line_of: L,
) -> Result<(String, Option<String>)>
where
    F: Fn(&str) -> Option<ResolvedEnvValue>,
    L: Fn() -> usize,
{
    fn tag(name: &str, resolved: ResolvedEnvValue) -> (String, Option<String>) {
        let key = resolved.from_secret_store.then(|| name.to_string());
        (resolved.value, key)
    }

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
            Some(resolved) => tag(name, resolved),
            None => (default.to_string(), None),
        });
    }
    if let Some(idx) = required_idx {
        let name = body[..idx].trim();
        validate_name(name, source_label, &line_of)?;
        let message = body[idx + 2..].trim();
        return match resolver(name) {
            Some(resolved) => Ok(tag(name, resolved)),
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
        Some(resolved) => Ok(tag(name, resolved)),
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
    let mut comments = CommentSpans::new(content);
    let mut out = String::with_capacity(content.len());
    let mut i = 0usize;
    let mut copy_from = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'$' || comments.contains(i) {
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

/// Like [`interpolate_env_and_secrets`], but also returns the map of
/// resolved value → redaction label for every substitution whose value came
/// from a secret source: `${secret.<name>}` references (labeled by secret
/// name) and plain `${VAR}` references resolved from the installed secret
/// store / keychain (labeled by env var name). Plain `${VAR}` references
/// resolved from the process environment are not collected. The map is
/// keyed by the resolved VALUE so a secret and a keychain env var sharing a
/// name can never shadow each other's entry; callers use it to redact
/// resolved secret values from any user-visible diagnostics built from the
/// substituted content.
pub(crate) fn interpolate_env_and_secrets_collecting(
    content: &str,
    source_label: &str,
    secrets: &BTreeMap<String, SecretRef>,
) -> Result<(String, BTreeMap<String, String>)> {
    interpolate_env_and_secrets_with_resolutions(content, source_label, secrets, lookup_env_then_secret_store_tagged)
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
    interpolate_env_and_secrets_with_resolutions(content, source_label, secrets, |key| {
        resolver(key).map(|value| ResolvedEnvValue { value, from_secret_store: false })
    })
    .map(|(out, _)| out)
}

fn interpolate_env_and_secrets_with_resolutions<F>(
    content: &str,
    source_label: &str,
    secrets: &BTreeMap<String, SecretRef>,
    resolver: F,
) -> Result<(String, BTreeMap<String, String>)>
where
    F: Fn(&str) -> Option<ResolvedEnvValue>,
{
    let untagged_resolver = |key: &str| resolver(key).map(|resolved| resolved.value);
    let mut resolutions: BTreeMap<String, String> = BTreeMap::new();
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
                let value = resolve_secret_reference(body, source_label, secrets, &untagged_resolver, || {
                    line_number_for_offset(content, start)
                })?;
                let key = body.trim().strip_prefix(SECRET_PREFIX).unwrap_or("").trim();
                resolutions.insert(value.clone(), key.to_string());
                value
            } else {
                let (value, secret_store_key) =
                    resolve_reference_tagged(body, source_label, &resolver, || line_number_for_offset(content, start))?;
                if let Some(key) = secret_store_key {
                    resolutions.insert(value.clone(), key);
                }
                value
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
    fn comment_only_line_with_unset_var_is_left_untouched() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEY);
        let src = format!("# export ${{{}}}\nkey: value\n", KEY);
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn comment_with_invalid_var_name_is_left_untouched() {
        let _g = env_lock().lock().unwrap();
        let src = "# see ${docs-url}\nkey: value\n";
        let out = interpolate_env(src, "test.yaml").unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn trailing_comment_after_value_is_left_untouched() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEY, "expanded");
        let src = format!("key: ${{{KEY}}} # docs: ${{UNSET_IN_COMMENT}}\n");
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, "key: expanded # docs: ${UNSET_IN_COMMENT}\n");
    }

    #[test]
    fn hash_inside_quoted_scalar_still_interpolates() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEY, "expanded");
        let out = interpolate_env(&format!("key: \"#not-a-comment ${{{}}}\"\n", KEY), "test.yaml").unwrap();
        assert_eq!(out, "key: \"#not-a-comment expanded\"\n");
    }

    #[test]
    fn hash_heading_inside_block_scalar_still_interpolates() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEY, "expanded");
        let src = format!("prompt: |\n  # Heading ${{{}}}\n  body\nkey: value\n", KEY);
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, "prompt: |\n  # Heading expanded\n  body\nkey: value\n");
    }

    #[test]
    fn comment_after_block_scalar_content_is_left_untouched() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEY);
        let src = format!("prompt: |\n  body text\n# note ${{{}}}\nkey: value\n", KEY);
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn unpaired_quote_in_plain_scalar_does_not_suppress_trailing_comment() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEY);
        let src = format!("directive: Build \"docs # see ${{{}}}\n", KEY);
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn quote_after_indicator_still_opens_quoted_scalar() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEY, "expanded");
        let src = format!("items: [a, \"#tag ${{{}}}\"]\n", KEY);
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, "items: [a, \"#tag expanded\"]\n");
    }

    #[test]
    fn quote_after_anchor_still_opens_quoted_scalar() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEY, "expanded");
        let src = format!("directive: &d \"build # ${{{}}}\"\n", KEY);
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, "directive: &d \"build # expanded\"\n");
    }

    #[test]
    fn quote_after_tag_still_opens_quoted_scalar() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEY, "expanded");
        let src = format!("directive: !!str \"build # ${{{}}}\"\n", KEY);
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, "directive: !!str \"build # expanded\"\n");
    }

    #[test]
    fn apostrophe_in_plain_scalar_does_not_suppress_later_comment() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEY);
        let src = format!("note: it's fine\n# export ${{{}}}\n", KEY);
        let out = interpolate_env(&src, "test.yaml").unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn combined_pass_leaves_comment_references_untouched() {
        let src = "# setup: ${UNSET_VAR} and ${secret.undeclared}\nkey: value\n";
        let out = interpolate_env_and_secrets_with(src, "test.yaml", &BTreeMap::new(), |_| None).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn lint_skips_sensitive_looking_references_in_comments() {
        let src = "# export ${LINEAR_TOKEN}\nurl: ${TEAM_URL:-https://example.com}\n";
        let warnings = lint_sensitive_interpolations(src, "test.yaml");
        assert!(warnings.is_empty(), "comment-only reference should not warn: {warnings:?}");
    }

    #[test]
    fn collecting_pass_collects_keychain_resolved_env_values() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::unset(KEYCHAIN_FALLBACK_KEY);
        let mut map = std::collections::BTreeMap::new();
        map.insert(KEYCHAIN_FALLBACK_KEY.to_string(), "keychain-token-value".to_string());
        install_workflow_secret_resolver_for_test(Arc::new(StubResolver(map)));

        let src = format!("token: ${{{}}}\n", KEYCHAIN_FALLBACK_KEY);
        let (out, resolutions) = interpolate_env_and_secrets_collecting(&src, "test.yaml", &BTreeMap::new()).unwrap();
        assert_eq!(out, "token: keychain-token-value\n");
        assert_eq!(resolutions.get("keychain-token-value").map(|s| s.as_str()), Some(KEYCHAIN_FALLBACK_KEY));

        clear_workflow_secret_resolver_for_test();
    }

    #[test]
    fn collecting_pass_keeps_both_values_when_secret_and_keychain_names_collide() {
        let resolver = |key: &str| match key {
            "SECRET_ENV" => Some(ResolvedEnvValue { value: "first-secret-value".to_string(), from_secret_store: true }),
            "TOKEN" => Some(ResolvedEnvValue { value: "second-keychain-value".to_string(), from_secret_store: true }),
            _ => None,
        };
        let secrets = secret_map("TOKEN", "SECRET_ENV");
        let (out, resolutions) = interpolate_env_and_secrets_with_resolutions(
            "a: ${secret.TOKEN}\nb: ${TOKEN}\n",
            "test.yaml",
            &secrets,
            resolver,
        )
        .unwrap();
        assert_eq!(out, "a: first-secret-value\nb: second-keychain-value\n");
        assert_eq!(resolutions.get("first-secret-value").map(|s| s.as_str()), Some("TOKEN"));
        assert_eq!(resolutions.get("second-keychain-value").map(|s| s.as_str()), Some("TOKEN"));
    }

    #[test]
    fn collecting_pass_does_not_collect_process_env_values() {
        let _g = env_lock().lock().unwrap();
        let _v = EnvVarGuard::set(KEYCHAIN_FALLBACK_KEY, "from-env");
        let mut map = std::collections::BTreeMap::new();
        map.insert(KEYCHAIN_FALLBACK_KEY.to_string(), "from-keychain".to_string());
        install_workflow_secret_resolver_for_test(Arc::new(StubResolver(map)));

        let src = format!("token: ${{{}}}\n", KEYCHAIN_FALLBACK_KEY);
        let (out, resolutions) = interpolate_env_and_secrets_collecting(&src, "test.yaml", &BTreeMap::new()).unwrap();
        assert_eq!(out, "token: from-env\n");
        assert!(resolutions.is_empty(), "process-env resolutions must not be collected: {resolutions:?}");

        clear_workflow_secret_resolver_for_test();
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
