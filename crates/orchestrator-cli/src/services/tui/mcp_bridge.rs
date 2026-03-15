use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command as TokioCommand};
use tokio::sync::{oneshot, Mutex};

pub(crate) struct AoCliMcpBridge {
    endpoint: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    server_task: Option<tokio::task::JoinHandle<()>>,
    child: Option<Child>,
}

impl AoCliMcpBridge {
    pub(crate) async fn start(project_root: &str) -> Result<Self> {
        let binary = std::env::current_exe().context("failed to resolve ao binary path")?;
        let mut child = TokioCommand::new(binary)
            .arg("--project-root")
            .arg(project_root)
            .arg("mcp")
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn `ao mcp serve`")?;

        let stdin = child.stdin.take().context("failed to capture stdin for `ao mcp serve`")?;
        let stdout = child.stdout.take().context("failed to capture stdout for `ao mcp serve`")?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while lines.next_line().await.ok().flatten().is_some() {}
            });
        }

        let state = Arc::new(BridgeState::new(stdin, stdout));
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.context("failed to bind MCP bridge listener")?;
        let address = listener.local_addr().context("failed to read MCP bridge address")?;
        let endpoint = format!("http://{address}/mcp/ao");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let app = Router::new().route("/mcp/ao", post(handle_bridge_request)).with_state(state);

        let server_task = tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            });
            let _ = server.await;
        });

        Ok(Self { endpoint, shutdown_tx: Some(shutdown_tx), server_task: Some(server_task), child: Some(child) })
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) async fn stop(mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(server_task) = self.server_task.take() {
            let _ = server_task.await;
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

impl Drop for AoCliMcpBridge {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(server_task) = self.server_task.take() {
            server_task.abort();
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

struct BridgeState {
    io: Mutex<BridgeIo>,
}

impl BridgeState {
    fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self { io: Mutex::new(BridgeIo::new(stdin, stdout)) }
    }

    async fn forward_request(&self, payload: Value) -> Result<Value> {
        let mut io = self.io.lock().await;
        io.forward_request(payload).await
    }
}

struct BridgeIo {
    stdin: ChildStdin,
    stdout_lines: Lines<BufReader<ChildStdout>>,
}

impl BridgeIo {
    fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self { stdin, stdout_lines: BufReader::new(stdout).lines() }
    }

    async fn forward_request(&mut self, payload: Value) -> Result<Value> {
        let serialized = serde_json::to_string(&payload).context("failed to serialize MCP request")?;
        self.stdin.write_all(serialized.as_bytes()).await.context("failed to write request to `ao mcp serve`")?;
        self.stdin.write_all(b"\n").await.context("failed to write newline to `ao mcp serve`")?;
        self.stdin.flush().await.context("failed to flush `ao mcp serve` stdin")?;

        while let Some(line) = self.stdout_lines.next_line().await.context("failed to read `ao mcp serve` output")? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let response = serde_json::from_str::<Value>(trimmed)
                .with_context(|| format!("invalid MCP response from ao mcp serve: {trimmed}"))?;
            return Ok(response);
        }

        Err(anyhow!("`ao mcp serve` closed its stdout"))
    }
}

async fn handle_bridge_request(State(state): State<Arc<BridgeState>>, Json(payload): Json<Value>) -> Response {
    match state.forward_request(payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => {
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32000,
                    "message": error.to_string()
                },
                "id": Value::Null
            });
            (StatusCode::BAD_GATEWAY, Json(payload)).into_response()
        }
    }
}
