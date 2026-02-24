//! Example: MCP Integration with CLI Wrapper
//!
//! This example demonstrates how to:
//! 1. Start an MCP server
//! 2. Register CLI agents
//! 3. Test CLIs with MCP capabilities
//! 4. Clean up resources
//!
//! Run with: cargo run --example mcp_integration

use cli_wrapper::{CliRegistry, CliTester, CliType, McpServerManager, TestSuite};
use tempfile::TempDir;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("🚀 MCP Integration Example");
    println!("{}", "=".repeat(60));

    // 1. Create temporary workspace
    let temp_dir = TempDir::new()?;
    let workspace_path = temp_dir.path().to_path_buf();

    println!("\n✓ Created workspace: {}", workspace_path.display());

    // 2. Start MCP server
    println!("\n📡 Starting MCP server...");
    let mut mcp_manager = McpServerManager::new(workspace_path.clone(), 3001);

    match mcp_manager.start().await {
        Ok(_) => {
            println!("✓ MCP server started on port 3001");
            println!("  Base URL: {}", mcp_manager.get_base_url());
            println!("  Agents endpoint: {}", mcp_manager.get_agents_endpoint());
        }
        Err(e) => {
            println!("⚠ Failed to start MCP server: {}", e);
            println!("  Note: MCP server binary may need to be built first:");
            println!("  cd cli-wrapper/mcp-server && cargo build --release");
            return Ok(());
        }
    }

    // 3. Discover installed CLIs
    println!("\n🔍 Discovering installed CLIs...");
    let mut registry = CliRegistry::new();
    let count = registry.discover_clis().await?;
    println!("✓ Found {} CLI(s)", count);

    // 4. Test CLIs with MCP support
    println!("\n🧪 Testing CLIs with MCP support...");

    let mcp_clis = vec![CliType::Claude, CliType::OpenCode];

    for cli_type in mcp_clis {
        if let Some(cli_impl) = registry.get(cli_type) {
            let metadata = cli_impl.metadata();

            if metadata.capabilities.supports_mcp {
                println!("\n  Testing {} with MCP...", cli_type.display_name());

                // Show MCP endpoint for this CLI
                let endpoint = mcp_manager.get_endpoint(cli_type.executable_name());
                println!("    MCP endpoint: {}", endpoint);

                // Run basic MCP tests
                let tester = CliTester::new().with_temp_dir(workspace_path.clone());
                let test_suite = TestSuite::mcp_integration();

                match tester.test_cli(cli_impl, &test_suite).await {
                    Ok(results) => {
                        let passed = results.iter().filter(|r| r.passed).count();
                        println!("    Results: {}/{} tests passed", passed, results.len());

                        for result in &results {
                            let status = if result.passed { "✓" } else { "✗" };
                            println!("      {} {}", status, result.test_name);

                            if !result.passed {
                                if let Some(ref error) = result.error {
                                    println!("        Error: {}", error);
                                }
                                for failure in &result.failures {
                                    println!("        Failure: {}", failure);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        println!("    ✗ Testing failed: {}", e);
                    }
                }
            } else {
                println!("  ⊗ {} does not support MCP", cli_type.display_name());
            }
        }
    }

    // 5. Demonstrate manual MCP requests
    println!("\n🔧 Testing manual MCP requests...");

    // Test health endpoint
    let health_url = format!("{}/health", mcp_manager.get_base_url());
    match reqwest::get(&health_url).await {
        Ok(response) if response.status().is_success() => {
            println!("  ✓ Health check passed");
        }
        Ok(response) => {
            println!("  ✗ Health check failed: {}", response.status());
        }
        Err(e) => {
            println!("  ✗ Health check error: {}", e);
        }
    }

    // Test agents list
    let agents_url = mcp_manager.get_agents_endpoint();
    match reqwest::get(&agents_url).await {
        Ok(response) if response.status().is_success() => {
            if let Ok(json) = response.json::<serde_json::Value>().await {
                println!("  ✓ Agents list: {}", json);
            }
        }
        Ok(response) => {
            println!("  ✗ Agents list failed: {}", response.status());
        }
        Err(e) => {
            println!("  ✗ Agents list error: {}", e);
        }
    }

    // 6. Clean up
    println!("\n🧹 Cleaning up...");
    mcp_manager.stop()?;
    println!("✓ MCP server stopped");

    println!("\n{}", "=".repeat(60));
    println!("✅ MCP Integration example completed successfully!");

    Ok(())
}
