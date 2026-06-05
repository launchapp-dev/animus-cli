//! API-key environment variable checks for installed provider plugins.
//!
//! Two sources for the expected key list:
//! 1. The plugin's own `env_required` list (truth — set by plugin author).
//! 2. A conventional fallback table (claude → ANTHROPIC_API_KEY, etc.) for
//!    older manifests that pre-date `env_required`.

use animus_plugin_protocol::EnvRequirement;
use orchestrator_plugin_host::discover_plugins;

use super::check_kit::{CheckContext, CheckFix, CheckStatus, DiagnosticCheck};

const CATEGORY: &str = "api_keys";
const DOCS_URL: &str = "https://animus-docs.vercel.app/reference/configuration";

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

pub(crate) fn run(ctx: &CheckContext) -> Vec<DiagnosticCheck> {
    let mut out = Vec::new();
    let discovered = match discover_plugins(ctx.project_root.clone()) {
        Ok(list) => list,
        Err(_) => return out,
    };

    for plugin in &discovered {
        let manifest_required: Vec<EnvRequirement> =
            plugin.manifest.env_required.iter().filter(|env| env.required).cloned().collect();
        let conventional = conventional_keys_for(&plugin.name);

        // Manifest is authoritative. Only fall back to convention when the
        // plugin declared nothing.
        let names: Vec<String> = if !manifest_required.is_empty() {
            manifest_required.iter().map(|env| env.name.clone()).collect()
        } else {
            conventional.iter().map(|s| s.to_string()).collect()
        };

        if names.is_empty() {
            continue;
        }

        for name in names {
            let id = format!("api_key_present.{}.{}", sanitize(&plugin.name), sanitize(&name));
            let present = std::env::var(&name).map(|v| !v.trim().is_empty()).unwrap_or(false);
            let check = if present {
                DiagnosticCheck::new(id, CATEGORY, CheckStatus::Pass, format!("API key {name} for {}", plugin.name))
                    .details("env var is set in this shell".to_string())
            } else {
                DiagnosticCheck::new(id, CATEGORY, CheckStatus::Fail, format!("API key {name} for {}", plugin.name))
                    .current("env var unset or empty".to_string())
                    .expected(format!("{name}=<your-key>"))
                    .fix(CheckFix::manual(
                        "set_api_key_env",
                        &format!("Add `export {name}=<your-key>` to your shell profile. See {DOCS_URL}."),
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
}
