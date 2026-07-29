#![allow(dead_code)]

use super::CliExecutionResult;
use serde_json::{json, Value};

pub(super) fn extract_cli_success_data(stdout_json: Option<Value>) -> Value {
    stdout_json
        .map(|envelope| match envelope {
            Value::Object(mut map) => map.remove("data").unwrap_or(Value::Object(map)),
            other => other,
        })
        .unwrap_or(Value::Null)
}

/// Pull a normalized `error` value off the first available
/// `animus.cli.v1` envelope, preferring stderr over stdout.
///
/// Error envelopes are written to **stderr** per
/// `docs/reference/json-envelope.md`, so a CLI failure with a structured
/// envelope on stderr (and either nothing or a *success* envelope on stdout)
/// is the canonical shape we need to surface to MCP callers. The pre-fix
/// production path only checked `stdout_json`, which meant a properly-emitted
/// stderr envelope was silently discarded and callers saw `error: null` with
/// just the raw `stderr` blob as a fallback.
///
/// Falls back to `data` only when the envelope lacks an `error` field — that
/// matches what scripted callers historically saw when the underlying CLI
/// emitted a success-shaped envelope but exited non-zero (rare, but the
/// fallback exists so we don't lose information).
fn pick_envelope_error(result: &CliExecutionResult) -> Option<Value> {
    let envelope = result.stderr_json.as_ref().or(result.stdout_json.as_ref())?;
    envelope.get("error").cloned().or_else(|| envelope.get("data").cloned())
}

/// Derive a machine-actionable `remediation` object from the CLI's
/// `animus.cli.v1` error body, for the determinate failure classes.
///
/// Priority order:
///
/// 1. Structured pass-through: typed CLI errors built with
///    `error_with_remediation` carry `error.details.remediation` (e.g. the
///    missing-plugin constructors in `ops_subject` / `ops_queue` and the
///    missing-provider path in `provider_client`). That object is hoisted
///    verbatim — no message scraping.
/// 2. `code == "invalid_input"` (exit 2): the CLI's message is the hint —
///    surface it as `{kind: "invalid_input", help: <message>}`.
/// 3. `code == "unavailable"` (exit 5) whose message names the
///    daemon-not-running condition: `{kind: "daemon_not_running",
///    next_step: "animus daemon start"}`. The match is intentionally
///    narrow (the deterministic phrases our daemon/events constructors
///    emit) so unrelated unavailable errors don't get a misleading fix.
fn remediation_for_error(error: &Value) -> Option<Value> {
    if let Some(remediation) = error.pointer("/details/remediation") {
        if remediation.is_object() {
            return Some(remediation.clone());
        }
    }
    let code = error.get("code").and_then(Value::as_str)?;
    let message = error.get("message").and_then(Value::as_str).unwrap_or_default();
    match code {
        "invalid_input" => Some(json!({ "kind": "invalid_input", "help": message })),
        "unavailable" if mentions_daemon_not_running(message) => {
            Some(json!({ "kind": "daemon_not_running", "next_step": "animus daemon start" }))
        }
        _ => None,
    }
}

fn mentions_daemon_not_running(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("requires a running daemon")
        || lower.contains("daemon is not running")
        || lower.contains("animus daemon start")
}

fn attach_remediation(payload: &mut Value) {
    if let Some(remediation) = payload.get("error").and_then(remediation_for_error) {
        payload["remediation"] = remediation;
    }
}

/// Shape an in-process application error exactly like a failed CLI child
/// command, without requiring a stderr JSON round-trip.
pub(super) fn build_inproc_tool_error_payload(tool_name: &str, err: &anyhow::Error) -> Value {
    let kind = crate::classify_cli_error_kind(err);
    let exit_code = kind.exit_code();
    let mut error = json!({
        "code": kind.code(),
        "message": format!("{err:#}"),
        "exit_code": exit_code,
    });
    if let Some(details) = crate::extract_cli_error_details(err) {
        error["details"] = details;
    }
    let mut payload = json!({
        "tool": tool_name,
        "exit_code": exit_code,
        "error": error,
    });
    attach_remediation(&mut payload);
    payload
}

pub(super) fn build_tool_error_payload(tool_name: &str, result: &CliExecutionResult) -> Value {
    let mut payload = json!({ "tool": tool_name, "exit_code": result.exit_code });
    if let Some(error) = pick_envelope_error(result) {
        payload["error"] = error;
    }
    let stderr = result.stderr.trim().to_string();
    if !stderr.is_empty() {
        payload["stderr"] = json!(stderr);
    }
    attach_remediation(&mut payload);
    payload
}

/// Test-only alias kept for the existing `ops_mcp::tests` coverage. The
/// previous test helper diverged from production by checking `stderr_json`
/// first — that behavior is now the production contract, so this is just a
/// forwarding shim. Inlined here (rather than deleted) so the existing tests
/// keep passing without churn while a new production-path test below proves
/// `build_tool_error_payload` actually reads stderr.
#[cfg(test)]
pub(super) fn build_cli_error_payload(tool_name: &str, result: &CliExecutionResult) -> Value {
    build_tool_error_payload(tool_name, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::CLI_SCHEMA_ID;

    fn failure_with_envelopes(stdout: Option<Value>, stderr: Option<Value>, stderr_text: &str) -> CliExecutionResult {
        CliExecutionResult {
            command: "animus".to_string(),
            args: vec!["--json".to_string()],
            requested_args: vec!["daemon".to_string(), "start".to_string()],
            project_root: "/tmp/project".to_string(),
            exit_code: 5,
            success: false,
            stdout: String::new(),
            stderr: stderr_text.to_string(),
            stdout_json: stdout,
            stderr_json: stderr,
        }
    }

    /// Production-path regression: `build_tool_error_payload` MUST read the
    /// `error` field off the stderr envelope when present. Before this fix
    /// the function only looked at `stdout_json`, so a properly-emitted error
    /// envelope on stderr was silently dropped and MCP callers saw a null
    /// error with just the raw stderr text. The test_only helper that did
    /// the right thing was named `build_cli_error_payload` and was wired up
    /// only behind `#[cfg(test)]`, so the green test suite hid the regression.
    #[test]
    fn build_tool_error_payload_prefers_stderr_envelope_over_stdout_envelope() {
        let result = failure_with_envelopes(
            Some(json!({"schema": CLI_SCHEMA_ID, "ok": false, "error": {"message": "stdout-error"}})),
            Some(json!({"schema": CLI_SCHEMA_ID, "ok": false, "error": {"message": "stderr-error"}})),
            "stderr body",
        );
        let payload = build_tool_error_payload("animus.daemon.start", &result);
        assert_eq!(
            payload.pointer("/error/message").and_then(Value::as_str),
            Some("stderr-error"),
            "production helper must surface the stderr envelope (canonical error channel per the v1 contract)"
        );
        assert_eq!(payload.get("exit_code").and_then(Value::as_i64), Some(5));
        assert_eq!(payload.get("stderr").and_then(Value::as_str), Some("stderr body"));
        assert_eq!(payload.get("tool").and_then(Value::as_str), Some("animus.daemon.start"));
    }

    /// Production-path fallback: when no stderr envelope exists, the stdout
    /// envelope should still be consulted so we don't lose information from
    /// CLIs that historically emitted a success-shaped envelope but exited
    /// non-zero.
    #[test]
    fn build_tool_error_payload_falls_back_to_stdout_envelope_when_stderr_json_missing() {
        let result = failure_with_envelopes(
            Some(json!({"schema": CLI_SCHEMA_ID, "ok": false, "error": {"message": "stdout-error"}})),
            None,
            "",
        );
        let payload = build_tool_error_payload("animus.task.get", &result);
        assert_eq!(payload.pointer("/error/message").and_then(Value::as_str), Some("stdout-error"));
    }

    /// Missing-plugin failures: a typed CLI error built with
    /// `error_with_remediation` carries `error.details.remediation` in the
    /// stderr envelope — the MCP payload must hoist it verbatim, install
    /// command included, with no message scraping.
    #[test]
    fn build_tool_error_payload_hoists_structured_missing_plugin_remediation() {
        let result = failure_with_envelopes(
            None,
            Some(json!({
                "schema": CLI_SCHEMA_ID,
                "ok": false,
                "error": {
                    "code": "unavailable",
                    "message": "subject call 'task/list' failed (-32001): no subject backend mounted for kind 'task'; install one with `animus plugin install-defaults --include-subjects`",
                    "exit_code": 5,
                    "details": {
                        "remediation": {
                            "kind": "missing_plugin",
                            "install_command": "animus plugin install-defaults --include-subjects",
                            "next_step": "Install a subject_backend plugin that serves this kind, then retry.",
                        }
                    }
                }
            })),
            "subject call failed",
        );
        let payload = build_tool_error_payload("animus.subject.list", &result);
        assert_eq!(payload.pointer("/remediation/kind").and_then(Value::as_str), Some("missing_plugin"));
        assert_eq!(
            payload.pointer("/remediation/install_command").and_then(Value::as_str),
            Some("animus plugin install-defaults --include-subjects")
        );
        assert!(
            payload.pointer("/remediation/next_step").and_then(Value::as_str).is_some(),
            "missing_plugin remediation carries a next_step"
        );
    }

    /// Daemon-not-running failures: an `unavailable` error whose message
    /// names the condition gets the `daemon_not_running` remediation even
    /// without structured details.
    #[test]
    fn build_tool_error_payload_classifies_daemon_not_running() {
        let result = failure_with_envelopes(
            None,
            Some(json!({
                "schema": CLI_SCHEMA_ID,
                "ok": false,
                "error": {
                    "code": "unavailable",
                    "message": "animus events tail requires a running daemon (control socket not found). Start one with: animus daemon start",
                    "exit_code": 5,
                }
            })),
            "daemon down",
        );
        let payload = build_tool_error_payload("animus.daemon.events", &result);
        assert_eq!(payload.pointer("/remediation/kind").and_then(Value::as_str), Some("daemon_not_running"));
        assert_eq!(payload.pointer("/remediation/next_step").and_then(Value::as_str), Some("animus daemon start"));
    }

    /// Invalid-input failures (exit 2): the CLI's message is the hint line —
    /// surface it as `help` so agents can self-correct the call.
    #[test]
    fn build_tool_error_payload_classifies_invalid_input() {
        let result = failure_with_envelopes(
            None,
            Some(json!({
                "schema": CLI_SCHEMA_ID,
                "ok": false,
                "error": {
                    "code": "invalid_input",
                    "message": "subject update requires at least one of --status / --priority / --labels",
                    "exit_code": 2,
                }
            })),
            "bad input",
        );
        let payload = build_tool_error_payload("animus.subject.update", &result);
        assert_eq!(payload.pointer("/remediation/kind").and_then(Value::as_str), Some("invalid_input"));
        assert_eq!(
            payload.pointer("/remediation/help").and_then(Value::as_str),
            Some("subject update requires at least one of --status / --priority / --labels")
        );
    }

    #[test]
    fn build_inproc_tool_error_payload_preserves_typed_code_details_and_remediation() {
        let err = crate::error_with_remediation(
            crate::CliErrorKind::Unavailable,
            "subject backend missing",
            crate::missing_plugin_remediation("animus plugin install-defaults", "Install the backend, then retry."),
        );

        let payload = build_inproc_tool_error_payload("animus.subject.list", &err);
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("unavailable"));
        assert_eq!(payload.pointer("/error/exit_code").and_then(Value::as_i64), Some(5));
        assert_eq!(payload.pointer("/remediation/kind").and_then(Value::as_str), Some("missing_plugin"));
        assert_eq!(
            payload.pointer("/error/details/remediation/next_step").and_then(Value::as_str),
            Some("Install the backend, then retry.")
        );
    }

    /// Indeterminate failures must NOT get a remediation guess — an
    /// internal error with no structured details stays remediation-free.
    #[test]
    fn build_tool_error_payload_omits_remediation_for_indeterminate_errors() {
        let result = failure_with_envelopes(
            None,
            Some(json!({
                "schema": CLI_SCHEMA_ID,
                "ok": false,
                "error": { "code": "internal", "message": "store corrupted", "exit_code": 1 }
            })),
            "boom",
        );
        let payload = build_tool_error_payload("animus.subject.get", &result);
        assert!(payload.get("remediation").is_none(), "no remediation for indeterminate errors: {payload}");

        // An unrelated unavailable error (no daemon phrasing) also stays bare.
        let result = failure_with_envelopes(
            None,
            Some(json!({
                "schema": CLI_SCHEMA_ID,
                "ok": false,
                "error": { "code": "unavailable", "message": "request timed out", "exit_code": 5 }
            })),
            "timeout",
        );
        let payload = build_tool_error_payload("animus.subject.get", &result);
        assert!(payload.get("remediation").is_none(), "no daemon guess for generic unavailable: {payload}");
    }
}
