use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliErrorKind {
    InvalidInput,
    NotFound,
    Conflict,
    Unavailable,
    Internal,
}

impl CliErrorKind {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }

    pub(crate) const fn exit_code(self) -> i32 {
        match self {
            Self::InvalidInput => 2,
            Self::NotFound => 3,
            Self::Conflict => 4,
            Self::Unavailable => 5,
            Self::Internal => 1,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CliError {
    kind: CliErrorKind,
    message: String,
}

impl CliError {
    pub(crate) fn new(kind: CliErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) const fn kind(&self) -> CliErrorKind {
        self.kind
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
        if let Some(io_error) = source.downcast_ref::<std::io::Error>() {
            match io_error.kind() {
                std::io::ErrorKind::NotFound => return CliErrorKind::NotFound,
                std::io::ErrorKind::AddrInUse
                | std::io::ErrorKind::AddrNotAvailable
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::TimedOut => return CliErrorKind::Unavailable,
                _ => {}
            }
        }
    }
    CliErrorKind::Internal
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
}
