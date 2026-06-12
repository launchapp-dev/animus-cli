use anyhow::Result;
use orchestrator_core::DaemonStatus;
use orchestrator_daemon_runtime::control::ControlClient;
use protocol::is_process_alive;
use serde_json::{json, Value};
use std::path::Path;

use crate::services::runtime::{
    canonicalize_lossy, overlay_runtime_pause, read_daemon_pid, remove_daemon_pid, set_daemon_pid,
};

fn resolve_project_root(default_root: &str, override_value: Option<String>) -> String {
    let candidate = override_value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_root.to_string());
    canonicalize_lossy(candidate.as_str())
}

pub(super) async fn daemon_status_inproc(default_project_root: &str, project_root: Option<String>) -> Result<Value> {
    let project_root = resolve_project_root(default_project_root, project_root);
    let project_root_path = Path::new(&project_root);

    if let Some(client) = ControlClient::try_connect(project_root_path).await? {
        match client.daemon_status().await {
            Ok(response) => {
                // Wire types are pinned out-of-tree; overlay the pause state
                // so MCP consumers see it too (matches the CLI's JSON path).
                let mut value = serde_json::to_value(response)?;
                overlay_runtime_pause(&mut value, &project_root);
                return Ok(value);
            }
            Err(err) if orchestrator_daemon_runtime::control::is_method_unavailable(&err) => {
                tracing::debug!(error = %err, "daemon/status wire unavailable; falling back to local");
            }
            Err(err) => return Err(err),
        }
    }

    let snapshot = orchestrator_core::load_daemon_status_snapshot_fast(project_root_path).await?;
    let mut status = snapshot.status;
    let runtime_pid = read_daemon_pid(&project_root);
    let pid = runtime_pid.or(snapshot.daemon_pid);
    if let Some(active_pid) = pid {
        let alive = match (runtime_pid, snapshot.daemon_pid, snapshot.process_alive) {
            (Some(rt), Some(snap), Some(alive)) if rt == snap => alive,
            _ => is_process_alive(active_pid),
        };
        if !alive && matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
            status = DaemonStatus::Crashed;
            remove_daemon_pid(&project_root);
            let _ = set_daemon_pid(&project_root, None);
        }
    } else if matches!(status, DaemonStatus::Running | DaemonStatus::Paused) {
        status = DaemonStatus::Crashed;
    }
    Ok(serde_json::to_value(status)?)
}

pub(super) async fn daemon_health_inproc(default_project_root: &str, project_root: Option<String>) -> Result<Value> {
    let project_root = resolve_project_root(default_project_root, project_root);
    let project_root_path = Path::new(&project_root);

    if let Some(client) = ControlClient::try_connect(project_root_path).await? {
        match client.daemon_health().await {
            Ok(response) => {
                let healthy = crate::services::runtime::daemon_health_verdict(response.status, &response.plugins);
                let mut value = serde_json::to_value(response)?;
                if let Some(map) = value.as_object_mut() {
                    map.insert("healthy".to_string(), Value::Bool(healthy));
                }
                overlay_runtime_pause(&mut value, &project_root);
                return Ok(value);
            }
            Err(err) if orchestrator_daemon_runtime::control::is_method_unavailable(&err) => {
                tracing::debug!(error = %err, "daemon/health wire unavailable; falling back to local");
            }
            Err(err) => return Err(err),
        }
    }

    let mut health = orchestrator_core::load_daemon_health_snapshot(project_root_path).await?;
    let pid = read_daemon_pid(&project_root);
    let alive = pid.map(is_process_alive);
    if crate::services::runtime::finalize_offline_health(&mut health, pid, alive) {
        remove_daemon_pid(&project_root);
        let _ = set_daemon_pid(&project_root, None);
    }
    Ok(serde_json::to_value(health)?)
}

pub(super) async fn daemon_agents_inproc(default_project_root: &str, project_root: Option<String>) -> Result<Value> {
    let project_root = resolve_project_root(default_project_root, project_root);
    let project_root_path = Path::new(&project_root);

    if let Some(client) = ControlClient::try_connect(project_root_path).await? {
        match client.daemon_agents().await {
            Ok(response) => return Ok(serde_json::to_value(response)?),
            Err(err) if orchestrator_daemon_runtime::control::is_method_unavailable(&err) => {
                tracing::debug!(error = %err, "daemon/agents wire unavailable; falling back to local");
            }
            Err(err) => return Err(err),
        }
    }

    let health = orchestrator_core::load_daemon_health_snapshot(project_root_path).await?;
    Ok(json!({ "active_agents": health.active_agents }))
}

pub(super) fn wrap_tool_result(tool_name: &str, result: Result<Value>) -> Value {
    match result {
        Ok(data) => json!({ "tool": tool_name, "result": data }),
        Err(err) => json!({ "tool": tool_name, "error": err.to_string() }),
    }
}

pub(super) fn is_tool_error(payload: &Value) -> bool {
    payload.get("error").is_some()
}
