use anyhow::Result;
use protocol::{AgentStatusQueryResponse, AgentStatusRequest, ModelStatusRequest, RunnerStatusRequest};
use tokio::io::AsyncWrite;
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::ipc::router::write_json_line;
use crate::runner::Runner;

pub(crate) async fn handle_model_status_request<W: AsyncWrite + Unpin>(
    req: ModelStatusRequest,
    runner: &std::sync::Arc<Mutex<Runner>>,
    writer: &mut W,
    connection_id: u64,
) -> Result<()> {
    info!(connection_id, model_count = req.models.len(), "Handling model status request");
    let response = runner.lock().await.handle_model_status(req).await;
    debug!(connection_id, statuses_count = response.statuses.len(), "Sending model status response");
    write_json_line(writer, &response).await?;
    Ok(())
}

pub(crate) async fn handle_runner_status_request<W: AsyncWrite + Unpin>(
    _req: RunnerStatusRequest,
    runner: &std::sync::Arc<Mutex<Runner>>,
    writer: &mut W,
    connection_id: u64,
) -> Result<()> {
    info!(connection_id, "Handling runner status request");
    let response = runner.lock().await.handle_runner_status();
    debug!(connection_id, active_agents = response.active_agents, "Sending runner status response");
    write_json_line(writer, &response).await?;
    Ok(())
}

pub(crate) async fn handle_agent_status_request<W: AsyncWrite + Unpin>(
    req: AgentStatusRequest,
    runner: &std::sync::Arc<Mutex<Runner>>,
    writer: &mut W,
    connection_id: u64,
) -> Result<()> {
    info!(
        connection_id,
        run_id = %req.run_id.0.as_str(),
        "Handling agent status request"
    );
    let response = runner.lock().await.handle_agent_status(req);
    match &response {
        AgentStatusQueryResponse::Status(status) => {
            debug!(
                connection_id,
                run_id = %status.run_id.0.as_str(),
                status = ?status.status,
                "Sending agent status response"
            );
        }
        AgentStatusQueryResponse::Error(error) => {
            debug!(
                connection_id,
                run_id = %error.run_id.0.as_str(),
                error_code = ?error.code,
                "Sending agent status error response"
            );
        }
    }
    write_json_line(writer, &response).await?;
    Ok(())
}
