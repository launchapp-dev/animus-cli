use anyhow::Result;
use serde::Serialize;

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
            serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{\"ok\":true}".to_string())
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
    let message = err.to_string().to_ascii_lowercase();
    if message.contains("invalid")
        || message.contains("parse")
        || message.contains("missing required")
        || message.contains("required arguments were not provided")
        || message.contains("unexpected argument")
        || message.contains("unknown argument")
        || message.contains("unrecognized option")
        || message.contains("confirmation_required")
        || message.contains("must be")
    {
        return ("invalid_input", 2);
    }
    if message.contains("not found") {
        return ("not_found", 3);
    }
    if message.contains("already") || message.contains("conflict") {
        return ("conflict", 4);
    }
    if message.contains("timed out")
        || message.contains("connection")
        || message.contains("unavailable")
        || message.contains("failed to connect")
    {
        return ("unavailable", 5);
    }

    ("internal", 1)
}

pub(crate) fn classify_exit_code(err: &anyhow::Error) -> i32 {
    classify_error(err).1
}

pub(crate) fn emit_cli_error(err: &anyhow::Error, json: bool) {
    let (code, exit_code) = classify_error(err);
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
        if code == "invalid_input" && !err.to_string().contains("--help") {
            eprintln!("hint: run with --help to view accepted arguments and values");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn classify_error_marks_validation_failures_as_invalid_input() {
        let (code, exit_code) =
            classify_error(&anyhow!("missing required arguments were not provided"));
        assert_eq!(code, "invalid_input");
        assert_eq!(exit_code, 2);
    }

    #[test]
    fn classify_error_marks_not_found_failures() {
        let (code, exit_code) = classify_error(&anyhow!("task not found: TASK-123"));
        assert_eq!(code, "not_found");
        assert_eq!(exit_code, 3);
    }

    #[test]
    fn classify_error_marks_conflicts() {
        let (code, exit_code) = classify_error(&anyhow!("resource already exists"));
        assert_eq!(code, "conflict");
        assert_eq!(exit_code, 4);
    }

    #[test]
    fn classify_error_marks_unavailable_connectivity_paths() {
        let (code, exit_code) = classify_error(&anyhow!("failed to connect to daemon"));
        assert_eq!(code, "unavailable");
        assert_eq!(exit_code, 5);
    }

    #[test]
    fn classify_error_defaults_to_internal() {
        let (code, exit_code) = classify_error(&anyhow!("unexpected panic in scheduler loop"));
        assert_eq!(code, "internal");
        assert_eq!(exit_code, 1);
    }
}
