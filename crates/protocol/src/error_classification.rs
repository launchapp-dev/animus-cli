use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidInput,
    NotFound,
    Conflict,
    Unavailable,
    Internal,
}

impl ErrorKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }

    pub const fn exit_code(self) -> i32 {
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
pub struct ClassifiedError {
    kind: ErrorKind,
    message: String,
}

impl ClassifiedError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl Display for ClassifiedError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ClassifiedError {}

pub fn classify_anyhow_error_kind(err: &anyhow::Error) -> ErrorKind {
    for source in err.chain() {
        if let Some(classified) = source.downcast_ref::<ClassifiedError>() {
            return classified.kind();
        }
        if let Some(io_error) = source.downcast_ref::<std::io::Error>() {
            match io_error.kind() {
                std::io::ErrorKind::NotFound => return ErrorKind::NotFound,
                std::io::ErrorKind::AddrInUse
                | std::io::ErrorKind::AddrNotAvailable
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::TimedOut => return ErrorKind::Unavailable,
                _ => {}
            }
        }
    }
    let (code, _) = classify_error_message(&err.to_string());
    match code {
        "invalid_input" => ErrorKind::InvalidInput,
        "not_found" => ErrorKind::NotFound,
        "conflict" => ErrorKind::Conflict,
        "unavailable" => ErrorKind::Unavailable,
        _ => ErrorKind::Internal,
    }
}

const INVALID_INPUT_PATTERNS: &[&str] = &[
    "invalid",
    "parse",
    "missing required",
    "required arguments were not provided",
    "unexpected argument",
    "unknown argument",
    "unrecognized option",
    "confirmation_required",
    "must be",
];
const NOT_FOUND_PATTERNS: &[&str] = &["not found", "no such file or directory", "does not exist"];
const CONFLICT_PATTERNS: &[&str] = &["already", "conflict"];
const UNAVAILABLE_PATTERNS: &[&str] = &[
    "timed out",
    "timeout",
    "connection",
    "unavailable",
    "failed to connect",
];

pub fn classify_error_message(message: &str) -> (&'static str, i32) {
    let normalized = message.to_ascii_lowercase();
    if contains_any(&normalized, INVALID_INPUT_PATTERNS) {
        return ("invalid_input", 2);
    }
    if contains_any(&normalized, NOT_FOUND_PATTERNS) {
        return ("not_found", 3);
    }
    if contains_any(&normalized, CONFLICT_PATTERNS) {
        return ("conflict", 4);
    }
    if contains_any(&normalized, UNAVAILABLE_PATTERNS) {
        return ("unavailable", 5);
    }

    ("internal", 1)
}

fn contains_any(message: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| message.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::classify_error_message;

    #[test]
    fn classify_error_message_marks_invalid_inputs() {
        assert_eq!(
            classify_error_message("required arguments were not provided: --id"),
            ("invalid_input", 2)
        );
    }

    #[test]
    fn classify_error_message_marks_not_found_paths() {
        assert_eq!(
            classify_error_message("No such file or directory (os error 2)"),
            ("not_found", 3)
        );
        assert_eq!(
            classify_error_message("task does not exist"),
            ("not_found", 3)
        );
    }

    #[test]
    fn classify_error_message_covers_cli_pattern_set() {
        let cases = [
            ("unknown argument '--bogus' found", ("invalid_input", 2)),
            ("unrecognized option '--bogus'", ("invalid_input", 2)),
            (
                "CONFIRMATION_REQUIRED: rerun command with --confirm TASK-1",
                ("invalid_input", 2),
            ),
            (
                "priority must be one of critical|high|medium|low",
                ("invalid_input", 2),
            ),
            ("resource already exists", ("conflict", 4)),
            ("failed to connect to daemon", ("unavailable", 5)),
        ];

        for (message, expected) in cases {
            assert_eq!(classify_error_message(message), expected, "{message}");
        }
    }

    #[test]
    fn classify_error_message_marks_conflicts() {
        assert_eq!(
            classify_error_message("resource already exists"),
            ("conflict", 4)
        );
    }

    #[test]
    fn classify_error_message_marks_unavailable_paths() {
        assert_eq!(
            classify_error_message("timeout while waiting for daemon"),
            ("unavailable", 5)
        );
    }

    #[test]
    fn classify_error_message_keeps_precedence_order() {
        assert_eq!(
            classify_error_message("invalid and not found"),
            ("invalid_input", 2)
        );
        assert_eq!(
            classify_error_message("task not found in unavailable registry"),
            ("not_found", 3)
        );
    }

    #[test]
    fn classify_error_message_defaults_to_internal() {
        assert_eq!(
            classify_error_message("unexpected panic in scheduler loop"),
            ("internal", 1)
        );
    }
}
