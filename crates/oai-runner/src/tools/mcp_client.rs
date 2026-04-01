use anyhow::{Context, Result};
use rmcp::model::{CallToolRequestParams, RawContent};
use rmcp::service::RunningService;
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::streamable_http_client::{StreamableHttpClientTransport, StreamableHttpClientTransportConfig};
use rmcp::{RoleClient, ServiceExt};
use serde::Deserialize;
use std::borrow::Cow;
use std::sync::Arc;
use tokio::process::Command;

use crate::api::types::{FunctionSchema, ToolDefinition};

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// HTTP endpoint URL. When set, uses HTTP/SSE transport instead of stdio.
    #[serde(default)]
    pub url: Option<String>,
    /// Transport type hint ("stdio" or "http"). Presence of `url` takes precedence.
    #[serde(default)]
    pub transport: Option<String>,
}

pub struct McpClient {
    service: RunningService<RoleClient, ()>,
    tool_names: Vec<String>,
}

pub async fn connect(config: &McpServerConfig) -> Result<McpClient> {
    // Use HTTP transport when a URL is provided or transport is explicitly "http".
    let use_http = config.url.is_some() || config.transport.as_deref().is_some_and(|t| t.eq_ignore_ascii_case("http"));

    if use_http {
        let url = config
            .url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("HTTP MCP server config is missing 'url'"))?;
        let transport = StreamableHttpClientTransport::with_client(
            reqwest::Client::new(),
            StreamableHttpClientTransportConfig::with_uri(url),
        );
        let service: RunningService<RoleClient, ()> =
            ().serve(transport).await.map_err(|e| anyhow::anyhow!("failed to initialize HTTP MCP session: {}", e))?;
        return Ok(McpClient { service, tool_names: Vec::new() });
    }

    let mut cmd = Command::new(&config.command);
    for arg in &config.args {
        cmd.arg(arg);
    }

    let transport = TokioChildProcess::new(cmd).context("failed to spawn MCP server process")?;

    let service: RunningService<RoleClient, ()> =
        ().serve(transport).await.map_err(|e| anyhow::anyhow!("failed to initialize MCP session: {}", e))?;

    Ok(McpClient { service, tool_names: Vec::new() })
}

pub async fn connect_all(configs: &[McpServerConfig]) -> Result<Vec<McpClient>> {
    let mut clients = Vec::with_capacity(configs.len());
    for config in configs {
        clients.push(connect(config).await?);
    }
    Ok(clients)
}

pub async fn fetch_tool_definitions(client: &mut McpClient) -> Result<Vec<ToolDefinition>> {
    let tools =
        client.service.peer().list_all_tools().await.map_err(|e| anyhow::anyhow!("failed to list MCP tools: {}", e))?;

    let mut defs = Vec::new();
    for tool in &tools {
        client.tool_names.push(tool.name.to_string());
        defs.push(mcp_tool_to_openai(tool));
    }
    Ok(defs)
}

pub async fn fetch_all_tool_definitions(clients: &mut [McpClient]) -> Result<Vec<ToolDefinition>> {
    let mut all_defs = Vec::new();
    for client in clients.iter_mut() {
        all_defs.extend(fetch_tool_definitions(client).await?);
    }
    Ok(all_defs)
}

fn mcp_tool_to_openai(tool: &rmcp::model::Tool) -> ToolDefinition {
    let input_schema: &Arc<serde_json::Map<String, serde_json::Value>> = &tool.input_schema;
    let parameters = serde_json::Value::Object((**input_schema).clone());

    ToolDefinition {
        type_: "function".to_string(),
        function: FunctionSchema {
            name: tool.name.to_string(),
            description: tool.description.as_deref().unwrap_or("").to_string(),
            parameters,
        },
    }
}

pub fn find_client_for_tool<'a>(clients: &'a [McpClient], name: &str) -> Option<&'a McpClient> {
    clients.iter().find(|c| c.tool_names.iter().any(|n| n == name))
}

pub async fn call_tool(client: &McpClient, name: &str, args_json: &str) -> Result<String> {
    let args: serde_json::Value =
        serde_json::from_str(args_json).unwrap_or(serde_json::Value::Object(Default::default()));

    let arguments = match args {
        serde_json::Value::Object(map) => Some(map),
        _ => None,
    };

    let mut params = CallToolRequestParams::new(Cow::Owned(name.to_string()));
    if let Some(args) = arguments {
        params = params.with_arguments(args);
    }

    let result = client
        .service
        .peer()
        .call_tool(params)
        .await
        .map_err(|e| anyhow::anyhow!("MCP tool call failed for {}: {}", name, e))?;

    let text_parts: Vec<String> = result
        .content
        .iter()
        .filter_map(|content| match &content.raw {
            RawContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();

    if result.is_error.unwrap_or(false) {
        anyhow::bail!("MCP tool error: {}", text_parts.join("\n"));
    }

    Ok(text_parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tool_to_openai_converts_basic_tool() {
        let schema = Arc::new(serde_json::Map::from_iter([
            ("type".to_string(), serde_json::json!("object")),
            (
                "properties".to_string(),
                serde_json::json!({
                    "query": { "type": "string", "description": "Search query" }
                }),
            ),
            ("required".to_string(), serde_json::json!(["query"])),
        ]));

        let tool = rmcp::model::Tool::new("search", "Search for files", schema);

        let def = mcp_tool_to_openai(&tool);
        assert_eq!(def.type_, "function");
        assert_eq!(def.function.name, "search");
        assert_eq!(def.function.description, "Search for files");
        assert_eq!(def.function.parameters["type"], "object");
        assert!(def.function.parameters["properties"]["query"].is_object());
    }

    #[test]
    fn mcp_tool_to_openai_handles_empty_description() {
        let schema = Arc::new(serde_json::Map::from_iter([
            ("type".to_string(), serde_json::json!("object")),
            ("properties".to_string(), serde_json::json!({})),
        ]));

        let tool = rmcp::model::Tool::new_with_raw("noop", None, schema);

        let def = mcp_tool_to_openai(&tool);
        assert_eq!(def.function.description, "");
    }
}
