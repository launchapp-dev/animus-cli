use super::daemon_inproc::{daemon_agents_inproc, daemon_health_inproc, daemon_status_inproc, wrap_tool_result};
use super::*;
use rmcp::model::CallToolResult;

#[tool_router(router = daemon_tool_router, vis = "pub(super)")]
impl AoMcpServer {
    #[tool(
        name = "animus.daemon.start",
        description = "Start the Animus daemon. Purpose: Launch the background daemon for task scheduling and agent management. Prerequisites: None. Example: {} or {\"interval_secs\": 5}. Sequencing: After starting, use animus.daemon.status or animus.daemon.health to verify it's running.",
        input_schema = ao_schema_for_type::<DaemonStartInput>()
    )]
    async fn ao_daemon_start(&self, params: Parameters<DaemonStartInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_daemon_start_args(&input);
        self.run_tool("animus.daemon.start", args, input.project_root).await
    }

    #[tool(
        name = "animus.daemon.stop",
        description = "Stop the Animus daemon. Purpose: Shutdown the daemon gracefully. Prerequisites: Daemon must be running (check with animus.daemon.status). Example: {}. Sequencing: Use animus.daemon.status first to verify daemon is running, or animus.daemon.agents to see active agents before stopping.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_stop(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.run_tool("animus.daemon.stop", vec!["daemon".to_string(), "stop".to_string()], params.0.project_root).await
    }

    #[tool(
        name = "animus.daemon.status",
        description = "Get daemon status. Purpose: Check if daemon is running and view basic state. Prerequisites: None. Example: {}. Sequencing: Use after animus.daemon.start to verify startup, or before animus.daemon.stop to confirm running.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_status(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        let payload = wrap_tool_result(
            "animus.daemon.status",
            daemon_status_inproc(&self.default_project_root, params.0.project_root).await,
        );
        if super::daemon_inproc::is_tool_error(&payload) {
            Ok(CallToolResult::structured_error(payload))
        } else {
            Ok(CallToolResult::structured(payload))
        }
    }

    #[tool(
        name = "animus.daemon.health",
        description = "Check daemon health. Purpose: Get detailed health metrics including active agents, queue state, and capacity. Prerequisites: Daemon should be running. Example: {}. Sequencing: Use animus.daemon.status first to check if running, then animus.daemon.health for detailed metrics.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_health(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        let payload = wrap_tool_result(
            "animus.daemon.health",
            daemon_health_inproc(&self.default_project_root, params.0.project_root).await,
        );
        if super::daemon_inproc::is_tool_error(&payload) {
            Ok(CallToolResult::structured_error(payload))
        } else {
            Ok(CallToolResult::structured(payload))
        }
    }

    #[tool(
        name = "animus.daemon.pause",
        description = "Pause the daemon scheduler. Purpose: Temporarily stop the daemon from picking up new tasks without stopping it. Prerequisites: Daemon must be running. Example: {}. Sequencing: Use animus.daemon.status first, then animus.daemon.resume to continue scheduling.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_pause(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.run_tool("animus.daemon.pause", vec!["daemon".to_string(), "pause".to_string()], params.0.project_root)
            .await
    }

    #[tool(
        name = "animus.daemon.resume",
        description = "Resume the daemon scheduler. Purpose: Continue task scheduling after a pause. Prerequisites: Daemon must be running and previously paused. Example: {}. Sequencing: Use after animus.daemon.pause, or check status with animus.daemon.status first.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_resume(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        self.run_tool("animus.daemon.resume", vec!["daemon".to_string(), "resume".to_string()], params.0.project_root)
            .await
    }

    #[tool(
        name = "animus.daemon.events",
        description = "List recent daemon events. Purpose: Debug and monitor daemon activity, task scheduling, and agent lifecycle events. Prerequisites: Daemon should be running. Example: {\"limit\": 50}. Sequencing: Use animus.daemon.status first to confirm running, then animus.daemon.agents to see active agents.",
        input_schema = ao_schema_for_type::<DaemonEventsInput>()
    )]
    async fn ao_daemon_events(&self, params: Parameters<DaemonEventsInput>) -> Result<CallToolResult, McpError> {
        match build_daemon_events_poll_result(&self.default_project_root, params.0) {
            Ok(result) => Ok(CallToolResult::structured(json!({
                "tool": "animus.daemon.events",
                "result": result,
            }))),
            Err(error) => Ok(CallToolResult::structured_error(json!({
                "tool": "animus.daemon.events",
                "error": error.to_string(),
            }))),
        }
    }

    #[tool(
        name = "animus.daemon.agents",
        description = "List active daemon agents. Purpose: See currently running agent tasks and their status. Prerequisites: Daemon should be running. Example: {}. Sequencing: Use animus.daemon.status first to confirm running, then animus.agent.status for specific agent details.",
        input_schema = ao_schema_for_type::<ProjectRootInput>()
    )]
    async fn ao_daemon_agents(&self, params: Parameters<ProjectRootInput>) -> Result<CallToolResult, McpError> {
        let payload = wrap_tool_result(
            "animus.daemon.agents",
            daemon_agents_inproc(&self.default_project_root, params.0.project_root).await,
        );
        if super::daemon_inproc::is_tool_error(&payload) {
            Ok(CallToolResult::structured_error(payload))
        } else {
            Ok(CallToolResult::structured(payload))
        }
    }

    #[tool(
        name = "animus.daemon.logs",
        description = "Read daemon log file. Purpose: View daemon process logs for debugging crashes and issues. Prerequisites: Daemon should have been started at least once. Example: {\"limit\": 100} or {\"search\": \"error\"}. Sequencing: Use animus.daemon.status first to check if daemon is running, then animus.daemon.logs to debug issues.",
        input_schema = ao_schema_for_type::<DaemonLogsInput>()
    )]
    async fn ao_daemon_logs(&self, params: Parameters<DaemonLogsInput>) -> Result<CallToolResult, McpError> {
        match build_daemon_logs_result(&self.default_project_root, params.0) {
            Ok(result) => Ok(CallToolResult::structured(json!({
                "tool": "animus.daemon.logs",
                "result": result,
            }))),
            Err(error) => Ok(CallToolResult::structured_error(json!({
                "tool": "animus.daemon.logs",
                "error": error.to_string(),
            }))),
        }
    }

    #[tool(
        name = "animus.daemon.config",
        description = "Read daemon configuration. Purpose: View runtime-reconfigurable daemon settings (pool_size, interval_secs, max_tasks_per_tick, etc). Prerequisites: None. Example: {}. Sequencing: Use animus.daemon.config-set to update values, or animus.daemon.status to check if daemon is running.",
        input_schema = ao_schema_for_type::<DaemonConfigInput>()
    )]
    async fn ao_daemon_config(&self, params: Parameters<DaemonConfigInput>) -> Result<CallToolResult, McpError> {
        Ok(self.daemon_config_inproc(params.0.project_root))
    }

    #[tool(
        name = "animus.daemon.config-set",
        description = "Update daemon configuration through the typed in-process application service. Purpose: Persist daemon automation settings and runtime-reconfigurable settings (pool_size, interval_secs, max_tasks_per_tick, stale_threshold_hours, phase_timeout_secs, max_daily_usd, silent_threshold_mins, notification_config, clear_notification_config). Prefer the structured notification_config object; notification_config_json and notification_config_file remain one-time compatibility inputs and cannot be combined with it. Runtime settings are hot-reloaded by the running daemon without restart. For the fleet daily spend cap prefer animus.budget.set. Prerequisites: None. Example: {\"pool_size\": 4, \"interval_secs\": 10}. Sequencing: Use animus.daemon.config to read current values first.",
        input_schema = ao_schema_for_type::<DaemonConfigSetInput>()
    )]
    async fn ao_daemon_config_set(&self, params: Parameters<DaemonConfigSetInput>) -> Result<CallToolResult, McpError> {
        Ok(self.daemon_config_set_inproc(params.0))
    }

    #[tool(
        name = "animus.daemon.observe",
        description = "Observe daemon activity via the routing front-door. Purpose: Get a merged, chronological window of daemon events + logs (or route to a single surface) for monitoring and debugging. Works offline (reads scoped event/log history; the daemon need not be running). This is the non-streaming view — it always returns and never follows live. Prerequisites: None. Example: {} (data-source matrix + recent tail), {\"since\": \"2h\"} (merged window), {\"source\": \"events\", \"workflow_id\": \"wf-abc123\", \"limit\": 50}. Sequencing: Use animus.daemon.events or animus.daemon.logs for a single surface, or animus.daemon.status for liveness.",
        input_schema = ao_schema_for_type::<DaemonObserveInput>()
    )]
    async fn ao_daemon_observe(&self, params: Parameters<DaemonObserveInput>) -> Result<CallToolResult, McpError> {
        let input = params.0;
        let args = build_daemon_observe_args(&input);
        self.run_tool("animus.daemon.observe", args, input.project_root).await
    }
}
