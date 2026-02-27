use anyhow::Result;
use serde::Serialize;

use super::{classify_cli_error_kind, CliErrorKind};

const CLI_SCHEMA: &str = "ao.cli.v1";

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
}

#[derive(Debug, Serialize)]
struct CliErrorEnvelope {
    schema: &'static str,
    ok: bool,
    error: CliErrorBody,
}

pub(crate) fn print_ok(message: &str, json: bool) {
    if json {
        let envelope = CliSuccessEnvelope {
            schema: CLI_SCHEMA,
            ok: true,
            data: serde_json::json!({ "message": message }),
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| {
                "{\"schema\":\"ao.cli.v1\",\"ok\":true,\"data\":{\"message\":\"ok\"}}".to_string()
            })
        );
    } else {
        println!("{message}");
    }
}

pub(crate) fn print_value<T: serde::Serialize>(value: T, json: bool) -> Result<()> {
    if json {
        let envelope = CliSuccessEnvelope {
            schema: CLI_SCHEMA,
            ok: true,
            data: value,
        };
        println!("{}", serde_json::to_string_pretty(&envelope)?);
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
        let envelope = CliErrorEnvelope {
            schema: CLI_SCHEMA,
            ok: false,
            error: CliErrorBody {
                code: code.to_string(),
                message: err.to_string(),
                exit_code,
            },
        };
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| {
                "{\"schema\":\"ao.cli.v1\",\"ok\":false,\"error\":{\"code\":\"internal\",\"message\":\"serialization failure\",\"exit_code\":1}}".to_string()
            })
        );
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
    fn classify_error_keeps_exit_code_stable_when_typed_message_changes() {
        let short = invalid_input_error("invalid value");
        let long = invalid_input_error(
            "invalid priority '<empty>'; expected one of: critical|high|medium|low",
        );
        assert_eq!(classify_exit_code(&short), 2);
        assert_eq!(classify_exit_code(&long), 2);
    }

    #[test]
    fn should_emit_help_hint_is_case_insensitive() {
        assert!(!should_emit_help_hint("Run with --HELP for usage"));
        assert!(should_emit_help_hint("invalid priority value"));
    }
}
