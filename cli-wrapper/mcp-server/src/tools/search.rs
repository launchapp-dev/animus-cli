//! Search Tool - Advanced code and file search capabilities

use crate::protocol::{CallToolParams, CallToolResult, Tool, ToolContent, ToolInputSchema};
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchArgs {
    pub query: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub file_pattern: Option<String>,
    #[serde(default)]
    pub regex: Option<bool>,
    #[serde(default)]
    pub case_sensitive: Option<bool>,
    #[serde(default)]
    pub max_results: Option<usize>,
}

pub struct SearchTool {
    root_path: PathBuf,
}

impl SearchTool {
    pub fn new(root_path: PathBuf) -> Self {
        Self { root_path }
    }

    pub fn definition() -> Tool {
        let mut properties = serde_json::Map::new();

        properties.insert(
            "query".to_string(),
            serde_json::json!({
                "type": "string",
                "description": "Search query or pattern"
            }),
        );

        properties.insert(
            "path".to_string(),
            serde_json::json!({
                "type": "string",
                "description": "Optional path to search in (relative to root)"
            }),
        );

        properties.insert(
            "file_pattern".to_string(),
            serde_json::json!({
                "type": "string",
                "description": "Optional file pattern (e.g., *.rs, *.md)"
            }),
        );

        properties.insert(
            "regex".to_string(),
            serde_json::json!({
                "type": "boolean",
                "description": "Whether query is a regex pattern"
            }),
        );

        properties.insert(
            "case_sensitive".to_string(),
            serde_json::json!({
                "type": "boolean",
                "description": "Case-sensitive search"
            }),
        );

        properties.insert(
            "max_results".to_string(),
            serde_json::json!({
                "type": "integer",
                "description": "Maximum number of results to return"
            }),
        );

        Tool {
            name: "search".to_string(),
            description: "Search for files and code content in the project. Supports glob patterns, regex, and respects .gitignore.".to_string(),
            input_schema: ToolInputSchema {
                schema_type: "object".to_string(),
                properties,
                required: Some(vec!["query".to_string()]),
            },
        }
    }

    pub async fn execute(&self, params: &CallToolParams) -> Result<CallToolResult> {
        let args: SearchArgs = serde_json::from_value(
            params.arguments.clone().unwrap_or(Value::Null)
        ).context("Failed to parse search arguments")?;

        debug!("Executing search: query={}, path={:?}", args.query, args.path);

        let search_path = if let Some(ref path) = args.path {
            self.root_path.join(path)
        } else {
            self.root_path.clone()
        };

        if !search_path.exists() {
            return Ok(CallToolResult {
                content: vec![ToolContent::Text {
                    text: format!("Path does not exist: {}", search_path.display()),
                }],
                is_error: Some(true),
            });
        }

        let results = self.search(&search_path, &args).await?;

        let text = if results.is_empty() {
            "No results found.".to_string()
        } else {
            format!("Found {} result(s):\n\n{}", results.len(), results.join("\n\n"))
        };

        Ok(CallToolResult {
            content: vec![ToolContent::Text { text }],
            is_error: None,
        })
    }

    async fn search(&self, path: &Path, args: &SearchArgs) -> Result<Vec<String>> {
        let mut results = Vec::new();
        let max_results = args.max_results.unwrap_or(100);

        // Build regex pattern
        let pattern = if args.regex.unwrap_or(false) {
            let regex_str = &args.query;
            Regex::new(regex_str)?
        } else {
            let escaped = regex::escape(&args.query);
            if args.case_sensitive.unwrap_or(false) {
                Regex::new(&escaped)?
            } else {
                Regex::new(&format!("(?i){}", escaped))?
            }
        };

        // Build walker with gitignore support
        let walker = WalkBuilder::new(path)
            .hidden(false)
            .git_ignore(true)
            .build();

        for entry in walker {
            if results.len() >= max_results {
                break;
            }

            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Walk error: {}", e);
                    continue;
                }
            };

            let file_path = entry.path();

            // Skip directories
            if file_path.is_dir() {
                continue;
            }

            // Check file pattern if specified
            if let Some(ref file_pat) = args.file_pattern {
                if let Some(file_name) = file_path.file_name().and_then(|n| n.to_str()) {
                    if !glob_match(file_pat, file_name) {
                        continue;
                    }
                }
            }

            // Search in file content
            if let Ok(content) = tokio::fs::read_to_string(file_path).await {
                let matches = self.find_matches(&content, &pattern);
                if !matches.is_empty() {
                    let relative_path = file_path.strip_prefix(&self.root_path)
                        .unwrap_or(file_path);

                    let result = format!(
                        "📄 {}\n{}",
                        relative_path.display(),
                        matches.join("\n")
                    );
                    results.push(result);
                }
            }
        }

        Ok(results)
    }

    fn find_matches(&self, content: &str, pattern: &Regex) -> Vec<String> {
        let mut matches = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            if pattern.is_match(line) {
                matches.push(format!("  Line {}: {}", line_num + 1, line.trim()));

                if matches.len() >= 5 {
                    matches.push("  ... (more matches in this file)".to_string());
                    break;
                }
            }
        }

        matches
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    // Simple glob matching (* and ?)
    let regex_pattern = pattern
        .replace(".", "\\.")
        .replace("*", ".*")
        .replace("?", ".");

    if let Ok(re) = Regex::new(&format!("^{}$", regex_pattern)) {
        re.is_match(text)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("*.rs", "lib.rs"));
        assert!(!glob_match("*.rs", "main.ts"));
        assert!(glob_match("test_*.rs", "test_foo.rs"));
        assert!(!glob_match("test_*.rs", "foo_test.rs"));
    }

    #[tokio::test]
    async fn test_search_tool_definition() {
        let tool = SearchTool::definition();
        assert_eq!(tool.name, "search");
        assert!(tool.description.contains("Search"));
    }
}
