//! Round-trip test for the reusable [`animus_mcp_oauth::McpSession`] client.
//!
//! A tiny in-process **stub MCP server** (answering `initialize` +
//! `tools/list` + `tools/call`) is served over an in-memory duplex byte stream
//! — the same newline-delimited JSON-RPC framing the `animus-mcp-proxy` stdio
//! child speaks. The `McpSession` client connects over the other end via
//! [`McpSession::connect_transport`] and drives the full
//! `initialize` -> `notifications/initialized` -> `tools/list` -> `tools/call`
//! handshake, proving the shared client behind `animus mcp tools` /
//! `animus mcp call` works end to end without any network, keychain, or
//! spawned binary.

use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult, Content, ListToolsResult, Tool};
use rmcp::service::RequestContext;
use rmcp::{serve_server, RoleServer, ServerHandler};
use serde_json::json;

/// A minimal MCP server exposing a single `echo` tool that returns its `text`
/// argument back as text content.
#[derive(Clone)]
struct StubServer;

impl ServerHandler for StubServer {
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let schema = match json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        }) {
            serde_json::Value::Object(map) => map,
            _ => unreachable!(),
        };
        let tool = Tool::new("echo", "Echo the provided text back.", Arc::new(schema));
        Ok(ListToolsResult::with_all_items(vec![tool]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if request.name != "echo" {
            return Err(rmcp::ErrorData::invalid_params(format!("unknown tool `{}`", request.name), None));
        }
        let text = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("text"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(CallToolResult::success(vec![Content::text(format!("echo: {text}"))]))
    }
}

/// Spawn the stub server on one end of an in-memory duplex and return an
/// `McpSession` connected to the other end.
async fn connect_to_stub() -> animus_mcp_oauth::McpSession {
    let (client_io, server_io) = tokio::io::duplex(8 * 1024);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    // Serve the stub until the client disconnects.
    tokio::spawn(async move {
        if let Ok(running) = serve_server(StubServer, (server_read, server_write)).await {
            let _ = running.waiting().await;
        }
    });

    animus_mcp_oauth::McpSession::connect_transport((client_read, client_write), Duration::from_secs(5))
        .await
        .expect("client connects + completes the initialize handshake against the stub server")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_round_trips_through_the_session() {
    let session = connect_to_stub().await;

    let tools = session.list_tools().await.expect("tools/list succeeds");
    assert_eq!(tools.len(), 1, "stub exposes exactly one tool");
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].description.as_deref(), Some("Echo the provided text back."));

    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_call_round_trips_through_the_session() {
    let session = connect_to_stub().await;

    let args = match json!({ "text": "hello" }) {
        serde_json::Value::Object(map) => map,
        _ => unreachable!(),
    };
    let result = session.call_tool("echo", Some(args)).await.expect("tools/call succeeds");

    assert_ne!(result.is_error, Some(true), "a successful echo must not be flagged as an error");
    let text = result
        .content
        .iter()
        .find_map(|content| serde_json::to_value(&content.raw).ok())
        .and_then(|value| value.get("text").and_then(|t| t.as_str()).map(ToOwned::to_owned))
        .expect("the echo result carries text content");
    assert_eq!(text, "echo: hello");

    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_call_with_no_arguments_is_accepted() {
    // The stub's echo tool tolerates a missing `text` argument; this proves the
    // `None` (no-argument) call path shapes a valid `tools/call` request.
    let session = connect_to_stub().await;

    let result = session.call_tool("echo", None).await.expect("no-argument tools/call succeeds");
    let text = result
        .content
        .iter()
        .find_map(|content| serde_json::to_value(&content.raw).ok())
        .and_then(|value| value.get("text").and_then(|t| t.as_str()).map(ToOwned::to_owned))
        .expect("the echo result carries text content");
    assert_eq!(text, "echo: ");

    session.shutdown().await;
}
