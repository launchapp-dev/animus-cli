use std::collections::HashMap;
use std::env;

const ALLOWED_ENV_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    // API keys
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GOOGLE_API_KEY",
    // Claude CLI configuration
    "CLAUDE_CODE_SETTINGS_PATH",
    "CLAUDE_API_KEY",
    "CLAUDE_CODE_DIR",
];

pub fn sanitize_env() -> HashMap<String, String> {
    let mut sanitized = HashMap::new();

    for var in ALLOWED_ENV_VARS {
        if let Ok(value) = env::var(var) {
            sanitized.insert(var.to_string(), value);
        }
    }

    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_env() {
        let env = sanitize_env();
        assert!(env.contains_key("PATH"));
    }
}
