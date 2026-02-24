//! HTTP Transport for MCP Server

use crate::protocol::JsonRpcRequest;
use crate::server::McpServer;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

/// Agent-specific server registry
pub struct AgentRegistry {
    agents: Arc<RwLock<HashMap<String, Arc<McpServer>>>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an agent with its own MCP server
    pub async fn register_agent(&self, agent_id: String, server: Arc<McpServer>) {
        self.agents.write().await.insert(agent_id.clone(), server);
        info!("Registered agent: {}", agent_id);
    }

    /// Unregister an agent
    pub async fn unregister_agent(&self, agent_id: &str) {
        self.agents.write().await.remove(agent_id);
        info!("Unregistered agent: {}", agent_id);
    }

    /// Get server for agent
    pub async fn get_agent(&self, agent_id: &str) -> Option<Arc<McpServer>> {
        self.agents.read().await.get(agent_id).cloned()
    }

    /// List all registered agents
    pub async fn list_agents(&self) -> Vec<String> {
        self.agents.read().await.keys().cloned().collect()
    }
}

/// HTTP server for MCP with per-agent endpoints
pub struct HttpServer {
    registry: Arc<AgentRegistry>,
    port: u16,
}

impl HttpServer {
    pub fn new(port: u16) -> Self {
        Self {
            registry: Arc::new(AgentRegistry::new()),
            port,
        }
    }

    /// Get the agent registry for managing agents
    pub fn registry(&self) -> Arc<AgentRegistry> {
        self.registry.clone()
    }

    /// Start the HTTP server
    pub async fn start(self) -> Result<(), Box<dyn std::error::Error>> {
        let app = Router::new()
            .route("/", get(health_check))
            .route("/health", get(health_check))
            .route("/agents", get(list_agents))
            .route("/agents/:agent_id", get(get_agent))
            .route("/mcp/:agent_id", post(handle_agent_request))
            .layer(
                CorsLayer::new()
                    .allow_origin(Any)
                    .allow_methods(Any)
                    .allow_headers(Any),
            )
            .with_state(self.registry.clone());

        let addr = format!("127.0.0.1:{}", self.port);
        info!("Starting MCP HTTP server on {}", addr);
        info!("Agent endpoints: http://{}/mcp/<agent_id>", addr);

        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "mcp-server"
    }))
}

async fn list_agents(State(registry): State<Arc<AgentRegistry>>) -> impl IntoResponse {
    let agents = registry.list_agents().await;
    Json(serde_json::json!({
        "agents": agents,
        "count": agents.len()
    }))
}

async fn get_agent(
    Path(agent_id): Path<String>,
    State(registry): State<Arc<AgentRegistry>>,
) -> Response {
    match registry.get_agent(&agent_id).await {
        Some(server) => {
            let initialized = server.is_initialized().await;
            Json(serde_json::json!({
                "agent_id": agent_id,
                "registered": true,
                "status": "registered",
                "endpoint": agent_endpoint(&agent_id),
                "initialized": initialized
            }))
            .into_response()
        }
        None => error_response(
            StatusCode::NOT_FOUND,
            &format!("Agent not found: {}", agent_id),
        ),
    }
}

async fn handle_agent_request(
    Path(agent_id): Path<String>,
    State(registry): State<Arc<AgentRegistry>>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    match registry.get_agent(&agent_id).await {
        Some(server) => {
            let response = server.handle_request(request).await;
            Json(response).into_response()
        }
        None => error_response(
            StatusCode::NOT_FOUND,
            &format!("Agent not found: {}", agent_id),
        ),
    }
}

/// Error response helper
#[allow(dead_code)]
fn error_response(code: StatusCode, message: &str) -> Response {
    (
        code,
        Json(serde_json::json!({
            "error": message
        })),
    )
        .into_response()
}

fn agent_endpoint(agent_id: &str) -> String {
    format!("/mcp/{}", agent_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::Value;
    use tempfile::TempDir;
    use tower::ServiceExt;

    async fn read_json(response: Response) -> Value {
        let body = response.into_body();
        let bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .expect("read body");
        serde_json::from_slice::<Value>(&bytes).expect("parse json")
    }

    fn build_test_router(registry: Arc<AgentRegistry>) -> Router {
        Router::new()
            .route("/", get(health_check))
            .route("/health", get(health_check))
            .route("/agents", get(list_agents))
            .route("/agents/:agent_id", get(get_agent))
            .route("/mcp/:agent_id", post(handle_agent_request))
            .with_state(registry)
    }

    #[tokio::test]
    async fn get_agents_includes_count() {
        let registry = Arc::new(AgentRegistry::new());
        let router = build_test_router(registry);

        let response = router
            .oneshot(Request::builder().uri("/agents").body(Body::empty()).unwrap())
            .await
            .expect("request");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json(response).await;
        assert_eq!(payload["count"], 0);
    }

    #[tokio::test]
    async fn get_agent_returns_initialized_status() {
        let registry = Arc::new(AgentRegistry::new());
        let temp = TempDir::new().unwrap();
        let server = Arc::new(McpServer::new(
            "test-agent".to_string(),
            "0.1.0".to_string(),
            temp.path().to_path_buf(),
        ));
        registry.register_agent("agent-1".to_string(), server).await;

        let router = build_test_router(registry);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/agents/agent-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("request");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json(response).await;
        assert_eq!(payload["agent_id"], "agent-1");
        assert_eq!(payload["registered"], true);
        assert_eq!(payload["status"], "registered");
        assert_eq!(payload["initialized"], false);
    }

    #[test]
    fn agent_endpoint_formats_url() {
        assert_eq!(agent_endpoint("pm"), "/mcp/pm");
    }
}
