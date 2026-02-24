# MCP Integration for CLI Wrapper

Complete guide to using Model Context Protocol (MCP) with the CLI wrapper testing framework.

## Overview

The CLI wrapper now supports testing AI coding CLIs with **Model Context Protocol (MCP)** capabilities. This allows you to:

- Test CLIs that connect to MCP servers
- Verify tool discovery and execution
- Test search, file operations, and custom tools
- Run end-to-end MCP integration tests

## Architecture

```
┌─────────────────┐         ┌──────────────────┐
│   CLI Wrapper   │         │   MCP Server     │
│   (Test Runner) │ ◄─────► │   (Port 3000)    │
└─────────────────┘         └──────────────────┘
         │                           │
         │                           │
         ▼                           ▼
   ┌──────────┐              ┌─────────────┐
   │ Claude   │              │  Tools:     │
   │ OpenCode │              │  - search   │
   │ (Agents) │              │  - (more)   │
   └──────────┘              └─────────────┘
```

## Quick Start

### 1. Build the MCP Server

```bash
cd cli-wrapper/mcp-server
cargo build --release
```

### 2. Start the MCP Server

```bash
# From repository root
./cli-wrapper/mcp-server/target/release/mcp-server /path/to/project

# Server starts on port 3000 with these endpoints:
# - http://127.0.0.1:3000/mcp/pm
# - http://127.0.0.1:3000/mcp/em
# - http://127.0.0.1:3000/mcp/review
# - http://127.0.0.1:3000/agents
```

### 3. Test CLIs with MCP

```bash
cd cli-wrapper

# Test all CLIs with MCP test suite
cargo run -- test --suite mcp

# Test specific CLI
cargo run -- test claude --suite mcp

# Check which CLIs support MCP
cargo run -- info claude
cargo run -- info opencode
```

## Using the MCP Server Manager

The `McpServerManager` handles MCP server lifecycle in tests:

```rust
use cli_wrapper::McpServerManager;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    // Create manager for project root on port 3001
    let mut manager = McpServerManager::new(
        PathBuf::from("/path/to/project"),
        3001
    );

    // Start server (builds if needed, waits until ready)
    manager.start().await?;

    // Get agent endpoint URL
    let claude_endpoint = manager.get_endpoint("claude");
    println!("Claude MCP endpoint: {}", claude_endpoint);

    // Use with CLI tests...

    // Stop server when done
    manager.stop()?;

    Ok(())
}
```

## MCP Test Suite

The `TestSuite::mcp_integration()` includes:

### 1. Connection Test
Verifies CLI can connect to MCP server and complete handshake.

### 2. Tool Discovery Test
Verifies CLI can list available tools via `tools/list` method.

### 3. Search Tool Test
Verifies CLI can execute the search tool to find files.

### 4. Pattern Search Test
Verifies CLI can search for code patterns using regex.

## MCP Capabilities

CLIs report MCP support in their capabilities:

```rust
pub struct CliCapability {
    // ... other fields
    pub supports_mcp: bool,         // Can connect to MCP servers
    pub mcp_endpoint: Option<String>, // Custom endpoint URL
}
```

Current MCP support status:

| CLI | MCP Support | Notes |
|-----|-------------|-------|
| Claude Code | ✅ | Full MCP support |
| OpenCode | ✅ | Full MCP support |
| Codex | ❌ | No MCP support |
| Gemini | ❌ | No MCP support |
| Aider | ❌ | No MCP support |

## Available MCP Tools

The MCP server provides these tools:

### Search Tool

Search for files and code content with powerful options:

```json
{
  "name": "search",
  "arguments": {
    "query": "CliInterface",
    "file_pattern": "*.rs",
    "regex": false,
    "case_sensitive": true,
    "max_results": 100
  }
}
```

**Features:**
- Regex pattern matching
- Glob pattern file filtering
- Case-sensitive/insensitive search
- Gitignore support (respects `.gitignore`)
- Configurable result limits
- Context-aware line matching

**Example responses:**
```
Found 5 result(s):

📄 cli-wrapper/src/cli/interface.rs
  Line 73: pub trait CliInterface: Send + Sync {
  Line 104: async fn run_process(

📄 cli-wrapper/src/cli/claude.rs
  Line 21: impl CliInterface for ClaudeCli {
```

## Manual MCP Testing

### Using curl

```bash
# Initialize connection
curl -X POST http://127.0.0.1:3000/mcp/pm \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2024-11-05",
      "capabilities": {},
      "clientInfo": {
        "name": "test-client",
        "version": "1.0.0"
      }
    }
  }'

# List tools
curl -X POST http://127.0.0.1:3000/mcp/pm \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/list"
  }'

# Execute search
curl -X POST http://127.0.0.1:3000/mcp/pm \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 3,
    "method": "tools/call",
    "params": {
      "name": "search",
      "arguments": {
        "query": "CliInterface",
        "file_pattern": "*.rs"
      }
    }
  }'
```

## Integration Test Example

See `cli-wrapper/examples/mcp_integration.rs` for a complete example:

```bash
cargo run --example mcp_integration
```

This demonstrates:
- Starting MCP server programmatically
- Discovering CLIs with MCP support
- Running MCP test suite
- Manual MCP requests
- Proper cleanup

## Configuration

### Custom MCP Server Binary

```rust
let manager = McpServerManager::new(root_path, 3000)
    .with_binary(PathBuf::from("/custom/path/mcp-server"));
```

### Custom Port

```rust
let manager = McpServerManager::new(root_path, 8080);
// Server will run on port 8080
```

### Environment Variables

```bash
# Server port (default: 3000)
PORT=8080 mcp-server /path/to/project

# Logging level
RUST_LOG=debug mcp-server /path/to/project
```

## Adding New MCP Tools

To add a new tool to the MCP server:

1. **Create tool module** in `cli-wrapper/mcp-server/src/tools/`:

```rust
// my_tool.rs
pub struct MyTool {
    // ... fields
}

impl MyTool {
    pub fn definition() -> Tool {
        Tool {
            name: "my_tool".to_string(),
            description: "My custom tool".to_string(),
            input_schema: ToolInputSchema {
                // ... schema
            },
        }
    }

    pub async fn execute(&self, params: &CallToolParams) -> Result<CallToolResult> {
        // ... implementation
    }
}
```

2. **Register in ToolRegistry** (`tools/mod.rs`):

```rust
pub use my_tool::MyTool;

impl ToolRegistry {
    pub fn list_tools(&self) -> Vec<Tool> {
        vec![
            SearchTool::definition(),
            MyTool::definition(),  // Add here
        ]
    }

    pub async fn execute_tool(&self, params: &CallToolParams) -> Result<CallToolResult> {
        match params.name.as_str() {
            "search" => self.search_tool.execute(params).await,
            "my_tool" => self.my_tool.execute(params).await,  // Add here
            _ => { /* ... */ }
        }
    }
}
```

3. **Add tests**:

```rust
#[tokio::test]
async fn test_my_tool() {
    let tool = MyTool::new();
    let params = CallToolParams {
        name: "my_tool".to_string(),
        arguments: Some(serde_json::json!({
            "param1": "value1"
        })),
    };

    let result = tool.execute(&params).await.unwrap();
    assert!(!result.is_error.unwrap_or(false));
}
```

## Troubleshooting

### Server won't start

```bash
# Ensure binary is built
cd cli-wrapper/mcp-server
cargo build --release

# Check if port is in use
lsof -i :3000

# Try different port
PORT=3001 mcp-server /path/to/project
```

### Tests failing

```bash
# Run with verbose logging
RUST_LOG=debug cargo test mcp_integration

# Check server logs
tail -f /tmp/mcp-server.log

# Verify server is responding
curl http://127.0.0.1:3000/health
```

### Tool execution errors

1. **Check tool arguments** match the schema
2. **Verify file paths** are relative to server root
3. **Check permissions** on files being accessed
4. **Review server logs** for detailed error messages

## Best Practices

1. **Always start MCP server before tests**
   ```rust
   manager.start().await?;  // Waits until ready
   ```

2. **Use proper cleanup**
   ```rust
   // Manager auto-stops on drop, but explicit is better
   manager.stop()?;
   ```

3. **Test MCP support before using**
   ```rust
   if metadata.capabilities.supports_mcp {
       // Use MCP features
   }
   ```

4. **Handle connection failures gracefully**
   ```rust
   match manager.start().await {
       Ok(_) => { /* proceed */ }
       Err(e) => {
           eprintln!("MCP server unavailable: {}", e);
           // Skip MCP tests or fail gracefully
       }
   }
   ```

## References

- [Model Context Protocol Specification](https://modelcontextprotocol.io/)
- [MCP Server Implementation](mcp-server/)
- [CLI Wrapper README](README.md)
- [Integration Example](examples/mcp_integration.rs)
