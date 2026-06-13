use std::fmt::{Display, Formatter};

use protocol::ErrorKind;

pub(crate) type CliErrorKind = ErrorKind;

#[derive(Debug)]
pub(crate) struct CliError {
    kind: CliErrorKind,
    message: String,
    details: Option<serde_json::Value>,
    exit_only: bool,
}

impl CliError {
    pub(crate) fn new(kind: CliErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), details: None, exit_only: false }
    }

    pub(crate) fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub(crate) fn exit_only(mut self) -> Self {
        self.exit_only = true;
        self
    }

    pub(crate) const fn kind(&self) -> CliErrorKind {
        self.kind
    }

    pub(crate) fn details(&self) -> Option<&serde_json::Value> {
        self.details.as_ref()
    }

    pub(crate) const fn is_exit_only(&self) -> bool {
        self.exit_only
    }
}

impl Display for CliError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

pub(crate) fn invalid_input_error(message: impl Into<String>) -> anyhow::Error {
    CliError::new(CliErrorKind::InvalidInput, message).into()
}

pub(crate) fn not_found_error(message: impl Into<String>) -> anyhow::Error {
    CliError::new(CliErrorKind::NotFound, message).into()
}

pub(crate) fn conflict_error(message: impl Into<String>) -> anyhow::Error {
    CliError::new(CliErrorKind::Conflict, message).into()
}

pub(crate) fn unavailable_error(message: impl Into<String>) -> anyhow::Error {
    CliError::new(CliErrorKind::Unavailable, message).into()
}

pub(crate) fn internal_error(message: impl Into<String>) -> anyhow::Error {
    CliError::new(CliErrorKind::Internal, message).into()
}

/// A typed error whose only job is to set a non-zero process exit code. The
/// command has already rendered its full output (e.g. `animus doctor` printed
/// its report / JSON envelope), so `emit_cli_error` must NOT print a second
/// error envelope for it — doing so would contradict the success envelope
/// already on stdout. The `message` is still available for human stderr.
pub(crate) fn exit_only_error(kind: CliErrorKind, message: impl Into<String>) -> anyhow::Error {
    CliError::new(kind, message).exit_only().into()
}

/// True when `err` is an [`exit_only_error`] — the dispatcher should set the
/// exit code from it but suppress any error-envelope emission.
pub(crate) fn is_exit_only_error(err: &anyhow::Error) -> bool {
    err.chain().any(|source| source.downcast_ref::<CliError>().is_some_and(CliError::is_exit_only))
}

/// Structured remediation payload for "a required plugin is not installed"
/// failures. Carried under `error.details.remediation` in the
/// `animus.cli.v1` envelope so machine callers (notably the MCP server's
/// tool-error payloads) can surface the exact install command without
/// scraping the human-readable message.
pub(crate) fn missing_plugin_remediation(
    install_command: impl Into<String>,
    next_step: impl Into<String>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "missing_plugin",
        "install_command": install_command.into(),
        "next_step": next_step.into(),
    })
}

/// Structured remediation payload for "this command needs a running daemon"
/// failures.
pub(crate) fn daemon_not_running_remediation() -> serde_json::Value {
    serde_json::json!({
        "kind": "daemon_not_running",
        "next_step": "animus daemon start",
    })
}

/// Build a typed [`CliError`] that carries a structured `remediation`
/// object in its details. The human-readable `message` is unchanged from
/// what the plain constructors would produce — this only adds structure.
pub(crate) fn error_with_remediation(
    kind: CliErrorKind,
    message: impl Into<String>,
    remediation: serde_json::Value,
) -> anyhow::Error {
    CliError::new(kind, message).with_details(serde_json::json!({ "remediation": remediation })).into()
}

pub(crate) fn classify_cli_error_kind(err: &anyhow::Error) -> CliErrorKind {
    for source in err.chain() {
        if let Some(cli_error) = source.downcast_ref::<CliError>() {
            return cli_error.kind();
        }
    }
    protocol::classify_anyhow_error_kind(err)
}

pub(crate) fn extract_cli_error_details(err: &anyhow::Error) -> Option<serde_json::Value> {
    for source in err.chain() {
        if let Some(cli_error) = source.downcast_ref::<CliError>() {
            return cli_error.details().cloned();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn cli_error_kind_maps_to_expected_codes_and_exit_codes() {
        let cases = [
            (CliErrorKind::InvalidInput, "invalid_input", 2),
            (CliErrorKind::NotFound, "not_found", 3),
            (CliErrorKind::Conflict, "conflict", 4),
            (CliErrorKind::Unavailable, "unavailable", 5),
            (CliErrorKind::Internal, "internal", 1),
        ];

        for (kind, code, exit_code) in cases {
            assert_eq!(kind.code(), code);
            assert_eq!(kind.exit_code(), exit_code);
        }
    }

    #[test]
    fn classify_cli_error_kind_reads_wrapped_typed_errors() {
        let err = Err::<(), anyhow::Error>(not_found_error("workflow missing"))
            .context("outer context")
            .expect_err("typed error should remain discoverable in chain");
        assert_eq!(classify_cli_error_kind(&err), CliErrorKind::NotFound);
    }

    #[test]
    fn classify_cli_error_kind_maps_io_error_kinds_without_message_matching() {
        let not_found = anyhow::Error::from(std::io::Error::new(std::io::ErrorKind::NotFound, "missing file"));
        let unavailable =
            anyhow::Error::from(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "runner down"));

        assert_eq!(classify_cli_error_kind(&not_found), CliErrorKind::NotFound);
        assert_eq!(classify_cli_error_kind(&unavailable), CliErrorKind::Unavailable);
    }

    #[test]
    fn extract_cli_error_details_returns_attached_details() {
        let err: anyhow::Error = CliError::new(CliErrorKind::Internal, "daemon failed")
            .with_details(serde_json::json!({"startup_log_tail": "error: panic"}))
            .into();
        let details = extract_cli_error_details(&err).expect("details should be present");
        assert_eq!(details.get("startup_log_tail").and_then(serde_json::Value::as_str), Some("error: panic"));
    }

    #[test]
    fn error_with_remediation_keeps_kind_and_carries_structured_payload() {
        let err = error_with_remediation(
            CliErrorKind::Unavailable,
            "no subject backend mounted",
            missing_plugin_remediation("animus plugin install-defaults --include-subjects", "Install, then retry."),
        );
        assert_eq!(classify_cli_error_kind(&err), CliErrorKind::Unavailable);
        let details = extract_cli_error_details(&err).expect("details present");
        assert_eq!(details.pointer("/remediation/kind").and_then(serde_json::Value::as_str), Some("missing_plugin"));
        assert_eq!(
            details.pointer("/remediation/install_command").and_then(serde_json::Value::as_str),
            Some("animus plugin install-defaults --include-subjects")
        );

        let daemon = daemon_not_running_remediation();
        assert_eq!(daemon.get("kind").and_then(serde_json::Value::as_str), Some("daemon_not_running"));
        assert_eq!(daemon.get("next_step").and_then(serde_json::Value::as_str), Some("animus daemon start"));
    }

    #[test]
    fn extract_cli_error_details_returns_none_when_absent() {
        let err: anyhow::Error = CliError::new(CliErrorKind::Internal, "plain error").into();
        assert!(extract_cli_error_details(&err).is_none());
    }
}
