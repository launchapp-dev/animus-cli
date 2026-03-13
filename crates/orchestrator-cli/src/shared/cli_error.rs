use std::fmt::{Display, Formatter};

use protocol::ErrorKind;

pub(crate) type CliErrorKind = ErrorKind;

#[derive(Debug)]
pub(crate) struct CliError {
    kind: CliErrorKind,
    message: String,
    details: Option<serde_json::Value>,
}

impl CliError {
    pub(crate) fn new(kind: CliErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    pub(crate) fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub(crate) const fn kind(&self) -> CliErrorKind {
        self.kind
    }

    pub(crate) fn details(&self) -> Option<&serde_json::Value> {
        self.details.as_ref()
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
        let not_found = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing file",
        ));
        let unavailable = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "runner down",
        ));

        assert_eq!(classify_cli_error_kind(&not_found), CliErrorKind::NotFound);
        assert_eq!(
            classify_cli_error_kind(&unavailable),
            CliErrorKind::Unavailable
        );
    }

    #[test]
    fn extract_cli_error_details_returns_attached_details() {
        let err: anyhow::Error = CliError::new(CliErrorKind::Internal, "daemon failed")
            .with_details(serde_json::json!({"startup_log_tail": "error: panic"}))
            .into();
        let details = extract_cli_error_details(&err).expect("details should be present");
        assert_eq!(
            details
                .get("startup_log_tail")
                .and_then(serde_json::Value::as_str),
            Some("error: panic")
        );
    }

    #[test]
    fn extract_cli_error_details_returns_none_when_absent() {
        let err: anyhow::Error = CliError::new(CliErrorKind::Internal, "plain error").into();
        assert!(extract_cli_error_details(&err).is_none());
    }
}
