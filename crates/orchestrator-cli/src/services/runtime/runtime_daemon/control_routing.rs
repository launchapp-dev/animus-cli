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
        // This routing runs inside the daemon process, so the process-global
        // plugin status registry reflects live supervisor state (restart
        // counts, disabled-by-supervisor windows). Fold it into the wire
        // response's per-plugin rows; a supervisor-disabled plugin also
        // degrades the top-level verdict so `animus daemon health` flags it
        // without the operator scanning the plugin table.
        let rows = orchestrator_plugin_host::global_status_registry().map(|r| r.snapshot()).unwrap_or_default();
        let disabled: Vec<&str> =
            rows.iter().filter(|row| row.disabled_by_supervisor).map(|row| row.name.as_str()).collect();
        let (status, last_error) = if wire_status == DaemonHealthStatus::Healthy && !disabled.is_empty() {
            (DaemonHealthStatus::Degraded, Some(format!("plugins disabled by supervisor: {}", disabled.join(", "))))
        } else {
            (wire_status, None)
        };
        let plugins: Vec<PluginHealth> = rows.iter().map(plugin_health_from_runtime_status).collect();
        Ok(DaemonHealthResponse { status, plugins, last_error })
    }

    async fn daemon_agents(&self) -> Result<DaemonAgentsResponse, ControlError> {
        // The daemon-side agent registry is still under development
        // (see C7 plan). For now we return the same empty list as the
        // CLI in-process path until the AgentPool exposes a queryable
        // snapshot.
        Ok(DaemonAgentsResponse { agents: Vec::new() })
    }
}

/// Project a [`PluginRuntimeStatus`] registry row onto the wire's pinned
/// [`PluginHealth`] shape. Supervisor state has no dedicated wire field
/// (`animus-control-protocol` is pinned), so a disabled plugin surfaces as
/// `Unhealthy` with a self-describing `last_error` carrying the restart
/// count and cooldown deadline.
fn plugin_health_from_runtime_status(row: &orchestrator_plugin_host::PluginRuntimeStatus) -> PluginHealth {
    use orchestrator_plugin_host::PluginRuntimeState;

    let (status, supervisor_error) = if row.disabled_by_supervisor {
        let until = row.cooldown_until.map(|t| t.to_rfc3339()).unwrap_or_else(|| "unknown".to_string());
        (
            DaemonHealthStatus::Unhealthy,
            Some(format!("disabled by supervisor after {} restart(s); cooldown until {until}", row.restart_count)),
        )
    } else {
        let status = match row.state {
            PluginRuntimeState::Missing => DaemonHealthStatus::Unhealthy,
            PluginRuntimeState::Restarting | PluginRuntimeState::Stopped => DaemonHealthStatus::Degraded,
            PluginRuntimeState::Discovered | PluginRuntimeState::Running => DaemonHealthStatus::Healthy,
        };
        (status, None)
    };
    PluginHealth {
        name: row.name.clone(),
        kind: row.kind.clone(),
        status,
        uptime_ms: None,
        last_error: supervisor_error.or_else(|| row.last_error.as_ref().map(|err| err.message.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_plugin_host::{PluginRuntimeState, PluginRuntimeStatus};

    fn row(state: PluginRuntimeState) -> PluginRuntimeStatus {
        PluginRuntimeStatus {
            name: "animus-subject-default".into(),
            kind: "task".into(),
            state,
            pid: None,
            last_rpc_at: None,
            last_error: None,
            restart_count: 0,
            binary_path: None,
            manifest_name: None,
            disabled_by_supervisor: false,
            cooldown_until: None,
        }
    }

    #[test]
    fn running_row_maps_to_healthy() {
        let health = plugin_health_from_runtime_status(&row(PluginRuntimeState::Running));
        assert_eq!(health.status, DaemonHealthStatus::Healthy);
        assert!(health.last_error.is_none());
    }

    #[test]
    fn restarting_and_stopped_rows_map_to_degraded() {
        assert_eq!(
            plugin_health_from_runtime_status(&row(PluginRuntimeState::Restarting)).status,
            DaemonHealthStatus::Degraded
        );
        assert_eq!(
            plugin_health_from_runtime_status(&row(PluginRuntimeState::Stopped)).status,
            DaemonHealthStatus::Degraded
        );
    }

    #[test]
    fn missing_row_maps_to_unhealthy() {
        assert_eq!(
            plugin_health_from_runtime_status(&row(PluginRuntimeState::Missing)).status,
            DaemonHealthStatus::Unhealthy
        );
    }

    #[test]
    fn supervisor_disabled_row_maps_to_unhealthy_with_cooldown_message() {
        let mut disabled = row(PluginRuntimeState::Stopped);
        disabled.disabled_by_supervisor = true;
        disabled.restart_count = 4;
        disabled.cooldown_until = Some(chrono::Utc::now() + chrono::Duration::seconds(240));
        let health = plugin_health_from_runtime_status(&disabled);
        assert_eq!(health.status, DaemonHealthStatus::Unhealthy);
        let message = health.last_error.expect("supervisor message");
        assert!(message.contains("disabled by supervisor after 4 restart(s)"), "got: {message}");
        assert!(message.contains("cooldown until"), "got: {message}");
    }
}
