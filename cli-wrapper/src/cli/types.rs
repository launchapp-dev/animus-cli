//! Common types for CLI management

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Supported CLI types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliType {
    Claude,
    Codex,
    Gemini,
    OpenCode,
    Aider,
    Cursor,
    Cline,
    Custom,
}

impl CliType {
    pub fn executable_name(&self) -> &str {
        match self {
            CliType::Claude => "claude",
            CliType::Codex => "codex",
            CliType::Gemini => "gemini",
            CliType::OpenCode => "opencode",
            CliType::Aider => "aider",
            CliType::Cursor => "cursor",
            CliType::Cline => "cline",
            CliType::Custom => "custom",
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            CliType::Claude => "Claude Code",
            CliType::Codex => "OpenAI Codex",
            CliType::Gemini => "Google Gemini CLI",
            CliType::OpenCode => "OpenCode",
            CliType::Aider => "Aider",
            CliType::Cursor => "Cursor CLI",
            CliType::Cline => "Cline",
            CliType::Custom => "Custom CLI",
        }
    }
}

/// CLI capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliCapability {
    pub supports_file_editing: bool,
    pub supports_streaming: bool,
    pub supports_tool_use: bool,
    pub supports_vision: bool,
    pub supports_long_context: bool,
    pub max_context_tokens: Option<usize>,
    pub supports_mcp: bool,
    pub mcp_endpoint: Option<String>,
}

/// CLI status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliStatus {
    Available,
    NotInstalled,
    NotAuthenticated,
    Error(String),
}

/// CLI metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliMetadata {
    pub cli_type: CliType,
    pub version: Option<String>,
    pub executable_path: PathBuf,
    pub capabilities: CliCapability,
    pub status: CliStatus,
    pub last_tested: Option<chrono::DateTime<chrono::Utc>>,
}

impl CliMetadata {
    pub fn new(cli_type: CliType, executable_path: PathBuf) -> Self {
        Self {
            cli_type,
            version: None,
            executable_path,
            capabilities: CliCapability::default_for_type(cli_type),
            status: CliStatus::Available,
            last_tested: None,
        }
    }

    pub fn is_usable(&self) -> bool {
        matches!(self.status, CliStatus::Available)
    }
}

impl CliCapability {
    pub fn default_for_type(cli_type: CliType) -> Self {
        match cli_type {
            CliType::Claude => Self {
                supports_file_editing: true,
                supports_streaming: true,
                supports_tool_use: true,
                supports_vision: true,
                supports_long_context: true,
                max_context_tokens: Some(200_000),
                supports_mcp: true,
                mcp_endpoint: None,
            },
            CliType::Codex => Self {
                supports_file_editing: true,
                supports_streaming: true,
                supports_tool_use: true,
                supports_vision: false,
                supports_long_context: false,
                max_context_tokens: Some(128_000),
                supports_mcp: false,
                mcp_endpoint: None,
            },
            CliType::Gemini => Self {
                supports_file_editing: true,
                supports_streaming: true,
                supports_tool_use: true,
                supports_vision: true,
                supports_long_context: true,
                max_context_tokens: Some(1_000_000),
                supports_mcp: false,
                mcp_endpoint: None,
            },
            CliType::OpenCode => Self {
                supports_file_editing: true,
                supports_streaming: true,
                supports_tool_use: true,
                supports_vision: false,
                supports_long_context: true,
                max_context_tokens: Some(200_000), // Supports multiple models
                supports_mcp: true,
                mcp_endpoint: None,
            },
            CliType::Aider => Self {
                supports_file_editing: true,
                supports_streaming: true,
                supports_tool_use: false,
                supports_vision: false,
                supports_long_context: false,
                max_context_tokens: Some(128_000),
                supports_mcp: false,
                mcp_endpoint: None,
            },
            _ => Self {
                supports_file_editing: false,
                supports_streaming: false,
                supports_tool_use: false,
                supports_vision: false,
                supports_long_context: false,
                max_context_tokens: None,
                supports_mcp: false,
                mcp_endpoint: None,
            },
        }
    }
}
