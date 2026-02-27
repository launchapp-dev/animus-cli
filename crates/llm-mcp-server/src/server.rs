//! MCP Server Implementation

use crate::protocol::*;
use crate::tools::ToolRegistry;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// MCP Server state
pub struct McpServer {
    server_info: ServerInfo,
    capabilities: ServerCapabilities,
    tools: Arc<ToolRegistry>,
    initialized: Arc<RwLock<bool>>,
}

impl McpServer {
    /// Create a new MCP server instance
    pub fn new(name: String, version: String, root_path: PathBuf) -> Self {
        let server_info = ServerInfo {
            name,
            version,
            protocol_version: Some("2024-11-05".to_string()),
        };

        let capabilities = ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: Some(false),
            }),
            resources: None,
            prompts: None,
        };

        let tools = Arc::new(ToolRegistry::new(root_path));

        Self {
            server_info,
            capabilities,
            tools,
            initialized: Arc::new(RwLock::new(false)),
        }
    }

    /// Handle incoming JSON-RPC request
    pub async fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        debug!("Handling request: method={}", request.method);

        match request.method.as_str() {
            "initialize" => self.handle_initialize(&request).await,
            "initialized" => self.handle_initialized(&request).await,
            "tools/list" => self.handle_list_tools(&request).await,
            "tools/call" => self.handle_call_tool(&request).await,
            "ping" => JsonRpcResponse::success(request.id, serde_json::json!({ "status": "ok" })),
            _ => {
                warn!("Unknown method: {}", request.method);
                JsonRpcResponse::error(
                    request.id,
                    -32601,
                    format!("Method not found: {}", request.method),
                )
            }
        }
    }

    async fn handle_initialize(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        info!("Initializing MCP server");

        match &request.params {
            Some(params_value) => {
                match serde_json::from_value::<InitializeParams>(params_value.clone()) {
                    Ok(params) => {
                        info!(
                            "Client connected: {} v{}",
                            params.client_info.name, params.client_info.version
                        );

                        let result = InitializeResult {
                            protocol_version: "2024-11-05".to_string(),
                            capabilities: self.capabilities.clone(),
                            server_info: self.server_info.clone(),
                        };

                        JsonRpcResponse::success(
                            request.id.clone(),
                            serde_json::to_value(result).unwrap(),
                        )
                    }
                    Err(e) => JsonRpcResponse::error(
                        request.id.clone(),
                        -32602,
                        format!("Invalid parameters: {}", e),
                    ),
                }
            }
            None => {
                JsonRpcResponse::error(request.id.clone(), -32602, "Missing parameters".to_string())
            }
        }
    }

    async fn handle_initialized(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        *self.initialized.write().await = true;
        info!("MCP server initialized successfully");

        JsonRpcResponse::success(request.id.clone(), Value::Null)
    }

    async fn handle_list_tools(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        if !*self.initialized.read().await {
            return JsonRpcResponse::error(
                request.id.clone(),
                -32002,
                "Server not initialized".to_string(),
            );
        }

        let tools = self.tools.list_tools();
        let result = ListToolsResult { tools };

        JsonRpcResponse::success(request.id.clone(), serde_json::to_value(result).unwrap())
    }

    async fn handle_call_tool(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        if !*self.initialized.read().await {
            return JsonRpcResponse::error(
                request.id.clone(),
                -32002,
                "Server not initialized".to_string(),
            );
        }

        match &request.params {
            Some(params_value) => {
                match serde_json::from_value::<CallToolParams>(params_value.clone()) {
                    Ok(params) => {
                        debug!("Calling tool: {}", params.name);

                        match self.tools.execute_tool(&params).await {
                            Ok(result) => JsonRpcResponse::success(
                                request.id.clone(),
                                serde_json::to_value(result).unwrap(),
                            ),
                            Err(e) => {
                                warn!("Tool execution failed: {}", e);
                                JsonRpcResponse::error(
                                    request.id.clone(),
                                    -32000,
                                    format!("Tool execution failed: {}", e),
                                )
                            }
                        }
                    }
                    Err(e) => JsonRpcResponse::error(
                        request.id.clone(),
                        -32602,
                        format!("Invalid parameters: {}", e),
                    ),
                }
            }
            None => {
                JsonRpcResponse::error(request.id.clone(), -32602, "Missing parameters".to_string())
            }
        }
    }

    /// Check if server is initialized
    pub async fn is_initialized(&self) -> bool {
        *self.initialized.read().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_server_creation() {
        let temp = TempDir::new().unwrap();
        let server = McpServer::new(
            "test-server".to_string(),
            "1.0.0".to_string(),
            temp.path().to_path_buf(),
        );

        assert_eq!(server.server_info.name, "test-server");
        assert!(!server.is_initialized().await);
    }

    #[tokio::test]
    async fn test_initialize_request() {
        let temp = TempDir::new().unwrap();
        let server = McpServer::new(
            "test-server".to_string(),
            "1.0.0".to_string(),
            temp.path().to_path_buf(),
        );

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            })),
        };

        let response = server.handle_request(request).await;
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn test_list_tools() {
        let temp = TempDir::new().unwrap();
        let server = McpServer::new(
            "test-server".to_string(),
            "1.0.0".to_string(),
            temp.path().to_path_buf(),
        );

        // Initialize first
        *server.initialized.write().await = true;

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(2)),
            method: "tools/list".to_string(),
            params: None,
        };

        let response = server.handle_request(request).await;
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }
}
