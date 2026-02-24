//! MCP Server Binary - Standalone MCP server with per-agent endpoints

use mcp_server::{HttpServer, McpConfig, McpServer};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, Level};
use tracing_subscriber;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    info!("Starting MCP Server");

    // Get project root from args or use current dir
    let root_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    info!("Project root: {}", root_path.display());

    // Create HTTP server
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let http_server = HttpServer::new(port);
    let registry = http_server.registry();

    // Register example agents
    // Each agent can have different tools/capabilities

    // Agent 1: PM Agent with full search capabilities
    let pm_config = McpConfig::new("pm-agent".to_string(), root_path.clone()).with_port(3001);
    let pm_server = Arc::new(McpServer::new(
        pm_config.name.clone(),
        pm_config.version.clone(),
        pm_config.root_path.clone(),
    ));
    registry.register_agent("pm".to_string(), pm_server).await;

    // Agent 2: EM Agent with full search capabilities
    let em_config = McpConfig::new("em-agent".to_string(), root_path.clone()).with_port(3002);
    let em_server = Arc::new(McpServer::new(
        em_config.name.clone(),
        em_config.version.clone(),
        em_config.root_path.clone(),
    ));
    registry.register_agent("em".to_string(), em_server).await;

    // Agent 3: Code Review Agent
    let review_config = McpConfig::new("review-agent".to_string(), root_path.clone()).with_port(3003);
    let review_server = Arc::new(McpServer::new(
        review_config.name.clone(),
        review_config.version.clone(),
        review_config.root_path.clone(),
    ));
    registry.register_agent("review".to_string(), review_server).await;

    info!("Registered agents: pm, em, review");
    info!("Endpoints:");
    info!("  - http://127.0.0.1:{}/mcp/pm", port);
    info!("  - http://127.0.0.1:{}/mcp/em", port);
    info!("  - http://127.0.0.1:{}/mcp/review", port);
    info!("  - http://127.0.0.1:{}/agents (list all agents)", port);
    info!(
        "  - http://127.0.0.1:{}/agents/<agent_id> (agent details)",
        port
    );

    // Start server
    http_server.start().await?;

    Ok(())
}
