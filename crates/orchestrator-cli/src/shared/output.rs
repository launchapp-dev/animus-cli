use anyhow::Result;
use protocol::CLI_SCHEMA_ID;
use serde::Serialize;
use serde_json::{json, Value};

use super::{classify_cli_error_kind, CliErrorKind};

const CLI_SCHEMA: &str = CLI_SCHEMA_ID;

#[derive(Debug, Serialize)]
struct CliSuccessEnvelope<T: Serialize> {
    schema: &'static str,
    ok: bool,
    data: T,
}

#[derive(Debug, Serialize)]
struct CliErrorBody {
    code: String,
    message: String,
    exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct CliErrorEnvelope {
    schema: &'static str,
    ok: bool,
    error: CliErrorBody,
}

pub(crate) fn print_ok(message: &str, json: bool) {
    if json {
        let envelope =
            CliSuccessEnvelope { schema: CLI_SCHEMA, ok: true, data: serde_json::json!({ "message": message }) };
        println!(
            "{}",
            serialize_compact_json(&envelope).unwrap_or_else(|_| {
                format!("{{\"schema\":\"{}\",\"ok\":true,\"data\":{{\"message\":\"ok\"}}}}", CLI_SCHEMA_ID)
            })
        );
    } else {
        println!("{message}");
    }
}

pub(crate) fn print_value<T: serde::Serialize>(value: T, json: bool) -> Result<()> {
    if json {
        let envelope = CliSuccessEnvelope { schema: CLI_SCHEMA, ok: true, data: value };
        println!("{}", serialize_compact_json(&envelope)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }

    Ok(())
}

pub(crate) fn classify_error(err: &anyhow::Error) -> (&'static str, i32) {
    let kind = classify_cli_error_kind(err);
    (kind.code(), kind.exit_code())
}

pub(crate) fn classify_exit_code(err: &anyhow::Error) -> i32 {
    classify_error(err).1
}

pub(crate) fn emit_cli_error(err: &anyhow::Error, json: bool) {
    let kind = classify_cli_error_kind(err);
    let code = kind.code();
    let exit_code = kind.exit_code();
    if json {
        let details = super::extract_cli_error_details(err);
        let envelope = CliErrorEnvelope {
            schema: CLI_SCHEMA,
            ok: false,
            error: CliErrorBody { code: code.to_string(), message: err.to_string(), exit_code, details },
        };
        eprintln!("{}", serialize_compact_json(&envelope).unwrap_or_else(|_| {
            format!("{{\"schema\":\"{}\",\"ok\":false,\"error\":{{\"code\":\"internal\",\"message\":\"serialization failure\",\"exit_code\":1}}}}", CLI_SCHEMA_ID)
        }));
    } else {
        eprintln!("error: {}", err);
        if kind == CliErrorKind::InvalidInput && should_emit_help_hint(&err.to_string()) {
            eprintln!("hint: run with --help to view accepted arguments and values");
        }
    }
}

fn should_emit_help_hint(message: &str) -> bool {
    !message.to_ascii_lowercase().contains("--help")
}

fn serialize_compact_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

pub(crate) fn dry_run_envelope(
    operation: &str,
    target: Value,
    action: &str,
    effects: Vec<String>,
    confirm_hint: &str,
) -> Value {
    json!({
        "operation": operation,
        "target": target,
        "action": action,
        "dry_run": true,
        "destructive": true,
        "requires_confirmation": true,
        "planned_effects": effects,
        "next_step": confirm_hint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{conflict_error, invalid_input_error, not_found_error, unavailable_error};
    use anyhow::anyhow;

    #[test]
    fn classify_error_marks_typed_invalid_input_failures() {
        let (code, exit_code) = classify_error(&invalid_input_error("invalid flag value"));
        assert_eq!(code, "invalid_input");
        assert_eq!(exit_code, 2);
    }

    #[test]
    fn classify_error_uses_typed_kind_when_message_contains_not_found_text() {
        let (code, exit_code) =
            classify_error(&invalid_input_error("task not found: TASK-123; expected --id TASK-123"));
        assert_eq!(code, "invalid_input");
        assert_eq!(exit_code, 2);
    }

    #[test]
    fn classify_error_marks_typed_not_found_failures() {
        let (code, exit_code) = classify_error(&not_found_error("task not found: TASK-123"));
        assert_eq!(code, "not_found");
        assert_eq!(exit_code, 3);
    }

    #[test]
    fn classify_error_marks_io_not_found_failures_without_message_matching() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "missing file");
        let (code, exit_code) = classify_error(&anyhow::Error::from(io_error));
        assert_eq!(code, "not_found");
        assert_eq!(exit_code, 3);
    }

    #[test]
    fn classify_error_marks_typed_conflicts() {
        let (code, exit_code) = classify_error(&conflict_error("resource already exists"));
        assert_eq!(code, "conflict");
        assert_eq!(exit_code, 4);
    }

    #[test]
    fn classify_error_marks_typed_unavailable_connectivity_paths() {
        let (code, exit_code) = classify_error(&unavailable_error("failed to connect to daemon"));
        assert_eq!(code, "unavailable");
        assert_eq!(exit_code, 5);
    }

    #[test]
    fn classify_error_defaults_to_internal_for_untyped_errors() {
        let (code, exit_code) = classify_error(&anyhow!("unexpected panic in scheduler loop"));
        assert_eq!(code, "internal");
        assert_eq!(exit_code, 1);
    }

    #[test]
    fn classify_error_uses_string_fallback_for_untyped_errors_with_conflict_keywords() {
        let (code, exit_code) = classify_error(&anyhow!("resource already exists but no typed conflict was attached"));
        assert_eq!(code, "conflict");
        assert_eq!(exit_code, 4);
    }

    #[test]
    fn classify_error_keeps_exit_code_stable_when_typed_message_changes() {
        let short = invalid_input_error("invalid value");
        let long = invalid_input_error("invalid priority '<empty>'; expected one of: critical|high|medium|low");
        assert_eq!(classify_exit_code(&short), 2);
        assert_eq!(classify_exit_code(&long), 2);
    }

    #[test]
    fn should_emit_help_hint_is_case_insensitive() {
        assert!(!should_emit_help_hint("Run with --HELP for usage"));
        assert!(should_emit_help_hint("invalid priority value"));
    }

    #[test]
    fn serialize_compact_json_omits_pretty_print_whitespace() {
        let payload = json!({
            "schema": CLI_SCHEMA_ID,
            "ok": true,
            "data": { "message": "ok" }
        });
        let serialized = serialize_compact_json(&payload).expect("json should serialize");
        assert!(!serialized.contains('\n'));
        assert_eq!(serialized, r#"{"data":{"message":"ok"},"ok":true,"schema":"ao.cli.v1"}"#);
    }
}
