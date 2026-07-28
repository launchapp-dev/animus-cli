use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use animus_session_backend::error::{Error, Result};
use animus_session_backend::session::{
    session_backend::SessionBackend, session_request::SessionRequest, session_run::SessionRun,
};

use super::plugin_backend::{discover_provider_plugins, PluginSessionBackend};

/// Return the DECLARED `supports_mcp` of the provider plugin backing `tool`,
/// or `None` when no provider plugin is installed for it.
///
/// This is the SOURCE OF TRUTH for whether the daemon injects an agent's
/// profile/skill-declared MCP servers for a provider tool: it is the loaded
/// plugin's OWN declaration (`PluginSessionBackend::capabilities().supports_mcp`,
/// itself derived from the plugin manifest), resolved UNIFORMLY for every
/// provider tool — NOT a kernel-maintained name->capability allow-list and NOT
/// a name-based "unknown tool => assume MCP" heuristic. A tool with no backing
/// provider plugin returns `None`; the caller then falls back to its minimal
/// static behavior for genuinely in-tree/legacy tools.
///
/// Discovery is manifest-only (no long-running spawn) and backed by the
/// on-disk manifest cache, so repeated calls within a process are cheap.
pub fn provider_declared_supports_mcp(project_root: &Path, tool: &str) -> Option<bool> {
    let lookup = canonical_tool_alias(tool);
    discover_provider_plugins(project_root)
        .into_iter()
        .find(|plugin| plugin.provider_tool.eq_ignore_ascii_case(&lookup))
        .map(|plugin| plugin.declared_supports_mcp)
}

/// Provider tool names that historically mapped to in-tree (built-in) backends.
///
/// The in-tree backends were removed in v0.4.12 in favor of the standalone
/// `launchapp-dev/animus-provider-*` plugins. These names are still considered
/// "reserved" by the install pipeline: a third-party plugin whose
/// `provider_tool` matches one of them silently hijacks the entire dispatch
/// path for that tool — a supply-chain risk — so installs refuse such plugins
/// unless the operator passes `--allow-shadow-builtin`. The resolver no
/// longer emits a runtime warning because there is no longer a built-in to
/// shadow; the install-time gate is the sole defense.
pub const RESERVED_PROVIDER_TOOLS: &[&str] =
    &["claude", "codex", "gemini", "opencode", "oai", "oai-agent", "oai-runner"];

/// Returns `true` when the supplied provider tool name collides with the
/// canonical name of one of the externally-shipped first-party provider
/// plugins (case-insensitive).
pub fn is_reserved_provider_tool(tool: &str) -> bool {
    let lower = tool.trim().to_ascii_lowercase();
    RESERVED_PROVIDER_TOOLS.iter().any(|reserved| reserved.eq_ignore_ascii_case(&lower))
}

/// Workflows historically named the OAI runner backend `oai-runner` or
/// `animus-oai-runner`. The MULTI-STEP agentic provider
/// `launchapp-dev/animus-provider-oai-agent` registers under
/// `provider_tool = "oai-agent"` (plugin name minus the `animus-provider-`
/// prefix) and is the canonical home of the `animus-oai-runner` agent loop
/// (tool-calling, MCP client, multi-turn). Map the `oai-runner` aliases to it
/// so `tool: oai-runner` reaches the multi-step runner — a one-shot completion
/// provider cannot satisfy a reasoning/tool-calling model. Raw `tool: oai`
/// still resolves to the one-shot completion provider `animus-provider-oai`
/// for callers that explicitly want the simple, single-turn path.
pub fn canonical_tool_alias(tool: &str) -> String {
    let lower = tool.trim().to_ascii_lowercase();
    match lower.as_str() {
        "oai-runner" | "animus-oai-runner" => "oai-agent".to_string(),
        _ => lower,
    }
}

/// Dispatch resolver that routes session requests to the matching provider
/// plugin discovered on disk.
///
/// As of v0.4.12 there are no in-tree provider backends — every provider
/// (claude, codex, gemini, opencode, oai-runner, plus any third-party) is a
/// standalone plugin discovered through `orchestrator-plugin-host`. If a
/// request targets a tool whose plugin is not installed, the resolver
/// surfaces a hard error with an actionable install command. There is no
/// silent fallback path.
pub struct SessionBackendResolver {
    plugin_providers: HashMap<String, Arc<PluginSessionBackend>>,
}

impl SessionBackendResolver {
    /// Construct an empty resolver. Useful for tests that wire plugins in
    /// manually; production callers should use [`Self::with_plugin_discovery`].
    pub fn new() -> Self {
        Self { plugin_providers: HashMap::new() }
    }

    /// Construct a resolver populated from Animus STDIO provider plugin discovery
    /// against the supplied project root.
    pub fn with_plugin_discovery(project_root: &Path) -> Self {
        let mut resolver = Self::new();
        resolver.refresh_plugin_providers(project_root);
        resolver
    }

    /// Re-scan plugin discovery sources and replace the cached provider map.
    pub fn refresh_plugin_providers(&mut self, project_root: &Path) {
        self.plugin_providers = discover_provider_plugins(project_root)
            .into_iter()
            .map(|plugin| (plugin.provider_tool.to_ascii_lowercase(), plugin.into_backend()))
            .collect();
    }

    /// Resolve the backend for `request.tool`, or surface a hard error
    /// pointing at the right install command when no plugin is installed
    /// for the requested tool.
    pub fn resolve(&self, request: &SessionRequest) -> Result<Arc<dyn SessionBackend>> {
        let lookup_tool = canonical_tool_alias(&request.tool);
        if let Some(plugin) = self.plugin_providers.get(&lookup_tool) {
            return Ok(plugin.clone());
        }

        Err(Error::ExecutionFailed(missing_provider_message(&request.tool)))
    }

    /// Start a session through the resolved backend, or surface the
    /// "plugin not installed" error.
    pub async fn start_session(&self, request: SessionRequest) -> Result<SessionRun> {
        let backend = self.resolve(&request)?;
        backend.start_session(request).await
    }
}

impl Default for SessionBackendResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// The exact `animus plugin install ...` command that fixes a missing
/// provider plugin for `tool`. Public so callers (e.g. the CLI's error
/// envelopes) can carry the command as structured data instead of scraping
/// it back out of [`missing_provider_message`]'s human-readable text.
pub fn provider_install_command(tool: &str) -> String {
    let canonical = canonical_tool_alias(tool);
    let install_target = if is_reserved_provider_tool(&canonical) {
        format!("launchapp-dev/animus-provider-{canonical}")
    } else {
        format!("<publisher>/animus-provider-{canonical}")
    };
    format!("animus plugin install {install_target} --allow-shadow-builtin")
}

/// Build the actionable error message returned when a provider plugin is not
/// installed for the requested tool.
fn missing_provider_message(tool: &str) -> String {
    let install_command = provider_install_command(tool);
    format!(
        "Provider plugin '{tool}' not installed. Install with:\n  {install_command}\nOr run: animus plugin install-defaults"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::path::PathBuf;

    use animus_session_backend::session::SessionRequest;

    use super::SessionBackendResolver;

    fn request_for(tool: &str) -> SessionRequest {
        SessionRequest {
            tool: tool.to_string(),
            model: String::new(),
            prompt: "hello".to_string(),
            cwd: PathBuf::from("."),
            project_root: None,
            mcp_endpoint: None,
            mcp_servers: None,
            permission_mode: None,
            timeout_secs: None,
            env_vars: Vec::new(),
            extras: json!({}),
            actor: None,
        }
    }

    fn expect_resolve_err(resolver: &SessionBackendResolver, tool: &str) -> String {
        match resolver.resolve(&request_for(tool)) {
            Ok(_) => panic!("missing plugin must error for tool '{tool}'"),
            Err(err) => err.to_string(),
        }
    }

    fn expect_resolve_ok(
        resolver: &SessionBackendResolver,
        tool: &str,
    ) -> std::sync::Arc<dyn animus_session_backend::session::session_backend::SessionBackend> {
        match resolver.resolve(&request_for(tool)) {
            Ok(backend) => backend,
            Err(err) => panic!("plugin should resolve for tool '{tool}': {err}"),
        }
    }

    #[test]
    fn provider_install_command_matches_the_message_text() {
        assert_eq!(
            super::provider_install_command("claude"),
            "animus plugin install launchapp-dev/animus-provider-claude --allow-shadow-builtin"
        );
        assert_eq!(
            super::provider_install_command("custom-tool"),
            "animus plugin install <publisher>/animus-provider-custom-tool --allow-shadow-builtin"
        );
        // The oai-runner aliases now resolve to the multi-step agentic provider.
        assert_eq!(
            super::provider_install_command("oai-runner"),
            "animus plugin install launchapp-dev/animus-provider-oai-agent --allow-shadow-builtin"
        );
        // The human-readable message embeds the same command verbatim.
        let resolver = SessionBackendResolver::new();
        let msg = expect_resolve_err(&resolver, "claude");
        assert!(msg.contains(&super::provider_install_command("claude")), "actual: {msg}");
    }

    #[test]
    fn resolver_errors_when_no_plugin_for_reserved_tool() {
        let resolver = SessionBackendResolver::new();
        let msg = expect_resolve_err(&resolver, "claude");
        assert!(msg.contains("Provider plugin 'claude' not installed"), "actual: {msg}");
        assert!(msg.contains("animus plugin install launchapp-dev/animus-provider-claude"), "actual: {msg}");
        assert!(msg.contains("install-defaults"), "actual: {msg}");
    }

    #[test]
    fn resolver_errors_when_no_plugin_for_unknown_tool() {
        let resolver = SessionBackendResolver::new();
        let msg = expect_resolve_err(&resolver, "custom-tool");
        assert!(msg.contains("Provider plugin 'custom-tool' not installed"), "actual: {msg}");
        assert!(msg.contains("<publisher>/animus-provider-custom-tool"), "actual: {msg}");
    }

    #[tokio::test]
    async fn start_session_propagates_missing_plugin_error() {
        let resolver = SessionBackendResolver::new();
        let msg = match resolver.start_session(request_for("codex")).await {
            Ok(_) => panic!("start should fail without plugin"),
            Err(err) => err.to_string(),
        };
        assert!(msg.contains("Provider plugin 'codex' not installed"), "actual: {msg}");
    }

    #[test]
    fn resolver_dispatches_to_installed_plugin() {
        let mut resolver = SessionBackendResolver::new();
        resolver.plugin_providers.insert(
            "mock".to_string(),
            std::sync::Arc::new(super::super::plugin_backend::PluginSessionBackend::new(
                "animus-provider-mock",
                PathBuf::from("/tmp/animus-provider-mock"),
                "mock",
            )),
        );
        let backend = expect_resolve_ok(&resolver, "mock");
        assert_eq!(backend.info().provider_tool, "mock");
    }

    #[test]
    fn resolver_dispatches_to_plugin_even_for_reserved_tool() {
        // When a plugin claims a reserved tool (e.g. claude) the resolver hands
        // off to that plugin — the install gate decides whether the plugin
        // was allowed onto disk in the first place.
        let mut resolver = SessionBackendResolver::new();
        resolver.plugin_providers.insert(
            "claude".to_string(),
            std::sync::Arc::new(super::super::plugin_backend::PluginSessionBackend::new(
                "animus-provider-claude",
                PathBuf::from("/tmp/animus-provider-claude"),
                "claude",
            )),
        );
        let backend = expect_resolve_ok(&resolver, "claude");
        let info = backend.info();
        assert_eq!(info.provider_tool, "claude");
        assert!(
            info.display_name.to_ascii_lowercase().contains("plugin"),
            "expected plugin display name, got {}",
            info.display_name
        );
    }

    #[test]
    fn reserved_provider_tools_includes_all_first_party_providers() {
        for tool in ["claude", "codex", "gemini", "opencode", "oai", "oai-agent", "oai-runner"] {
            assert!(super::is_reserved_provider_tool(tool), "{tool} should be reserved");
            assert!(super::is_reserved_provider_tool(&tool.to_ascii_uppercase()));
        }
        assert!(!super::is_reserved_provider_tool("linear"));
        assert!(!super::is_reserved_provider_tool("custom-backend"));
    }

    #[test]
    fn resolver_routes_oai_runner_aliases_to_the_agentic_provider() {
        // The `oai-runner` / `animus-oai-runner` aliases are the canonical
        // workflow tool for the MULTI-STEP runner, which ships as
        // `launchapp-dev/animus-provider-oai-agent` (provider_tool "oai-agent").
        // Raw `tool: oai` stays pinned to the one-shot completion provider.
        let mut resolver = SessionBackendResolver::new();
        resolver.plugin_providers.insert(
            "oai-agent".to_string(),
            std::sync::Arc::new(super::super::plugin_backend::PluginSessionBackend::new(
                "animus-provider-oai-agent",
                PathBuf::from("/tmp/animus-provider-oai-agent"),
                "oai-agent",
            )),
        );
        resolver.plugin_providers.insert(
            "oai".to_string(),
            std::sync::Arc::new(super::super::plugin_backend::PluginSessionBackend::new(
                "animus-provider-oai",
                PathBuf::from("/tmp/animus-provider-oai"),
                "oai",
            )),
        );

        for alias in ["oai-runner", "animus-oai-runner", "OAI-Runner", "ANIMUS-OAI-RUNNER"] {
            let backend = expect_resolve_ok(&resolver, alias);
            assert_eq!(
                backend.info().provider_tool,
                "oai-agent",
                "alias {alias} should resolve to the agentic multi-step provider"
            );
        }
        // Raw `oai` still resolves to the one-shot completion provider.
        let oneshot = expect_resolve_ok(&resolver, "oai");
        assert_eq!(oneshot.info().provider_tool, "oai", "raw oai must stay one-shot");
    }

    #[test]
    fn missing_oai_runner_alias_hints_at_agentic_provider_install() {
        let resolver = SessionBackendResolver::new();
        for alias in ["oai-runner", "animus-oai-runner"] {
            let msg = expect_resolve_err(&resolver, alias);
            assert!(
                msg.contains("animus plugin install launchapp-dev/animus-provider-oai-agent"),
                "alias {alias} should hint at the agentic animus-provider-oai-agent install; got: {msg}"
            );
        }
    }
}
