//! API-key environment variable checks for installed provider plugins.
//!
//! Two sources for the expected key list:
//! 1. The plugin's own `env_required` list (truth — set by plugin author).
//! 2. A conventional fallback table (claude → ANTHROPIC_API_KEY, etc.) for
//!    older manifests that pre-date `env_required`.

use std::path::Path;

use animus_plugin_protocol::EnvRequirement;
use orchestrator_core::SecretStore;
use orchestrator_plugin_host::discover_plugins;
use protocol::repository_scope::{repository_scope_for_path, scoped_state_root};

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "api_keys";
const DOCS_URL: &str = "https://animus-docs.vercel.app/reference/secrets";

fn conventional_keys_for(plugin_name: &str) -> Vec<&'static str> {
    let lc = plugin_name.to_ascii_lowercase();
    if lc.contains("claude") {
        vec!["ANTHROPIC_API_KEY"]
    } else if lc.contains("codex") || lc.contains("opencode") || lc.contains("oai") {
        vec!["OPENAI_API_KEY"]
    } else if lc.contains("gemini") {
        vec!["GEMINI_API_KEY", "GOOGLE_API_KEY"]
    } else if lc.contains("linear") {
        vec!["LINEAR_API_TOKEN"]
    } else {
        Vec::new()
    }
}

/// Map a provider plugin name to the CLI whose config-based (subscription)
/// login satisfies its API key. Mirrors `ops_init`'s CLI detection: a
/// `claude`/`codex`/`gemini` config dir under `$HOME` means the user is
/// authenticated via that CLI's own login rather than an env var.
fn config_login_for(plugin_name: &str) -> Option<(&'static str, &'static str)> {
    let lc = plugin_name.to_ascii_lowercase();
    if lc.contains("claude") {
        Some(("claude", ".claude"))
    } else if lc.contains("codex") {
        Some(("codex", ".codex"))
    } else if lc.contains("gemini") {
        Some(("gemini", ".gemini"))
    } else if lc.contains("opencode") {
        Some(("opencode", ".opencode"))
    } else {
        None
    }
}

/// True when the provider's CLI config dir exists under `$HOME`, the same
/// signal `animus init`'s walkthrough reports as `api_key_via_config`.
fn cli_config_login_present(plugin_name: &str) -> Option<&'static str> {
    let (cli, subpath) = config_login_for(plugin_name)?;
    let home = dirs::home_dir()?;
    if home.join(subpath).exists() {
        Some(cli)
    } else {
        None
    }
}

/// Build a keychain-backed secret store for the project's repo-scope,
/// matching how `animus secret` resolves the scope (adopted scoped-state
/// directory name first, freshly-derived `repo-scope` otherwise).
fn keychain_store(project_root: &Path) -> Option<Box<dyn SecretStore>> {
    let scoped_root = scoped_state_root(project_root)?;
    let scope = scoped_root
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| repository_scope_for_path(project_root));
    Some(orchestrator_core::build_secret_store_for_project(&scope, scoped_root, project_root))
}

/// True when `name` resolves to a non-empty value in the project secret store.
fn keychain_has_key(store: Option<&dyn SecretStore>, name: &str) -> bool {
    match store {
        Some(store) => matches!(store.get(name), Ok(Some(value)) if !value.trim().is_empty()),
        None => false,
    }
}

pub(crate) fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();
    let discovered = match discover_plugins(ctx.project_root.clone()) {
        Ok(list) => list,
        Err(_) => return out,
    };

    let store = keychain_store(&ctx.project_root);

    for plugin in &discovered {
        let manifest_required: Vec<EnvRequirement> =
            plugin.manifest.env_required.iter().filter(|env| env.required).cloned().collect();
        let conventional = conventional_keys_for(&plugin.name);

        // Manifest is authoritative. Only fall back to convention when the
        // plugin declared nothing. We track whether each name is
        // manifest-declared: the plugin host only injects keychain secrets
        // that appear in the plugin's `env_required` allow-list, so a
        // keychain entry can only *satisfy* a manifest-declared key. For a
        // conventional-fallback key (legacy manifest with no `env_required`)
        // the keychain value would never reach the plugin at runtime, so
        // counting it would be a false pass.
        let names: Vec<String> = if !manifest_required.is_empty() {
            manifest_required.iter().map(|env| env.name.clone()).collect()
        } else {
            conventional.iter().map(|s| s.to_string()).collect()
        };
        let keychain_injectable = !manifest_required.is_empty();

        if names.is_empty() {
            continue;
        }

        let cli_login = cli_config_login_present(&plugin.name);
        // A provider CLI login only replaces that provider's auth token(s)
        // (the conventional keys), never arbitrary extra `env_required` vars
        // like org ids or endpoints — those still have to be set explicitly.
        let cli_auth_keys = conventional_keys_for(&plugin.name);

        for name in names {
            let id = format!("api_key_present.{}.{}", sanitize(&plugin.name), sanitize(&name));
            let title = format!("API key {name} for {}", plugin.name);

            // Satisfaction sources, in order of preference. A subscription
            // login through the provider's own CLI (the most common setup)
            // and a manifest-declared project keychain secret both count —
            // not just a raw env var. This matches the auth surfaces
            // documented in docs/reference/secrets.md and detected by
            // `animus init`.
            let check = if std::env::var(&name).map(|v| !v.trim().is_empty()).unwrap_or(false) {
                DiagnosticCheck::new(id, CATEGORY, CheckStatus::Pass, title)
                    .details(format!("{name} satisfied via env var set in this shell"))
            } else if keychain_injectable && keychain_has_key(store.as_deref(), &name) {
                DiagnosticCheck::new(id, CATEGORY, CheckStatus::Pass, title)
                    .details(format!("{name} satisfied via keychain (`animus secret`)"))
            } else if let Some(cli) = cli_login.filter(|_| cli_auth_keys.contains(&name.as_str())) {
                DiagnosticCheck::new(id, CATEGORY, CheckStatus::Pass, title)
                    .details(format!("{name} satisfied via {cli} CLI login"))
            } else {
                DiagnosticCheck::new(id, CATEGORY, CheckStatus::Fail, title)
                    .current("not set via env var, keychain, or provider CLI login".to_string())
                    .expected(format!("{name} available via `animus secret`, shell env, or provider CLI login"))
                    .fix(CheckFix::manual(
                        "set_api_key_secret",
                        &format!(
                            "Run `animus secret set {name}` (preferred) or export it in your shell. See {DOCS_URL}."
                        ),
                    ))
            };
            out.push(check);
        }
    }

    out
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conventional_keys_map_known_providers() {
        assert_eq!(conventional_keys_for("animus-provider-claude"), vec!["ANTHROPIC_API_KEY"]);
        assert_eq!(conventional_keys_for("animus-provider-codex"), vec!["OPENAI_API_KEY"]);
        assert_eq!(conventional_keys_for("animus-provider-gemini"), vec!["GEMINI_API_KEY", "GOOGLE_API_KEY"]);
        assert_eq!(conventional_keys_for("animus-provider-oai"), vec!["OPENAI_API_KEY"]);
    }

    #[test]
    fn conventional_keys_empty_for_unknown() {
        assert!(conventional_keys_for("animus-provider-unknown").is_empty());
    }

    #[test]
    fn config_login_maps_known_providers() {
        assert_eq!(config_login_for("animus-provider-claude"), Some(("claude", ".claude")));
        assert_eq!(config_login_for("animus-provider-codex"), Some(("codex", ".codex")));
        assert_eq!(config_login_for("animus-provider-gemini"), Some(("gemini", ".gemini")));
        assert_eq!(config_login_for("animus-provider-opencode"), Some(("opencode", ".opencode")));
        assert_eq!(config_login_for("animus-subject-linear"), None);
    }

    #[test]
    fn cli_config_login_present_detects_config_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let _home = protocol::test_utils::EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        assert_eq!(cli_config_login_present("animus-provider-claude"), None);
        std::fs::create_dir_all(temp.path().join(".claude")).unwrap();
        assert_eq!(cli_config_login_present("animus-provider-claude"), Some("claude"));
    }

    #[test]
    fn cli_login_only_replaces_conventional_auth_keys() {
        // A provider CLI login replaces its own auth token but cannot stand in
        // for an unrelated required var (org id, endpoint, etc.).
        let claude_auth = conventional_keys_for("animus-provider-claude");
        assert!(claude_auth.contains(&"ANTHROPIC_API_KEY"));
        assert!(!claude_auth.contains(&"ANTHROPIC_ORG_ID"));
        assert!(!claude_auth.contains(&"SOME_OTHER_REQUIRED_VAR"));
    }

    #[test]
    fn keychain_has_key_reads_non_empty_values() {
        use orchestrator_core::MockSecretStore;
        let store = MockSecretStore::with_entries([("ANTHROPIC_API_KEY", "sk-live")]);
        assert!(matches!(store.get("ANTHROPIC_API_KEY"), Ok(Some(v)) if !v.trim().is_empty()));
        assert!(matches!(store.get("MISSING"), Ok(None)));
    }
}
