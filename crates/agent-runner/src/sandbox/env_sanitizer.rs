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

fn is_allowed_env_var(var: &str) -> bool {
    ALLOWED_ENV_VARS.contains(&var)
        || ALLOWED_ENV_PREFIXES
            .iter()
            .any(|prefix| var.starts_with(prefix))
}

pub fn sanitize_env() -> HashMap<String, String> {
    env::vars_os()
        .filter_map(|(var, value)| {
            let var = var.into_string().ok()?;
            if !is_allowed_env_var(&var) {
                return None;
            }
            let value = value.into_string().ok()?;
            Some((var, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            Self::set_os(key, value.map(OsStr::new))
        }

        fn set_os(key: &'static str, value: Option<&OsStr>) -> Self {
            let previous = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            };
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
    fn allowlist_includes_required_entries_for_runner_clis() {
        for key in [
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "TERM",
            "COLORTERM",
            "SSH_AUTH_SOCK",
            "AO_CONFIG_DIR",
            "AO_RUNNER_CONFIG_DIR",
            "AO_RUNNER_SCOPE",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
        ] {
            assert!(is_allowed_env_var(key), "expected {key} to be allowed");
        }

        for key in ["AO_TASK_029_PREFIX_TEST", "XDG_RUNTIME_DIR"] {
            assert!(
                is_allowed_env_var(key),
                "expected {key} to be allowed by prefix"
            );
        }

        for key in ["AO", "XDG", "GOOGLE", "TERMINFO"] {
            assert!(!is_allowed_env_var(key), "expected {key} to be blocked");
        }
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
    fn forwards_known_ao_and_xdg_configuration_entries() {
        let _lock = env_lock();
        let _ao_config_dir = EnvVarGuard::set("AO_CONFIG_DIR", Some("/tmp/ao-config"));
        let _ao_runner_config_dir =
            EnvVarGuard::set("AO_RUNNER_CONFIG_DIR", Some("/tmp/ao-runner-config"));
        let _ao_runner_scope = EnvVarGuard::set("AO_RUNNER_SCOPE", Some("project"));
        let _xdg_config_home = EnvVarGuard::set("XDG_CONFIG_HOME", Some("/tmp/xdg-config"));
        let _xdg_cache_home = EnvVarGuard::set("XDG_CACHE_HOME", Some("/tmp/xdg-cache"));

        let env = sanitize_env();

        assert_eq!(
            env.get("AO_CONFIG_DIR").map(String::as_str),
            Some("/tmp/ao-config")
        );
        assert_eq!(
            env.get("AO_RUNNER_CONFIG_DIR").map(String::as_str),
            Some("/tmp/ao-runner-config")
        );
        assert_eq!(
            env.get("AO_RUNNER_SCOPE").map(String::as_str),
            Some("project")
        );
        assert_eq!(
            env.get("XDG_CONFIG_HOME").map(String::as_str),
            Some("/tmp/xdg-config")
        );
        assert_eq!(
            env.get("XDG_CACHE_HOME").map(String::as_str),
            Some("/tmp/xdg-cache")
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

    #[test]
    fn prefix_matching_is_strict() {
        let _lock = env_lock();
        let _ao = EnvVarGuard::set("AO_TASK_029_STRICT_TEST", Some("ao-allowed"));
        let _xdg = EnvVarGuard::set("XDG_RUNTIME_DIR", Some("/tmp/xdg-allowed"));
        let _ao_near_miss = EnvVarGuard::set("AO", Some("blocked"));
        let _xdg_near_miss = EnvVarGuard::set("XDG", Some("blocked"));
        let _ao_case_miss = EnvVarGuard::set("ao_TASK_029_STRICT_TEST", Some("blocked"));

        let env = sanitize_env();

        assert_eq!(
            env.get("AO_TASK_029_STRICT_TEST").map(String::as_str),
            Some("ao-allowed")
        );
        assert_eq!(
            env.get("XDG_RUNTIME_DIR").map(String::as_str),
            Some("/tmp/xdg-allowed")
        );
        assert!(!env.contains_key("AO"));
        assert!(!env.contains_key("XDG"));
        assert!(!env.contains_key("ao_TASK_029_STRICT_TEST"));
    }

    #[cfg(unix)]
    #[test]
    fn ignores_non_unicode_env_values() {
        use std::os::unix::ffi::OsStrExt;

        let _lock = env_lock();
        let invalid = OsStr::from_bytes(&[0x66, 0x6f, 0xff, 0x6f]);
        let _non_unicode = EnvVarGuard::set_os("AO_TASK_029_NON_UNICODE", Some(invalid));

        let env = sanitize_env();

        assert!(!env.contains_key("AO_TASK_029_NON_UNICODE"));
    }
}
