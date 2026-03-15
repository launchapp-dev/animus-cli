use anyhow::Result;
use protocol::{AgentControlAction, AgentControlRequest, AgentControlResponse};
use tokio::io::AsyncWrite;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::ipc::router::write_json_line;
use crate::runner::Runner;

pub(crate) async fn handle_control_request<W: AsyncWrite + Unpin>(
    req: AgentControlRequest,
    runner: &std::sync::Arc<Mutex<Runner>>,
    writer: &mut W,
    connection_id: u64,
) -> Result<()> {
    info!(
        connection_id,
        run_id = %req.run_id.0.as_str(),
        action = ?req.action,
        "Handling agent control request"
    );

    let response = match req.action {
        AgentControlAction::Pause | AgentControlAction::Resume => {
            warn!(
                connection_id,
                run_id = %req.run_id.0.as_str(),
                action = ?req.action,
                "Pause/Resume not supported"
            );
            AgentControlResponse {
                run_id: req.run_id,
                success: false,
                message: Some(format!("{:?} is not supported by the agent runner", req.action)),
            }
        }
        AgentControlAction::Terminate => {
            let mut runner_lock = runner.lock().await;
            let success = runner_lock.stop_agent(&req.run_id);
            AgentControlResponse {
                run_id: req.run_id,
                success,
                message: Some(if success {
                    "Agent Terminate successful".to_string()
                } else {
                    "Agent Terminate failed or already stopped".to_string()
                }),
            }
        }
    };

    info!(
        connection_id,
        run_id = %response.run_id.0.as_str(),
        success = response.success,
        "Sending control response"
    );

    write_json_line(writer, &response).await?;
    Ok(())
}
