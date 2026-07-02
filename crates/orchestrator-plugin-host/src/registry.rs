use std::collections::HashMap;
use std::path::PathBuf;

use animus_plugin_protocol::McpTool;
use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::host::{PluginSpawnOptions, PluginStderrSink};
use crate::{DiscoveredPlugin, PluginDiscovery, PluginHost};

pub struct PluginRegistry {
    discovered: HashMap<String, DiscoveredPlugin>,
    running: HashMap<String, PluginHost>,
    mcp_tools: HashMap<String, (String, McpTool)>,
    stderr_sink: Option<PluginStderrSink>,
    project_root: PathBuf,
}

impl PluginRegistry {
    /// Server-safe discovery: the project-local `<project_root>/.animus/plugins/`
    /// directory scan is OFF. This is the constructor wired into the daemon
    /// subject-resolution fallback (`PluginSubjectFallback`), so a daemon
    /// resolving subjects against a cloned repo never executes a
    /// repo-shipped binary during discovery. Mirrors
    /// [`crate::discover_plugins`].
    pub fn discover(project_root: impl Into<PathBuf>) -> Result<Self> {
        Self::discover_inner(project_root, false)
    }

    /// Like [`PluginRegistry::discover`] but also scans (and executes via
    /// `--manifest`) the project-local `<project_root>/.animus/plugins/`
    /// directory. Use ONLY from operator-facing local-dev surfaces (the MCP
    /// `animus.plugin.*` tools) where the working tree is trusted — never
    /// from an autonomous daemon path.
    pub fn discover_including_project_local(project_root: impl Into<PathBuf>) -> Result<Self> {
        Self::discover_inner(project_root, true)
    }

    fn discover_inner(project_root: impl Into<PathBuf>, probe_project_local: bool) -> Result<Self> {
        let project_root = project_root.into();
        let discovered = PluginDiscovery::new()
            .with_project_root(project_root.clone())
            .probe_project_local_plugins(probe_project_local)
            .discover()?
            .into_iter()
            .map(|plugin| (plugin.name.clone(), plugin))
            .collect();

        Ok(Self { discovered, running: HashMap::new(), mcp_tools: HashMap::new(), stderr_sink: None, project_root })
    }

    /// Route every spawned plugin's stderr through the supplied sink. Useful for
    /// surfacing plugin diagnostics into structured project logs.
    #[must_use]
    pub fn with_stderr_sink(mut self, sink: PluginStderrSink) -> Self {
        self.stderr_sink = Some(sink);
        self
    }

    pub fn list_plugins(&self) -> impl Iterator<Item = &DiscoveredPlugin> {
        self.discovered.values()
    }

    pub fn is_running(&self, name: &str) -> bool {
        self.running.contains_key(name)
    }

    pub async fn get_plugin(&mut self, name: &str) -> Result<&PluginHost> {
        if !self.running.contains_key(name) {
            let plugin = self.discovered.get(name).ok_or_else(|| anyhow!("unknown plugin '{name}'"))?;
            let path = plugin.path.clone();
            let options = PluginSpawnOptions::for_manifest(
                name.to_string(),
                &plugin.manifest.env_required,
                std::iter::empty::<String>(),
                self.stderr_sink.clone(),
            )
            .with_notification_buffer_hint(plugin.manifest.notification_buffer_size)
            .with_working_dir(&self.project_root);
            let host = PluginHost::spawn_with_options(&path, &[], options).await?;
            let result = match host.handshake().await {
                Ok(result) => result,
                Err(error) => {
                    let _ = host.shutdown().await;
                    return Err(error);
                }
            };
            if let Err(error) = self.register_mcp_tools(name, result.capabilities.mcp_tools) {
                let _ = host.shutdown().await;
                return Err(error);
            }
            self.running.insert(name.to_string(), host);
        }

        self.running.get(name).ok_or_else(|| anyhow!("plugin '{name}' was not available after startup"))
    }

    pub async fn initialize_all(&mut self) -> Result<()> {
        let names = self.discovered.keys().cloned().collect::<Vec<_>>();
        for name in names {
            self.get_plugin(&name).await?;
        }
        Ok(())
    }

    pub fn mcp_tools(&self) -> impl Iterator<Item = &McpTool> {
        self.mcp_tools.values().map(|(_, tool)| tool)
    }

    pub fn mcp_tool_owner(&self, tool_name: &str) -> Option<&str> {
        self.mcp_tools.get(tool_name).map(|(owner, _)| owner.as_str())
    }

    pub async fn call_mcp_tool(&mut self, tool_name: &str, arguments: Value) -> Result<Value> {
        let owner = self
            .mcp_tools
            .get(tool_name)
            .map(|(owner, _)| owner.clone())
            .ok_or_else(|| anyhow!("no plugin owns MCP tool '{tool_name}'"))?;
        let host = self.get_plugin(&owner).await?;
        host.request(
            "mcp/tool_call",
            Some(serde_json::json!({
                "name": tool_name,
                "arguments": arguments,
            })),
        )
        .await
        .map_err(|error| anyhow!("plugin MCP tool call failed ({}): {}", error.code, error.message))
    }

    pub async fn shutdown_all(&mut self) -> Result<()> {
        let running = std::mem::take(&mut self.running);
        for (_, host) in running {
            if let Err(error) = host.shutdown().await {
                tracing::warn!(%error, "failed to shut down plugin");
            }
        }
        self.mcp_tools.clear();
        Ok(())
    }

    #[cfg(test)]
    fn empty_for_tests() -> Self {
        Self {
            discovered: HashMap::new(),
            running: HashMap::new(),
            mcp_tools: HashMap::new(),
            stderr_sink: None,
            project_root: PathBuf::from("."),
        }
    }

    fn register_mcp_tools(&mut self, owner: &str, tools: Vec<McpTool>) -> Result<()> {
        // Validate the full batch BEFORE inserting anything so a mid-batch
        // cross-plugin duplicate cannot leave the registry advertising tools
        // from a plugin whose registration failed (and whose host was shut
        // down by the caller).
        for tool in &tools {
            if let Some((existing_owner, _)) = self.mcp_tools.get(&tool.name) {
                if existing_owner != owner {
                    return Err(anyhow!(
                        "duplicate MCP tool '{}' registered by '{}' and '{}'",
                        tool.name,
                        existing_owner,
                        owner
                    ));
                }
            }
        }
        for tool in tools {
            self.mcp_tools.insert(tool.name.clone(), (owner.to_string(), tool));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> McpTool {
        McpTool { name: name.to_string(), description: None, input_schema: None }
    }

    /// A cross-plugin duplicate anywhere in the batch must leave the
    /// registry untouched: no tool from the failed plugin may stay
    /// advertised (the caller shuts the plugin's host down on error, so a
    /// leftover entry would silently respawn it on the next tool call).
    #[test]
    fn duplicate_tool_registration_leaves_no_orphan_tools() {
        let mut registry = PluginRegistry::empty_for_tests();
        registry.register_mcp_tools("plugin-a", vec![tool("alpha")]).expect("first registration succeeds");

        let err = registry
            .register_mcp_tools("plugin-b", vec![tool("beta"), tool("alpha")])
            .expect_err("cross-plugin duplicate must be rejected");
        assert!(format!("{err}").contains("duplicate MCP tool 'alpha'"), "unexpected error: {err}");

        assert_eq!(registry.mcp_tool_owner("alpha"), Some("plugin-a"), "original owner must be preserved");
        assert_eq!(
            registry.mcp_tool_owner("beta"),
            None,
            "no tool from the failed plugin's batch may remain registered"
        );
        assert_eq!(registry.mcp_tools().count(), 1);
    }

    /// Re-registering the same owner's tools (e.g. after a respawn) stays
    /// allowed — only cross-plugin collisions are duplicates.
    #[test]
    fn same_owner_reregistration_is_allowed() {
        let mut registry = PluginRegistry::empty_for_tests();
        registry.register_mcp_tools("plugin-a", vec![tool("alpha")]).expect("first registration");
        registry.register_mcp_tools("plugin-a", vec![tool("alpha"), tool("gamma")]).expect("same-owner re-register");
        assert_eq!(registry.mcp_tool_owner("alpha"), Some("plugin-a"));
        assert_eq!(registry.mcp_tool_owner("gamma"), Some("plugin-a"));
    }
}
