use anyhow::{Context, Result};
use orchestrator_core::DaemonStatus;
use orchestrator_daemon_runtime::control::ControlClient;
use protocol::is_process_alive;
use serde_json::{json, Value};
use std::path::Path;

use super::exec_errors::build_inproc_tool_error_payload;
use super::AoMcpServer;
use crate::services::runtime::{
    canonicalize_lossy, daemon_config_application, overlay_budget_health, overlay_daily_cap_status,
    overlay_runtime_pause, read_daemon_pid, remove_daemon_pid, set_daemon_pid, DaemonConfigApplicationRequest,
};
use rmcp::model::CallToolResult;

pub(super) fn resolve_project_root(default_root: &str, override_value: Option<String>) -> String {
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
                // and fleet daily-cap flags so MCP consumers see them too
                // (matches the CLI's JSON path).
                let mut value = serde_json::to_value(response)?;
                overlay_runtime_pause(&mut value, &project_root);
                overlay_daily_cap_status(&mut value, &project_root);
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
                overlay_budget_health(&mut value, &project_root);
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
    let mut value = serde_json::to_value(health)?;
    overlay_budget_health(&mut value, &project_root);
    Ok(value)
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

fn notification_config_from_input(input: &super::DaemonConfigSetInput) -> Result<Option<Value>> {
    let supplied = usize::from(input.notification_config.is_some())
        + usize::from(input.notification_config_json.is_some())
        + usize::from(input.notification_config_file.is_some());
    if supplied > 1 {
        return Err(crate::invalid_input_error(
            "provide only one of notification_config, notification_config_json, or notification_config_file",
        ));
    }
    if supplied > 0 && input.clear_notification_config {
        return Err(crate::invalid_input_error(
            "notification configuration and clear_notification_config cannot be used together",
        ));
    }
    if let Some(value) = input.notification_config.clone() {
        return Ok(Some(value));
    }
    if let Some(raw_json) = input.notification_config_json.as_deref() {
        return Ok(Some(serde_json::from_str(raw_json).context("invalid notification_config_json")?));
    }
    if let Some(path) = input.notification_config_file.as_deref() {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read daemon notification config file at {path}"))?;
        return Ok(Some(
            serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse daemon notification config file at {path}"))?,
        ));
    }
    Ok(None)
}

impl AoMcpServer {
    fn run_daemon_config_application(
        &self,
        tool_name: &str,
        project_root: Option<String>,
        request: DaemonConfigApplicationRequest,
    ) -> CallToolResult {
        self.audit_actor_tool_decision(tool_name, false, "management-only");
        let project_root = resolve_project_root(&self.default_project_root, project_root);
        match daemon_config_application(&project_root, request) {
            Ok(result) => CallToolResult::structured(json!({ "tool": tool_name, "result": result })),
            Err(error) => CallToolResult::structured_error(build_inproc_tool_error_payload(tool_name, &error)),
        }
    }

    pub(super) fn daemon_config_inproc(&self, project_root: Option<String>) -> CallToolResult {
        self.run_daemon_config_application(
            "animus.daemon.config",
            project_root,
            DaemonConfigApplicationRequest::default(),
        )
    }

    pub(super) fn daemon_config_set_inproc(&self, input: super::DaemonConfigSetInput) -> CallToolResult {
        let notification_config = match notification_config_from_input(&input) {
            Ok(value) => value,
            Err(error) => {
                return CallToolResult::structured_error(build_inproc_tool_error_payload(
                    "animus.daemon.config-set",
                    &error,
                ));
            }
        };
        self.run_daemon_config_application(
            "animus.daemon.config-set",
            input.project_root,
            DaemonConfigApplicationRequest {
                pool_size: input.pool_size,
                interval_secs: input.interval_secs,
                max_tasks_per_tick: input.max_tasks_per_tick,
                stale_threshold_hours: input.stale_threshold_hours,
                phase_timeout_secs: input.phase_timeout_secs,
                max_daily_usd: input.max_daily_usd,
                silent_threshold_mins: input.silent_threshold_mins,
                notification_config,
                clear_notification_config: input.clear_notification_config,
            },
        )
    }
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

#[cfg(test)]
mod config_tests {
    use protocol::test_utils::EnvVarGuard;
    use serde_json::{json, Value};

    #[test]
    fn daemon_config_read_and_write_share_the_typed_application_service() {
        let _lock = crate::shared::test_env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temp home");
        let _home = EnvVarGuard::set("HOME", Some(temp.path().to_string_lossy().as_ref()));
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        let server = super::super::new_ao_mcp_server(project_root.to_string_lossy().as_ref());

        let set = server.daemon_config_set_inproc(super::super::DaemonConfigSetInput {
            pool_size: Some(6),
            interval_secs: Some(15),
            max_tasks_per_tick: Some(4),
            stale_threshold_hours: Some(48),
            phase_timeout_secs: Some(300),
            max_daily_usd: Some(25.0),
            silent_threshold_mins: Some(9),
            notification_config: Some(json!({"channels": {"ops": {"enabled": true}}})),
            project_root: None,
            ..Default::default()
        });
        let set_is_error = set.is_error;
        let payload = set.structured_content.expect("set payload");
        assert_ne!(set_is_error, Some(true), "{payload}");
        assert_eq!(payload.pointer("/result/pool_size").and_then(Value::as_u64), Some(6));
        assert_eq!(payload.pointer("/result/max_daily_usd").and_then(Value::as_f64), Some(25.0));
        assert_eq!(
            payload.pointer("/result/notification_config/channels/ops/enabled").and_then(Value::as_bool),
            Some(true)
        );

        let read = server.daemon_config_inproc(None);
        let payload = read.structured_content.expect("read payload");
        assert_eq!(payload.pointer("/result/pool_size").and_then(Value::as_u64), Some(6));
        assert_eq!(payload.pointer("/result/updated").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn daemon_config_rejects_ambiguous_notification_sources_before_writing() {
        let temp = tempfile::tempdir().expect("project root");
        let server = super::super::new_ao_mcp_server(temp.path().to_string_lossy().as_ref());
        let result = server.daemon_config_set_inproc(super::super::DaemonConfigSetInput {
            notification_config: Some(json!({"enabled": true})),
            notification_config_json: Some("{\"enabled\":false}".to_string()),
            project_root: None,
            ..Default::default()
        });

        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("typed conflict error");
        assert_eq!(payload.pointer("/error/code").and_then(Value::as_str), Some("invalid_input"), "{payload}");
    }
}
