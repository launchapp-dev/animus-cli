pub fn classify_error(message: &str) -> (&'static str, i32) {
    protocol::classify_error_message(message)
}

#[cfg(test)]
mod tests {
    use super::classify_error;

    #[test]
    fn classify_error_covers_cli_parity_patterns() {
        assert_eq!(
            classify_error("required arguments were not provided: --id <ID>"),
            ("invalid_input", 2)
        );
        assert_eq!(
            classify_error("unknown argument '--bogus' found"),
            ("invalid_input", 2)
        );
        assert_eq!(
            classify_error("unrecognized option '--bogus'"),
            ("invalid_input", 2)
        );
        assert_eq!(
            classify_error("No such file or directory (os error 2)"),
            ("not_found", 3)
        );
        assert_eq!(classify_error("record does not exist"), ("not_found", 3));
        assert_eq!(classify_error("resource already exists"), ("conflict", 4));
        assert_eq!(
            classify_error("timeout while waiting for daemon"),
            ("unavailable", 5)
        );
        assert_eq!(
            classify_error("unexpected panic in scheduler loop"),
            ("internal", 1)
        );
    }

    #[test]
    fn classify_error_keeps_protocol_precedence() {
        assert_eq!(
            classify_error("invalid and not found"),
            ("invalid_input", 2)
        );
        assert_eq!(
            classify_error("task not found in unavailable registry"),
            ("not_found", 3)
        );
    }

    #[test]
    fn classify_error_remains_in_protocol_lockstep() {
        let messages = [
            "unknown argument '--bogus' found",
            "unrecognized option '--bogus'",
            "CONFIRMATION_REQUIRED: rerun command with --confirm TASK-1",
            "priority must be one of critical|high|medium|low",
            "task not found in unavailable registry",
            "unexpected panic in scheduler loop",
        ];

        for message in messages {
            assert_eq!(classify_error(message), protocol::classify_error_message(message));
        }
    }
}
