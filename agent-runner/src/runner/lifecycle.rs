use protocol::RunId;
use std::process::ExitStatus;
use tokio::process::Child;
use tokio::sync::oneshot;
use tracing::warn;

pub(super) fn spawn_wait_task(
    mut child: Child,
    run_id: RunId,
    wait_tx: oneshot::Sender<std::io::Result<ExitStatus>>,
) {
    tokio::spawn(async move {
        let status = child.wait().await;
        if let Err(ref e) = status {
            warn!(
                run_id = %run_id.0.as_str(),
                error = %e,
                "Failed while waiting for CLI process to exit"
            );
        }
        let _ = wait_tx.send(status);
    });
}
