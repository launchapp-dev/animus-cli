use anyhow::{Context, Result};
use protocol::{
    AgentControlRequest, AgentRunEvent, AgentRunRequest, AgentStatusRequest, ModelStatusRequest,
    RunnerStatusRequest,
};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::handlers;
use crate::runner::Runner;

pub(super) fn truncate_for_log(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{}…", truncated)
}

fn event_kind(event: &AgentRunEvent) -> &'static str {
    match event {
        AgentRunEvent::Started { .. } => "started",
        AgentRunEvent::OutputChunk { .. } => "output_chunk",
        AgentRunEvent::Metadata { .. } => "metadata",
        AgentRunEvent::Error { .. } => "error",
        AgentRunEvent::Finished { .. } => "finished",
        AgentRunEvent::ToolCall { .. } => "tool_call",
        AgentRunEvent::ToolResult { .. } => "tool_result",
        AgentRunEvent::Artifact { .. } => "artifact",
        AgentRunEvent::Thinking { .. } => "thinking",
    }
}

pub(super) async fn write_json_line<W: AsyncWrite + Unpin, T: serde::Serialize>(
    writer: &mut W,
    payload: &T,
) -> Result<()> {
    let json = serde_json::to_string(payload)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

pub(super) async fn handle_connection<S>(
    stream: S,
    runner: Arc<Mutex<Runner>>,
    connection_id: u64,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half).lines();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentRunEvent>(100);
    debug!(connection_id, "Connection event channel initialized");

    loop {
        tokio::select! {
            line = reader.next_line() => {
                let Some(text) = line.context("Failed to read IPC message")? else {
                    info!(connection_id, "Client closed IPC stream");
                    break;
                };
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                debug!(
                    connection_id,
                    payload_bytes = text.len(),
                    payload_preview = %truncate_for_log(text, 240),
                    "Received IPC payload"
                );

                if let Ok(req) = serde_json::from_str::<AgentRunRequest>(text) {
                    handlers::run::handle_run_request(
                        req,
                        &runner,
                        &event_tx,
                        &mut write_half,
                        connection_id,
                    )
                    .await?;
                } else if let Ok(req) = serde_json::from_str::<ModelStatusRequest>(text) {
                    handlers::status::handle_model_status_request(
                        req,
                        &runner,
                        &mut write_half,
                        connection_id,
                    )
                    .await?;
                } else if let Ok(req) = serde_json::from_str::<AgentControlRequest>(text) {
                    handlers::control::handle_control_request(
                        req,
                        &runner,
                        &mut write_half,
                        connection_id,
                    )
                    .await?;
                } else if let Ok(req) = serde_json::from_str::<AgentStatusRequest>(text) {
                    handlers::status::handle_agent_status_request(
                        req,
                        &runner,
                        &mut write_half,
                        connection_id,
                    )
                    .await?;
                } else if let Ok(req) = serde_json::from_str::<RunnerStatusRequest>(text) {
                    handlers::status::handle_runner_status_request(
                        req,
                        &runner,
                        &mut write_half,
                        connection_id,
                    )
                    .await?;
                } else {
                    warn!(
                        connection_id,
                        payload_preview = %truncate_for_log(text, 600),
                        "Unrecognized IPC payload"
                    );
                }
            }
            Some(evt) = event_rx.recv() => {
                debug!(
                    connection_id,
                    event_kind = event_kind(&evt),
                    "Forwarding run event to client"
                );
                write_json_line(&mut write_half, &evt).await?;
            }
        }
    }

    Ok(())
}
