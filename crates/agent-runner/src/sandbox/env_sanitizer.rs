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
    // Claude CLI configuration paths (not credentials)
    "CLAUDE_CODE_SETTINGS_PATH",
    "CLAUDE_CODE_DIR",
];

const ALLOWED_ENV_PREFIXES: &[&str] = &["AO_", "XDG_"];

fn is_allowed_env_var(var: &str) -> bool {
    ALLOWED_ENV_VARS.contains(&var) || ALLOWED_ENV_PREFIXES.iter().any(|prefix| var.starts_with(prefix))
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
    use std::ffi::OsStr;
    use std::sync::MutexGuard;

    use protocol::test_utils::EnvVarGuard;

    fn env_lock() -> MutexGuard<'static, ()> {
        crate::test_env_lock().lock().expect("env lock should be available")
    }

    #[test]
    fn allowlist_includes_required_entries_for_runner_clis() {
        for key in [
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
            assert!(is_allowed_env_var(key), "expected {key} to be allowed by prefix");
        }

        for key in ["AO", "XDG", "GOOGLE", "TERMINFO"] {
            assert!(!is_allowed_env_var(key), "expected {key} to be blocked");
        }
    }

    #[test]
    fn forwards_terminal_and_ssh_entries() {
        let _lock = env_lock();
        let _term = EnvVarGuard::set("TERM", Some("xterm-256color"));
        let _colorterm = EnvVarGuard::set("COLORTERM", Some("truecolor"));
        let _ssh = EnvVarGuard::set("SSH_AUTH_SOCK", Some("/tmp/test-agent.sock"));

        let env = sanitize_env();

        assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
        assert_eq!(env.get("COLORTERM").map(String::as_str), Some("truecolor"));
        assert_eq!(env.get("SSH_AUTH_SOCK").map(String::as_str), Some("/tmp/test-agent.sock"));
    }

    #[test]
    fn forwards_allowed_prefix_entries() {
        let _lock = env_lock();
        let _ao = EnvVarGuard::set("AO_TASK_029_TEST_VAR", Some("ao-test-value"));
        let _xdg = EnvVarGuard::set("XDG_RUNTIME_DIR", Some("/tmp/xdg-runtime"));

        let env = sanitize_env();

        assert_eq!(env.get("AO_TASK_029_TEST_VAR").map(String::as_str), Some("ao-test-value"));
        assert_eq!(env.get("XDG_RUNTIME_DIR").map(String::as_str), Some("/tmp/xdg-runtime"));
    }

    #[test]
    fn forwards_known_ao_and_xdg_configuration_entries() {
        let _lock = env_lock();
        let _ao_config_dir = EnvVarGuard::set("AO_CONFIG_DIR", Some("/tmp/ao-config"));
        let _ao_runner_config_dir = EnvVarGuard::set("AO_RUNNER_CONFIG_DIR", Some("/tmp/ao-runner-config"));
        let _ao_runner_scope = EnvVarGuard::set("AO_RUNNER_SCOPE", Some("project"));
        let _xdg_config_home = EnvVarGuard::set("XDG_CONFIG_HOME", Some("/tmp/xdg-config"));
        let _xdg_cache_home = EnvVarGuard::set("XDG_CACHE_HOME", Some("/tmp/xdg-cache"));

        let env = sanitize_env();

        assert_eq!(env.get("AO_CONFIG_DIR").map(String::as_str), Some("/tmp/ao-config"));
        assert_eq!(env.get("AO_RUNNER_CONFIG_DIR").map(String::as_str), Some("/tmp/ao-runner-config"));
        assert_eq!(env.get("AO_RUNNER_SCOPE").map(String::as_str), Some("project"));
        assert_eq!(env.get("XDG_CONFIG_HOME").map(String::as_str), Some("/tmp/xdg-config"));
        assert_eq!(env.get("XDG_CACHE_HOME").map(String::as_str), Some("/tmp/xdg-cache"));
    }

    #[test]
    fn blocks_api_keys_and_secrets() {
        let _lock = env_lock();
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", Some("openai-test-key"));
        let _anthropic = EnvVarGuard::set("ANTHROPIC_API_KEY", Some("ant-test-key"));
        let _claude = EnvVarGuard::set("CLAUDE_API_KEY", Some("claude-test-key"));
        let _aws = EnvVarGuard::set("AWS_SECRET_ACCESS_KEY", Some("blocked-secret"));

        let env = sanitize_env();

        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert!(!env.contains_key("CLAUDE_API_KEY"));
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

        assert_eq!(env.get("AO_TASK_029_STRICT_TEST").map(String::as_str), Some("ao-allowed"));
        assert_eq!(env.get("XDG_RUNTIME_DIR").map(String::as_str), Some("/tmp/xdg-allowed"));
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

    #[test]
    fn allows_path_and_home() {
        let _lock = env_lock();
        let _path = EnvVarGuard::set("PATH", Some("/usr/bin:/usr/local/bin"));
        let _home = EnvVarGuard::set("HOME", Some("/Users/testuser"));

        let env = sanitize_env();

        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin:/usr/local/bin"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/Users/testuser"));
    }

    #[test]
    fn blocks_dangerous_env_vars() {
        let _lock = env_lock();
        let _db = EnvVarGuard::set("DATABASE_URL", Some("postgres://secret@localhost/db"));
        let _stripe = EnvVarGuard::set("STRIPE_SECRET_KEY", Some("sk_test_xxx"));
        let _npm = EnvVarGuard::set("NPM_TOKEN", Some("npm-token-secret"));
        let _docker = EnvVarGuard::set("DOCKER_HOST", Some("tcp://localhost:2375"));
        let _ld = EnvVarGuard::set("LD_PRELOAD", Some("/tmp/evil.so"));

        let env = sanitize_env();

        assert!(!env.contains_key("DATABASE_URL"));
        assert!(!env.contains_key("STRIPE_SECRET_KEY"));
        assert!(!env.contains_key("NPM_TOKEN"));
        assert!(!env.contains_key("DOCKER_HOST"));
        assert!(!env.contains_key("LD_PRELOAD"));
    }

    #[test]
    fn preserves_ao_prefix_vars() {
        let _lock = env_lock();
        let _custom1 = EnvVarGuard::set("AO_MY_CUSTOM_SETTING", Some("value1"));
        let _custom2 = EnvVarGuard::set("AO_RUNNER_BUILD_ID", Some("build-abc"));
        let _custom3 = EnvVarGuard::set("AO_DEBUG", Some("true"));

        let env = sanitize_env();

        assert_eq!(env.get("AO_MY_CUSTOM_SETTING").map(String::as_str), Some("value1"));
        assert_eq!(env.get("AO_RUNNER_BUILD_ID").map(String::as_str), Some("build-abc"));
        assert_eq!(env.get("AO_DEBUG").map(String::as_str), Some("true"));
    }

    #[test]
    fn sanitize_env_returns_only_allowed_keys() {
        let _lock = env_lock();
        let _allowed = EnvVarGuard::set("HOME", Some("/home/test"));
        let _blocked = EnvVarGuard::set("MYSQL_PASSWORD", Some("secret123"));

        let env = sanitize_env();

        for key in env.keys() {
            assert!(is_allowed_env_var(key), "sanitize_env returned disallowed key: {}", key);
        }
    }

    #[test]
    fn claude_config_paths_allowed_but_api_key_blocked() {
        let _lock = env_lock();
        let _settings = EnvVarGuard::set("CLAUDE_CODE_SETTINGS_PATH", Some("/tmp/.claude/settings.json"));
        let _api = EnvVarGuard::set("CLAUDE_API_KEY", Some("claude-key-123"));
        let _dir = EnvVarGuard::set("CLAUDE_CODE_DIR", Some("/tmp/claude-dir"));

        let env = sanitize_env();

        assert_eq!(env.get("CLAUDE_CODE_SETTINGS_PATH").map(String::as_str), Some("/tmp/.claude/settings.json"));
        assert_eq!(env.get("CLAUDE_CODE_DIR").map(String::as_str), Some("/tmp/claude-dir"));
        assert!(!env.contains_key("CLAUDE_API_KEY"), "CLAUDE_API_KEY is an API credential and must not be forwarded");
    }
}
