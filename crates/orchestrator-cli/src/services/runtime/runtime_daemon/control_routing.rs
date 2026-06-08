//! CLI-side `DaemonOpsRouting` adapter — bridges the daemon's control
//! surface back to the same `daemon/status`, `daemon/health`, and
//! `daemon/agents` helpers the CLI uses for its in-process code path.
//!
//! See the sibling [`crate::services::operations::ops_plugin::control_routing`]
//! module for the plugin/* equivalent.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use animus_control_protocol::{
    types::{DaemonAgentsResponse, DaemonHealthResponse, DaemonHealthStatus, DaemonStatusResponse, PluginHealth},
    ControlError,
};
use async_trait::async_trait;
use orchestrator_core::DaemonStatus;
use orchestrator_daemon_runtime::control::DaemonOpsRouting;
use protocol::is_process_alive;

use super::{read_daemon_pid, remove_daemon_pid, set_daemon_pid};

/// Build a [`DaemonOpsRouting`] handle bound to `project_root`. The
/// `started_at` clock is captured at daemon startup so the
/// `uptime_seconds` field reports the actual process uptime, not the
/// elapsed-since-first-call value.
pub fn build_daemon_ops_routing(project_root: PathBuf, started_at: SystemTime) -> Arc<dyn DaemonOpsRouting> {
    Arc::new(DaemonOpsRoutingImpl { project_root, started_at })
}

struct DaemonOpsRoutingImpl {
    project_root: PathBuf,
    started_at: SystemTime,
}

impl DaemonOpsRoutingImpl {
    fn project_root_str(&self) -> String {
        self.project_root.to_string_lossy().to_string()
    }
}

#[async_trait]
impl DaemonOpsRouting for DaemonOpsRoutingImpl {
    async fn daemon_status(&self) -> Result<DaemonStatusResponse, ControlError> {
        let project_root_str = self.project_root_str();
        let snapshot = orchestrator_core::load_daemon_status_snapshot_fast(self.project_root.as_path())
            .await
            .map_err(|err| ControlError::Internal(format!("daemon/status: {err:#}")))?;
        let mut status = snapshot.status;
        let runtime_pid = read_daemon_pid(&project_root_str);
        let pid = runtime_pid.or(snapshot.daemon_pid);
        if let Some(active_pid) = pid {
            let alive = match (runtime_pid, snapshot.daemon_pid, snapshot.process_alive) {
                (Some(rt), Some(snap), Some(alive)) if rt == snap => alive,
                _ => is_process_alive(active_pid),
            };
            if !alive && matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
                status = DaemonStatus::Crashed;
                remove_daemon_pid(&project_root_str);
                let _ = set_daemon_pid(&project_root_str, None);
            }
        } else if matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
            status = DaemonStatus::Crashed;
        }
        let running = matches!(status, DaemonStatus::Running | DaemonStatus::Paused);
        let uptime_seconds = self.started_at.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        Ok(DaemonStatusResponse {
            running,
            pid,
            uptime_seconds: Some(uptime_seconds),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            project_root: Some(self.project_root.clone()),
            log_path: None,
        })
    }

    async fn daemon_health(&self) -> Result<DaemonHealthResponse, ControlError> {
        let project_root_str = self.project_root_str();
        let mut snapshot = orchestrator_core::load_daemon_health_snapshot(self.project_root.as_path())
            .await
            .map_err(|err| ControlError::Internal(format!("daemon/health: {err:#}")))?;
        let pid = read_daemon_pid(&project_root_str);
        if let Some(pid) = pid {
            let alive = is_process_alive(pid);
            snapshot.daemon_pid = Some(pid);
            snapshot.process_alive = Some(alive);
            if !alive && matches!(snapshot.status, DaemonStatus::Running | DaemonStatus::Paused) {
                snapshot.status = DaemonStatus::Crashed;
                snapshot.healthy = false;
                remove_daemon_pid(&project_root_str);
                let _ = set_daemon_pid(&project_root_str, None);
            }
        } else if matches!(snapshot.status, DaemonStatus::Running | DaemonStatus::Paused) {
            snapshot.status = DaemonStatus::Crashed;
            snapshot.healthy = false;
        }
        let wire_status = if !snapshot.healthy {
            DaemonHealthStatus::Unhealthy
        } else {
            match snapshot.status {
                DaemonStatus::Running | DaemonStatus::Paused => DaemonHealthStatus::Healthy,
                DaemonStatus::Starting | DaemonStatus::Stopping => DaemonHealthStatus::Degraded,
                DaemonStatus::Stopped => DaemonHealthStatus::Down,
                DaemonStatus::Crashed => DaemonHealthStatus::Unhealthy,
            }
        };
        Ok(DaemonHealthResponse { status: wire_status, plugins: Vec::<PluginHealth>::new(), last_error: None })
    }

    async fn daemon_agents(&self) -> Result<DaemonAgentsResponse, ControlError> {
        // The daemon-side agent registry is still under development
        // (see C7 plan). For now we return the same empty list as the
        // CLI in-process path until the AgentPool exposes a queryable
        // snapshot.
        Ok(DaemonAgentsResponse { agents: Vec::new() })
    }
}
