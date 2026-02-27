//! MCP Server - Model Context Protocol Server Implementation
//!
//! This library provides a standalone MCP server that can expose tools
//! and resources to AI agents via the Model Context Protocol.
//!
//! # Features
//!
//! - **Search Tool**: Advanced code and file search with regex support
//! - **HTTP Transport**: Expose MCP via HTTP/JSON-RPC
//! - **Dynamic Configuration**: Configure per-agent capabilities
//! - **Gitignore Support**: Respects .gitignore patterns
//!
//! # Example
//!
//! ```no_run
//! use mcp_server::{McpServer, HttpServer};
//! use std::path::PathBuf;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() {
//!     let mcp = McpServer::new(
//!         "my-mcp-server".to_string(),
//!         "1.0.0".to_string(),
//!         PathBuf::from("/path/to/project"),
//!     );
//!
//!     let http_server = HttpServer::new(3000);
//!     http_server
//!         .registry()
//!         .register_agent("default".to_string(), Arc::new(mcp))
//!         .await;
//!     http_server.start().await.unwrap();
//! }
//! ```

pub mod http;
pub mod protocol;
pub mod server;
pub mod tools;

pub use http::HttpServer;
pub use protocol::*;
pub use server::McpServer;
pub use tools::ToolRegistry;

/// MCP Server configuration
#[derive(Debug, Clone)]
pub struct McpConfig {
    pub name: String,
    pub version: String,
    pub root_path: std::path::PathBuf,
    pub port: u16,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            name: "mcp-server".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            root_path: std::env::current_dir().unwrap_or_default(),
            port: 3000,
        }
    }
}

impl McpConfig {
    pub fn new(name: String, root_path: std::path::PathBuf) -> Self {
        Self {
            name,
            version: env!("CARGO_PKG_VERSION").to_string(),
            root_path,
            port: 3000,
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}
