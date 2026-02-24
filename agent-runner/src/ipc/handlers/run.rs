use anyhow::Result;
use protocol::{AgentRunEvent, AgentRunRequest, PROTOCOL_VERSION};
use tokio::io::AsyncWrite;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

use crate::ipc::router::write_json_line;
use crate::runner::Runner;

pub(crate) async fn handle_run_request<W: AsyncWrite + Unpin>(
    req: AgentRunRequest,
    runner: &std::sync::Arc<Mutex<Runner>>,
    event_tx: &mpsc::Sender<AgentRunEvent>,
    writer: &mut W,
    connection_id: u64,
) -> Result<()> {
    if req.protocol_version != PROTOCOL_VERSION {
        warn!(
            connection_id,
            run_id = %req.run_id.0.as_str(),
            expected_protocol = PROTOCOL_VERSION,
            received_protocol = %req.protocol_version,
            "Rejecting run request due to protocol version mismatch"
        );
        let evt = AgentRunEvent::Error {
            run_id: req.run_id,
            error: format!(
                "protocol_version_mismatch: expected {}, got {}",
                PROTOCOL_VERSION, req.protocol_version
            ),
        };
        write_json_line(writer, &evt).await?;
        return Ok(());
    }

    let context_keys = req.context.as_object().map(|obj| obj.len()).unwrap_or(0);
    info!(
        connection_id,
        run_id = %req.run_id.0.as_str(),
        model = %req.model.0.as_str(),
        timeout_secs = ?req.timeout_secs,
        context_keys,
        "Dispatching run request to supervisor"
    );

    runner
        .lock()
        .await
        .handle_run_request(req, event_tx.clone());

    Ok(())
}
