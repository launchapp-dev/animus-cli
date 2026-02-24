//! CLI output parsing utilities

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputType {
    Success,
    Error,
    ToolUse(String),
    FileModified(PathBuf),
    Thinking,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedOutput {
    pub output_type: OutputType,
    pub content: String,
    pub metadata: serde_json::Value,
}

pub struct OutputParser;

impl OutputParser {
    /// Parse CLI output and extract structured information
    pub fn parse(output: &str) -> Vec<ParsedOutput> {
        let mut results = Vec::new();

        // Parse tool use patterns (e.g., "Using tool: bash")
        if let Some(tool) = Self::extract_tool_use(output) {
            results.push(ParsedOutput {
                output_type: OutputType::ToolUse(tool.clone()),
                content: tool,
                metadata: serde_json::json!({}),
            });
        }

        // Parse file modifications
        for file in Self::extract_file_modifications(output) {
            results.push(ParsedOutput {
                output_type: OutputType::FileModified(file.clone()),
                content: file.display().to_string(),
                metadata: serde_json::json!({}),
            });
        }

        // Check for errors
        if output.contains("error") || output.contains("Error") {
            results.push(ParsedOutput {
                output_type: OutputType::Error,
                content: output.to_string(),
                metadata: serde_json::json!({}),
            });
        }

        if results.is_empty() {
            results.push(ParsedOutput {
                output_type: OutputType::Unknown,
                content: output.to_string(),
                metadata: serde_json::json!({}),
            });
        }

        results
    }

    fn extract_tool_use(output: &str) -> Option<String> {
        // Simple pattern matching - can be enhanced
        if output.contains("Using tool:") {
            output
                .lines()
                .find(|line| line.contains("Using tool:"))
                .and_then(|line| line.split("Using tool:").nth(1))
                .map(|s| s.trim().to_string())
        } else {
            None
        }
    }

    fn extract_file_modifications(output: &str) -> Vec<PathBuf> {
        let mut files = Vec::new();

        // Look for common file modification patterns
        for line in output.lines() {
            if line.contains("Modified:") || line.contains("Created:") || line.contains("Edited:") {
                if let Some(path) = line.split(':').nth(1) {
                    files.push(PathBuf::from(path.trim()));
                }
            }
        }

        files
    }
}
