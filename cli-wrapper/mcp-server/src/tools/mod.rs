//! MCP Tools - Available tools for agents

pub mod search;

use crate::protocol::{CallToolParams, CallToolResult, Tool};
use anyhow::Result;
use std::path::PathBuf;

pub use search::SearchTool;

/// Tool registry for managing available tools
pub struct ToolRegistry {
    search_tool: SearchTool,
}

impl ToolRegistry {
    pub fn new(root_path: PathBuf) -> Self {
        Self {
            search_tool: SearchTool::new(root_path),
        }
    }

    /// Get list of all available tools
    pub fn list_tools(&self) -> Vec<Tool> {
        vec![
            SearchTool::definition(),
        ]
    }

    /// Execute a tool by name
    pub async fn execute_tool(&self, params: &CallToolParams) -> Result<CallToolResult> {
        match params.name.as_str() {
            "search" => self.search_tool.execute(params).await,
            _ => Ok(CallToolResult {
                content: vec![crate::protocol::ToolContent::Text {
                    text: format!("Unknown tool: {}", params.name),
                }],
                is_error: Some(true),
            }),
        }
    }
}
