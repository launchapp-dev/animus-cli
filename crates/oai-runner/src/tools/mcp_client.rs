use anyhow::{Context, Result};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, RawAudioContent, RawContent, RawEmbeddedResource,
    RawImageContent, RawResource, ResourceContents,
};
use rmcp::service::RunningService;
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::{RoleClient, ServiceExt};
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::process::Command;

use crate::api::types::{FunctionSchema, ToolDefinition};

const MCP_OUTPUT_CHAR_LIMIT: usize = 16_384;
const MCP_OUTPUT_TRUNCATION_SUFFIX: &str = "\n...[MCP output truncated]";

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        auth_token: Option<String>,
    },
}

pub struct McpClient {
    service: RunningService<RoleClient, ()>,
    tool_names: Vec<String>,
    transport_label: String,
}

pub async fn connect(config: &McpServerConfig) -> Result<McpClient> {
    match config {
        McpServerConfig::Stdio { command, args, env } => connect_stdio(command, args, env).await,
        McpServerConfig::StreamableHttp { url, auth_token } => {
            connect_streamable_http(url, auth_token.as_deref()).await
        }
    }
}

async fn connect_stdio(command: &str, args: &[String], env: &BTreeMap<String, String>) -> Result<McpClient> {
    let transport_label = format!("stdio process {}", describe_command(command, args));

    let mut cmd = Command::new(command);
    cmd.args(args);
    cmd.envs(env);

    let transport = TokioChildProcess::new(cmd).with_context(|| {
        format!("failed to spawn {transport_label}")
    })?;

    let service: RunningService<RoleClient, ()> = ().serve::<_, _, _>(transport).await.map_err(|e| {
        anyhow::anyhow!("failed to initialize {transport_label} MCP session: {e}")
    })?;

    Ok(McpClient {
        service,
        tool_names: Vec::new(),
        transport_label,
    })
}

async fn connect_streamable_http(url: &str, auth_token: Option<&str>) -> Result<McpClient> {
    let transport_label = format!("streamable-http endpoint {}", quote_for_display(url));
    let mut transport_config = StreamableHttpClientTransportConfig::with_uri(url.to_string());
    if let Some(auth_token) = auth_token {
        transport_config = transport_config.auth_header(auth_token.to_string());
    }

    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let service: RunningService<RoleClient, ()> = ().serve::<_, _, _>(transport).await.map_err(|e| {
        anyhow::anyhow!("failed to initialize {transport_label} MCP session: {e}")
    })?;

    Ok(McpClient {
        service,
        tool_names: Vec::new(),
        transport_label,
    })
}

pub async fn connect_all(configs: &[McpServerConfig]) -> Result<Vec<McpClient>> {
    let mut clients = Vec::with_capacity(configs.len());
    for config in configs {
        clients.push(connect(config).await?);
    }
    Ok(clients)
}

pub async fn fetch_tool_definitions(client: &mut McpClient) -> Result<Vec<ToolDefinition>> {
    let tools = client
        .service
        .peer()
        .list_all_tools()
        .await
        .map_err(|e| anyhow::anyhow!("failed to list MCP tools from {}: {e}", client.transport_label))?;

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
        .map_err(|e| anyhow::anyhow!("MCP tool call failed for {name} via {}: {e}", client.transport_label))?;

    let rendered = render_call_tool_result(&result);

    if result.is_error.unwrap_or(false) {
        let rendered = if rendered.is_empty() {
            "tool returned an error with no content".to_string()
        } else {
            rendered
        };
        anyhow::bail!("MCP tool error from {} for {name}: {rendered}", client.transport_label);
    }

    Ok(rendered)
}

fn render_call_tool_result(result: &CallToolResult) -> String {
    let mut output = String::new();
    let mut truncated = false;
    let content_limit = MCP_OUTPUT_CHAR_LIMIT.saturating_sub(MCP_OUTPUT_TRUNCATION_SUFFIX.len());

    if let Some(structured_content) = &result.structured_content {
        push_limited(
            &mut output,
            &format!("[structured_content] {structured_content}"),
            content_limit,
            &mut truncated,
        );
    }

    for content in &result.content {
        if !output.is_empty() {
            push_limited(&mut output, "\n", content_limit, &mut truncated);
        }
        push_limited(
            &mut output,
            &format_raw_content(&content.raw),
            content_limit,
            &mut truncated,
        );
    }

    if truncated {
        output.push_str(MCP_OUTPUT_TRUNCATION_SUFFIX);
    }

    output
}

fn format_raw_content(content: &RawContent) -> String {
    match content {
        RawContent::Text(text) => text.text.clone(),
        RawContent::Image(RawImageContent { data, mime_type, .. }) => {
            format!("[image mime_type={} base64_chars={}]", quote_for_display(mime_type), data.len())
        }
        RawContent::Resource(RawEmbeddedResource { resource, .. }) => format_resource_contents(resource),
        RawContent::Audio(RawAudioContent { data, mime_type }) => {
            format!("[audio mime_type={} base64_chars={}]", quote_for_display(mime_type), data.len())
        }
        RawContent::ResourceLink(resource) => format_resource_link(resource),
    }
}

fn format_resource_contents(resource: &ResourceContents) -> String {
    match resource {
        ResourceContents::TextResourceContents {
            uri,
            mime_type,
            text,
            ..
        } => {
            let mut parts = vec![format!("uri={}", quote_for_display(uri))];
            if let Some(mime_type) = mime_type.as_deref() {
                parts.push(format!("mime_type={}", quote_for_display(mime_type)));
            }

            let header = format!("[resource {}]", parts.join(" "));
            if text.is_empty() {
                header
            } else {
                format!("{header}\n{text}")
            }
        }
        ResourceContents::BlobResourceContents {
            uri,
            mime_type,
            blob,
            ..
        } => {
            let mut parts = vec![format!("uri={}", quote_for_display(uri))];
            if let Some(mime_type) = mime_type.as_deref() {
                parts.push(format!("mime_type={}", quote_for_display(mime_type)));
            }
            parts.push(format!("blob_base64_chars={}", blob.len()));
            format!("[resource {}]", parts.join(" "))
        }
    }
}

fn format_resource_link(resource: &RawResource) -> String {
    let mut parts = vec![
        format!("uri={}", quote_for_display(&resource.uri)),
        format!("name={}", quote_for_display(&resource.name)),
    ];

    if let Some(title) = resource.title.as_deref() {
        parts.push(format!("title={}", quote_for_display(title)));
    }
    if let Some(description) = resource.description.as_deref() {
        parts.push(format!("description={}", quote_for_display(description)));
    }
    if let Some(mime_type) = resource.mime_type.as_deref() {
        parts.push(format!("mime_type={}", quote_for_display(mime_type)));
    }
    if let Some(size) = resource.size {
        parts.push(format!("size={size}"));
    }

    format!("[resource-link {}]", parts.join(" "))
}

fn push_limited(output: &mut String, piece: &str, limit: usize, truncated: &mut bool) {
    if *truncated {
        return;
    }

    let remaining = limit.saturating_sub(output.len());
    if piece.len() <= remaining {
        output.push_str(piece);
        return;
    }

    if remaining > 0 {
        output.push_str(truncate_to_boundary(piece, remaining));
    }
    *truncated = true;
}

fn truncate_to_boundary(text: &str, max_len: usize) -> &str {
    if text.len() <= max_len {
        return text;
    }

    let mut end = max_len.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn describe_command(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(quote_for_display)
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_for_display(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{CallToolResult, Content};

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

    #[test]
    fn render_call_tool_result_preserves_non_text_content() {
        let result = CallToolResult::success(vec![
            Content::text("hello"),
            Content::image("abcd", "image/png"),
            Content::resource(ResourceContents::TextResourceContents {
                uri: "file:///tmp/example.txt".to_string(),
                mime_type: Some("text/plain".to_string()),
                text: "resource text".to_string(),
                meta: None,
            }),
            Content::resource_link(
                RawResource::new("file:///tmp/data.json", "data")
                    .with_description("linked resource")
                    .with_mime_type("application/json")
                    .with_size(12),
            ),
        ]);

        let rendered = render_call_tool_result(&result);

        assert!(rendered.contains("hello"));
        assert!(rendered.contains("[image mime_type=\"image/png\" base64_chars=4]"));
        assert!(rendered.contains("[resource uri=\"file:///tmp/example.txt\" mime_type=\"text/plain\"]"));
        assert!(rendered.contains("resource text"));
        assert!(rendered.contains("[resource-link uri=\"file:///tmp/data.json\" name=\"data\""));
        assert!(rendered.contains("description=\"linked resource\""));
    }

    #[test]
    fn render_call_tool_result_truncates_large_output() {
        let result = CallToolResult::success(vec![Content::text("a".repeat(50_000))]);

        let rendered = render_call_tool_result(&result);

        assert!(rendered.len() <= MCP_OUTPUT_CHAR_LIMIT);
        assert!(rendered.ends_with(MCP_OUTPUT_TRUNCATION_SUFFIX));
    }

    #[test]
    fn parses_stdio_and_streamable_http_configs() {
        let stdio: McpServerConfig = serde_json::from_value(serde_json::json!({
            "command": "npx",
            "args": ["--yes", "@modelcontextprotocol/server-filesystem"]
        }))
        .expect("stdio config should parse");

        let http: McpServerConfig = serde_json::from_value(serde_json::json!({
            "url": "http://localhost:8000/mcp",
            "auth_token": "secret"
        }))
        .expect("http config should parse");

        match stdio {
            McpServerConfig::Stdio { command, args } => {
                assert_eq!(command, "npx");
                assert_eq!(args.len(), 2);
            }
            _ => panic!("expected stdio config"),
        }

        match http {
            McpServerConfig::StreamableHttp { url, auth_token } => {
                assert_eq!(url, "http://localhost:8000/mcp");
                assert_eq!(auth_token.as_deref(), Some("secret"));
            }
            _ => panic!("expected streamable-http config"),
        }
    }
}
