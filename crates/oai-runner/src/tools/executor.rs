use anyhow::{bail, Result};
use serde_json::Value;
use std::path::Path;

use super::{bash, file_ops, search};

pub async fn execute_tool(name: &str, args_json: &str, working_dir: &Path) -> Result<String> {
    let args: Value = serde_json::from_str(args_json).unwrap_or(Value::Object(Default::default()));

    match name {
        "read_file" => {
            let path = get_str(&args, "path")?;
            let offset = args.get("offset").and_then(|v| v.as_u64()).map(|v| v as usize);
            let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);
            file_ops::read_file(working_dir, path, offset, limit)
        }
        "write_file" => {
            let path = get_str(&args, "path")?;
            let content = get_str(&args, "content")?;
            file_ops::write_file(working_dir, path, content)
        }
        "edit_file" => {
            let path = get_str(&args, "path")?;
            let old_text = get_str(&args, "old_text")?;
            let new_text = get_str(&args, "new_text")?;
            file_ops::edit_file(working_dir, path, old_text, new_text)
        }
        "list_files" => {
            let pattern = get_str(&args, "pattern")?;
            let base_path = args.get("path").and_then(|v| v.as_str());
            file_ops::list_files(working_dir, pattern, base_path)
        }
        "search_files" => {
            let pattern = get_str(&args, "pattern")?;
            let search_path = args.get("path").and_then(|v| v.as_str());
            let include = args.get("include").and_then(|v| v.as_str());
            search::search_files(working_dir, pattern, search_path, include)
        }
        "execute_command" => {
            let command = get_str(&args, "command")?;
            let timeout = args.get("timeout_secs").and_then(|v| v.as_u64());
            bash::execute_command(working_dir, command, timeout).await
        }
        _ => bail!("Unknown tool: {}", name),
    }
}

fn get_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Missing required parameter: {}", key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_temp_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.txt"), "line one\nline two\nline three\n").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[tokio::test]
    async fn read_file_returns_content_with_line_numbers() {
        let dir = setup_temp_dir();
        let result = execute_tool("read_file", r#"{"path": "test.txt"}"#, dir.path()).await.unwrap();
        assert!(result.contains("line one"));
        assert!(result.contains("line two"));
        assert!(result.contains("1\t"));
    }

    #[tokio::test]
    async fn read_file_supports_offset_and_limit() {
        let dir = setup_temp_dir();
        let result =
            execute_tool("read_file", r#"{"path": "test.txt", "offset": 2, "limit": 1}"#, dir.path()).await.unwrap();
        assert!(result.contains("line two"));
        assert!(!result.contains("line one"));
        assert!(!result.contains("line three"));
    }

    #[tokio::test]
    async fn write_file_creates_new_file() {
        let dir = setup_temp_dir();
        let result =
            execute_tool("write_file", r#"{"path": "new.txt", "content": "hello world"}"#, dir.path()).await.unwrap();
        assert!(result.contains("Successfully wrote"));
        let content = fs::read_to_string(dir.path().join("new.txt")).unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn write_file_creates_parent_directories() {
        let dir = setup_temp_dir();
        execute_tool("write_file", r#"{"path": "deep/nested/dir/file.txt", "content": "nested"}"#, dir.path())
            .await
            .unwrap();
        let content = fs::read_to_string(dir.path().join("deep/nested/dir/file.txt")).unwrap();
        assert_eq!(content, "nested");
    }

    #[tokio::test]
    async fn edit_file_replaces_text() {
        let dir = setup_temp_dir();
        let result = execute_tool(
            "edit_file",
            r#"{"path": "test.txt", "old_text": "line two", "new_text": "LINE TWO"}"#,
            dir.path(),
        )
        .await
        .unwrap();
        assert!(result.contains("Successfully edited"));
        let content = fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert!(content.contains("LINE TWO"));
        assert!(!content.contains("line two"));
    }

    #[tokio::test]
    async fn edit_file_fails_when_old_text_not_found() {
        let dir = setup_temp_dir();
        let result = execute_tool(
            "edit_file",
            r#"{"path": "test.txt", "old_text": "nonexistent", "new_text": "replacement"}"#,
            dir.path(),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn list_files_matches_glob_pattern() {
        let dir = setup_temp_dir();
        let result = execute_tool("list_files", r#"{"pattern": "**/*.rs"}"#, dir.path()).await.unwrap();
        assert!(result.contains("src/main.rs"));
    }

    #[tokio::test]
    async fn search_files_finds_matching_content() {
        let dir = setup_temp_dir();
        let result = execute_tool("search_files", r#"{"pattern": "fn main"}"#, dir.path()).await.unwrap();
        assert!(result.contains("fn main"));
    }

    #[tokio::test]
    async fn execute_command_runs_shell_commands() {
        let dir = setup_temp_dir();
        let result = execute_tool("execute_command", r#"{"command": "echo hello"}"#, dir.path()).await.unwrap();
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn execute_command_captures_exit_code() {
        let dir = setup_temp_dir();
        let result = execute_tool("execute_command", r#"{"command": "exit 42"}"#, dir.path()).await.unwrap();
        assert!(result.contains("[exit code: 42]"));
    }

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        let dir = setup_temp_dir();
        let result = execute_tool("nonexistent_tool", r#"{}"#, dir.path()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown tool"));
    }

    #[tokio::test]
    async fn missing_required_param_returns_error() {
        let dir = setup_temp_dir();
        let result = execute_tool("read_file", r#"{}"#, dir.path()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing required parameter"));
    }
}
