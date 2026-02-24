pub fn classify_error(message: &str) -> (&'static str, i32) {
    let normalized = message.to_ascii_lowercase();

    if normalized.contains("invalid")
        || normalized.contains("parse")
        || normalized.contains("missing required")
        || normalized.contains("must be")
    {
        return ("invalid_input", 2);
    }

    if normalized.contains("not found") {
        return ("not_found", 3);
    }

    if normalized.contains("already") || normalized.contains("conflict") {
        return ("conflict", 4);
    }

    if normalized.contains("timed out")
        || normalized.contains("connection")
        || normalized.contains("unavailable")
        || normalized.contains("failed to connect")
    {
        return ("unavailable", 5);
    }

    ("internal", 1)
}
