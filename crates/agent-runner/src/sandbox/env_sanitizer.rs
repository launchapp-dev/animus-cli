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
    // Terminal/agent context
    "TERM",
    "COLORTERM",
    "SSH_AUTH_SOCK",
    // API keys
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    // Claude CLI configuration
    "CLAUDE_CODE_SETTINGS_PATH",
    "CLAUDE_API_KEY",
    "CLAUDE_CODE_DIR",
];

const ALLOWED_ENV_PREFIXES: &[&str] = &["AO_", "XDG_"];

pub fn sanitize_env() -> HashMap<String, String> {
    let mut sanitized = HashMap::new();

    for var in ALLOWED_ENV_VARS {
        if let Ok(value) = env::var(var) {
            sanitized.insert(var.to_string(), value);
        }
    }

    for (var, value) in env::vars() {
        if ALLOWED_ENV_PREFIXES
            .iter()
            .any(|prefix| var.starts_with(prefix))
        {
            sanitized.insert(var, value);
        }
    }

    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock should be available")
    }

    #[test]
    fn forwards_new_explicit_allowlist_entries() {
        let _lock = env_lock();
        let _gemini = EnvVarGuard::set("GEMINI_API_KEY", Some("gemini-test-key"));
        let _google = EnvVarGuard::set("GOOGLE_API_KEY", Some("google-test-key"));
        let _term = EnvVarGuard::set("TERM", Some("xterm-256color"));
        let _colorterm = EnvVarGuard::set("COLORTERM", Some("truecolor"));
        let _ssh = EnvVarGuard::set("SSH_AUTH_SOCK", Some("/tmp/test-agent.sock"));

        let env = sanitize_env();

        assert_eq!(
            env.get("GEMINI_API_KEY").map(String::as_str),
            Some("gemini-test-key")
        );
        assert_eq!(
            env.get("GOOGLE_API_KEY").map(String::as_str),
            Some("google-test-key")
        );
        assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
        assert_eq!(env.get("COLORTERM").map(String::as_str), Some("truecolor"));
        assert_eq!(
            env.get("SSH_AUTH_SOCK").map(String::as_str),
            Some("/tmp/test-agent.sock")
        );
    }

    #[test]
    fn forwards_allowed_prefix_entries() {
        let _lock = env_lock();
        let _ao = EnvVarGuard::set("AO_TASK_029_TEST_VAR", Some("ao-test-value"));
        let _xdg = EnvVarGuard::set("XDG_RUNTIME_DIR", Some("/tmp/xdg-runtime"));

        let env = sanitize_env();

        assert_eq!(
            env.get("AO_TASK_029_TEST_VAR").map(String::as_str),
            Some("ao-test-value")
        );
        assert_eq!(
            env.get("XDG_RUNTIME_DIR").map(String::as_str),
            Some("/tmp/xdg-runtime")
        );
    }

    #[test]
    fn keeps_existing_allowlist_and_blocks_unrelated_keys() {
        let _lock = env_lock();
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", Some("openai-test-key"));
        let _aws = EnvVarGuard::set("AWS_SECRET_ACCESS_KEY", Some("blocked-secret"));

        let env = sanitize_env();

        assert_eq!(
            env.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai-test-key")
        );
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
    }
}
