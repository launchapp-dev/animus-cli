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
    "GEMINI_API_KEY",
    // Claude CLI configuration
    "CLAUDE_CODE_SETTINGS_PATH",
    "CLAUDE_API_KEY",
    "CLAUDE_CODE_DIR",
];

pub fn sanitize_env() -> HashMap<String, String> {
    sanitize_env_with_extra_vars(&[])
}

pub fn sanitize_env_with_extra_vars(extra_allowed_vars: &[String]) -> HashMap<String, String> {
    let mut sanitized = HashMap::new();

    for var in ALLOWED_ENV_VARS {
        if let Ok(value) = env::var(var) {
            sanitized.insert(var.to_string(), value);
        }
    }

    for var in extra_allowed_vars
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
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

    #[test]
    fn sanitize_env_includes_requested_extra_vars() {
        std::env::set_var("AO_EXTRA_KEY", "test-value");
        let env = sanitize_env_with_extra_vars(&["AO_EXTRA_KEY".to_string()]);
        assert_eq!(
            env.get("AO_EXTRA_KEY").map(String::as_str),
            Some("test-value")
        );
        std::env::remove_var("AO_EXTRA_KEY");
    }
}
