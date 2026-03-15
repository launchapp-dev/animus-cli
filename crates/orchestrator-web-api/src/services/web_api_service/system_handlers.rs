use orchestrator_core::DaemonStatus;
use serde_json::{json, Value};

use super::{parsing::enum_as_string, WebApiError, WebApiService};

impl WebApiService {
    pub async fn system_info(&self) -> Result<Value, WebApiError> {
        let status = self.context.hub.daemon().status().await?;
        let daemon_running = matches!(
            status,
            DaemonStatus::Starting | DaemonStatus::Running | DaemonStatus::Paused | DaemonStatus::Stopping
        );
        let daemon_status = enum_as_string(&status)?;

        Ok(json!({
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "version": self.context.app_version,
            "daemon_running": daemon_running,
            "daemon_status": daemon_status,
            "project_root": self.context.project_root,
        }))
    }
}
