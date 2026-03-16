use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::workflow_config::{McpServerDefinition, PhaseMcpBinding, WorkflowConfig};

use super::loading::LoadedPackManifest;

#[derive(Debug, Clone, Default)]
pub struct PackMcpOverlay {
    pub servers: BTreeMap<String, McpServerDefinition>,
    pub phase_mcp_bindings: BTreeMap<String, PhaseMcpBinding>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackMcpServersFile {
    #[serde(default)]
    server: Vec<PackMcpServerDescriptor>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackMcpServerDescriptor {
    id: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    required_env: Vec<String>,
    #[serde(default)]
    tool_namespace: Option<String>,
    #[serde(default)]
    startup: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PackMcpBindingsFile {
    #[serde(default)]
    phase: BTreeMap<String, PackMcpPhaseBinding>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PackMcpPhaseBinding {
    #[serde(default)]
    servers: Vec<String>,
}

pub fn load_pack_mcp_overlay(pack: &LoadedPackManifest) -> Result<PackMcpOverlay> {
    let Some(mcp_assets) = pack.manifest.mcp.as_ref() else {
        return Ok(PackMcpOverlay::default());
    };

    let server_descriptors = if let Some(servers_path) = mcp_assets.servers.as_deref() {
        let raw = fs::read_to_string(pack.pack_root.join(servers_path))
            .with_context(|| format!("failed to read {}", servers_path))?;
        parse_pack_mcp_servers(&raw)?
    } else {
        Vec::new()
    };

    let mut local_server_ids = BTreeSet::new();
    let mut servers = BTreeMap::new();
    for descriptor in server_descriptors {
        let local_id = descriptor.id.trim();
        validate_local_server_id(local_id)?;
        if !local_server_ids.insert(local_id.to_ascii_lowercase()) {
            return Err(anyhow!("pack '{}' declares duplicate MCP server id '{}'", pack.manifest.id, local_id));
        }

        let namespaced = namespaced_server_id(&pack.manifest.id, local_id);
        let mut config = BTreeMap::<String, Value>::new();
        if !descriptor.required_env.is_empty() {
            config.insert("required_env".to_string(), json!(descriptor.required_env));
        }
        if let Some(tool_namespace) = descriptor.tool_namespace.as_deref() {
            if tool_namespace.trim().is_empty() {
                return Err(anyhow!("pack '{}' MCP server '{}' has empty tool_namespace", pack.manifest.id, local_id));
            }
            config.insert("tool_namespace".to_string(), Value::String(tool_namespace.trim().to_string()));
        }
        if let Some(startup) = descriptor.startup.as_deref() {
            validate_startup_mode(startup)?;
            config.insert("startup".to_string(), Value::String(startup.trim().to_string()));
        }

        servers.insert(
            namespaced,
            McpServerDefinition {
                command: descriptor.command,
                args: descriptor.args,
                transport: descriptor.transport,
                config,
                tools: descriptor.tools,
                env: descriptor.env,
            },
        );
    }

    let phase_mcp_bindings = if let Some(bindings_path) = mcp_assets.tools.as_deref() {
        let raw = fs::read_to_string(pack.pack_root.join(bindings_path))
            .with_context(|| format!("failed to read {}", bindings_path))?;
        parse_phase_bindings(&raw, &pack.manifest.id, &local_server_ids)?
    } else {
        BTreeMap::new()
    };

    Ok(PackMcpOverlay { servers, phase_mcp_bindings })
}

pub fn apply_pack_mcp_overlay(workflow: &mut WorkflowConfig, pack: &LoadedPackManifest) -> Result<()> {
    let overlay = load_pack_mcp_overlay(pack)?;

    for (server_id, definition) in overlay.servers {
        workflow.mcp_servers.insert(server_id, definition);
    }

    for (phase_id, binding) in overlay.phase_mcp_bindings {
        let entry = workflow.phase_mcp_bindings.entry(phase_id).or_default();
        let mut merged = entry.servers.clone();
        merged.extend(binding.servers);
        merged.sort();
        merged.dedup();
        entry.servers = merged;
    }

    Ok(())
}

fn parse_pack_mcp_servers(raw_toml: &str) -> Result<Vec<PackMcpServerDescriptor>> {
    let file: PackMcpServersFile = toml::from_str(raw_toml).context("failed to parse MCP servers TOML")?;

    for server in &file.server {
        if server.command.trim().is_empty() {
            return Err(anyhow!("MCP server '{}' command must not be empty", server.id));
        }
        if server.args.iter().any(|arg| arg.trim().is_empty()) {
            return Err(anyhow!("MCP server '{}' args must not contain empty values", server.id));
        }
        if server.tools.iter().any(|tool| tool.trim().is_empty()) {
            return Err(anyhow!("MCP server '{}' tools must not contain empty values", server.id));
        }
        if server.env.iter().any(|(key, value)| key.trim().is_empty() || value.trim().is_empty()) {
            return Err(anyhow!("MCP server '{}' env must not contain empty keys or values", server.id));
        }
        if server.required_env.iter().any(|key| key.trim().is_empty()) {
            return Err(anyhow!("MCP server '{}' required_env must not contain empty values", server.id));
        }
    }

    Ok(file.server)
}

fn parse_phase_bindings(
    raw_toml: &str,
    pack_id: &str,
    local_server_ids: &BTreeSet<String>,
) -> Result<BTreeMap<String, PhaseMcpBinding>> {
    let file: PackMcpBindingsFile = toml::from_str(raw_toml).context("failed to parse MCP phase bindings TOML")?;
    let mut bindings = BTreeMap::new();

    for (phase_id, binding) in file.phase {
        if phase_id.trim().is_empty() {
            return Err(anyhow!("phase MCP bindings must not contain empty phase ids"));
        }
        if binding.servers.is_empty() {
            return Err(anyhow!("phase MCP binding '{}' must include at least one local server id", phase_id));
        }

        let mut namespaced_servers = Vec::new();
        let mut seen = BTreeSet::new();
        for server_id in binding.servers {
            let trimmed = server_id.trim();
            validate_local_server_id(trimmed)?;
            let normalized = trimmed.to_ascii_lowercase();
            if !local_server_ids.contains(&normalized) {
                return Err(anyhow!("phase MCP binding '{}' references unknown local server '{}'", phase_id, trimmed));
            }
            let namespaced = namespaced_server_id(pack_id, trimmed);
            if seen.insert(namespaced.to_ascii_lowercase()) {
                namespaced_servers.push(namespaced);
            }
        }
        bindings.insert(phase_id, PhaseMcpBinding { servers: namespaced_servers });
    }

    Ok(bindings)
}

fn namespaced_server_id(pack_id: &str, local_id: &str) -> String {
    format!("{}/{}", pack_id.trim(), local_id.trim())
}

fn validate_local_server_id(server_id: &str) -> Result<()> {
    if server_id.is_empty() {
        return Err(anyhow!("MCP server id must not be empty"));
    }
    if !server_id
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_' || ch == '.')
    {
        return Err(anyhow!("MCP server id '{}' must use lowercase letters, numbers, '.', '-' or '_'", server_id));
    }
    Ok(())
}

fn validate_startup_mode(raw: &str) -> Result<()> {
    match raw.trim() {
        "phase-local" | "workflow-local" => Ok(()),
        other => {
            Err(anyhow!("MCP startup mode '{}' is not supported (expected 'phase-local' or 'workflow-local')", other))
        }
    }
}
